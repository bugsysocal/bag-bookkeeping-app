//! Spec 06 §3/§3.1 — Open Bills onboarding importer (mirror of Open Invoices
//! on the AP side). Template-only, fixed column order: supplier, reference,
//! bill_date, due_date, original_total, balance_outstanding, wht_applicable
//! (yes/no), wht_rate (percentage, e.g. "5" for 5%). The first row is always
//! a header and is skipped.
//!
//! Deviation from the spec's column list, noted here rather than silently:
//! the §3 table omits an "original total" column for bills (only "balance
//! outstanding*"), but `engine::import_open_bill` needs both to validate a
//! partial balance against the original amount — exactly as invoices do, and
//! as §3.1's own text ("the PDF/original-total field is retained for
//! display") assumes. v1 includes it for bills too.
//!
//! Every row that survives preview posts through `engine::import_open_bill`
//! (Dr 3000 OPENING_BALANCE_EQUITY / Cr AP at the outstanding balance, never
//! Expense or VAT), so nothing here re-derives that posting shape.

use crate::engine::{self, EngineError, PostCtx};
use crate::ids::new_id;
use crate::import_contacts::find_contact_match;
use rusqlite::{params, Connection, OptionalExtension};

type R<T> = Result<T, EngineError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowStatus {
    Ready,
    Warning,
    Error,
}

#[derive(Debug, Clone)]
pub struct OpenBillImportRow {
    pub row_num: usize,
    pub supplier_name: String,
    pub reference: Option<String>,
    pub bill_date: String,
    pub due_date: String,
    pub original_total_kobo: i64,
    pub balance_outstanding_kobo: i64,
    pub wht_applicable: bool,
    pub wht_rate_bp: Option<i64>,
    /// `Some` when an existing contact was matched; `None` means a new
    /// contact stub will be created for `supplier_name` at commit.
    pub resolved_contact_id: Option<String>,
    pub status: RowStatus,
    pub message: Option<String>,
}

pub fn preview_open_bills(conn: &Connection, company_id: &str, rows: &[Vec<String>]) -> Vec<OpenBillImportRow> {
    rows.iter()
        .enumerate()
        .skip(1) // row 0 is the header
        .map(|(i, row)| {
            let row_num = i + 1;
            let cell = |idx: usize| row.get(idx).map(|s| s.trim().to_string()).unwrap_or_default();
            let supplier_name = cell(0);
            let reference = opt(cell(1));
            let bill_raw = cell(2);
            let due_raw = cell(3);
            let total_raw = cell(4);
            let balance_raw = cell(5);
            let wht_flag_raw = cell(6).to_lowercase();
            let wht_rate_raw = cell(7);

            let blank = |field: &str| error_row(row_num, supplier_name.clone(), reference.clone(), format!("{field} is required"));
            if supplier_name.is_empty() {
                return blank("Supplier");
            }
            if bill_raw.is_empty() {
                return blank("Bill date");
            }
            if due_raw.is_empty() {
                return blank("Due date");
            }

            let bill_date = match crate::csv_util::parse_date_cell(&bill_raw) {
                Some(d) => d,
                None => return error_row(row_num, supplier_name, reference, format!("Bill date \"{bill_raw}\" isn't a recognizable date")),
            };
            let due_date = match crate::csv_util::parse_date_cell(&due_raw) {
                Some(d) => d,
                None => return error_row(row_num, supplier_name, reference, format!("Due date \"{due_raw}\" isn't a recognizable date")),
            };
            let original_total_kobo = match crate::csv_util::parse_kobo_cell(&total_raw) {
                Some(k) if k > 0 => k,
                _ => return error_row(row_num, supplier_name, reference, format!("Original total \"{total_raw}\" must be a positive number")),
            };
            let balance_outstanding_kobo = match crate::csv_util::parse_kobo_cell(&balance_raw) {
                Some(k) if k > 0 && k <= original_total_kobo => k,
                _ => return error_row(
                    row_num, supplier_name, reference,
                    "Balance outstanding must be positive and not more than the original total".into(),
                ),
            };
            let wht_applicable = match wht_flag_raw.as_str() {
                "" | "no" | "n" | "false" | "0" => false,
                "yes" | "y" | "true" | "1" => true,
                other => return error_row(row_num, supplier_name, reference, format!("\"{other}\" isn't yes or no for WHT")),
            };
            let wht_rate_bp = if !wht_applicable {
                None
            } else if wht_rate_raw.is_empty() {
                return error_row(row_num, supplier_name, reference, "WHT rate is required when WHT applies".into());
            } else {
                match crate::csv_util::parse_kobo_cell(&wht_rate_raw) {
                    Some(pct) => Some(pct), // parse_kobo_cell already scales by 100 (percent -> bp)
                    None => return error_row(row_num, supplier_name, reference, format!("WHT rate \"{wht_rate_raw}\" is not a number")),
                }
            };

            // Dedup rule (§3): supplier + ref + date — only meaningful when a
            // reference was actually given (a blank ref has no natural key).
            if let Some(ref_val) = &reference {
                let dup: bool = conn
                    .query_row(
                        "SELECT 1 FROM bills b JOIN contacts c ON c.id = b.contact_id
                         WHERE b.company_id = ?1 AND b.reference = ?2 AND b.bill_date = ?3
                           AND lower(trim(c.name)) = lower(trim(?4)) LIMIT 1",
                        params![company_id, ref_val, bill_date, supplier_name],
                        |_| Ok(()),
                    )
                    .optional()
                    .ok()
                    .flatten()
                    .is_some();
                if dup {
                    return error_row(row_num, supplier_name, reference, "This supplier reference has already been imported for this date".into());
                }
            }

            let matched = find_contact_match(conn, company_id, &supplier_name, "supplier").ok().flatten();
            match matched {
                Some((contact_id, exact)) => {
                    let guarded = engine::has_wizard_opening_line(conn, company_id, &contact_id, "AP").unwrap_or(false);
                    if guarded {
                        return error_row(
                            row_num, supplier_name, reference,
                            "This supplier already has an opening balance recorded during setup — \
                             ask your advisor to remove that opening entry before importing detailed bills for them"
                                .into(),
                        );
                    }
                    if exact {
                        OpenBillImportRow {
                            row_num, supplier_name, reference, bill_date, due_date,
                            original_total_kobo, balance_outstanding_kobo, wht_applicable, wht_rate_bp,
                            resolved_contact_id: Some(contact_id),
                            status: RowStatus::Ready, message: None,
                        }
                    } else {
                        OpenBillImportRow {
                            row_num, supplier_name: supplier_name.clone(), reference, bill_date, due_date,
                            original_total_kobo, balance_outstanding_kobo, wht_applicable, wht_rate_bp,
                            resolved_contact_id: Some(contact_id),
                            status: RowStatus::Warning,
                            message: Some(format!("Matched to an existing supplier close to \"{supplier_name}\" — will use that record")),
                        }
                    }
                }
                None => OpenBillImportRow {
                    row_num, supplier_name, reference, bill_date, due_date,
                    original_total_kobo, balance_outstanding_kobo, wht_applicable, wht_rate_bp,
                    resolved_contact_id: None,
                    status: RowStatus::Warning,
                    message: Some("No matching supplier found — a new supplier record will be created".into()),
                },
            }
        })
        .collect()
}

fn opt(s: String) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn error_row(row_num: usize, supplier_name: String, reference: Option<String>, message: String) -> OpenBillImportRow {
    OpenBillImportRow {
        row_num, supplier_name, reference,
        bill_date: String::new(), due_date: String::new(),
        original_total_kobo: 0, balance_outstanding_kobo: 0,
        wht_applicable: false, wht_rate_bp: None,
        resolved_contact_id: None,
        status: RowStatus::Error,
        message: Some(message),
    }
}

pub struct ImportBatchResult {
    pub batch_id: String,
    pub rows_total: usize,
    pub rows_ok: usize,
    pub rows_error: usize,
    pub exceptions: Vec<OpenBillImportRow>,
}

/// Stage 5 (Commit) — same skip-bad-rows discipline as Open Invoices:
/// `Error` never posts; `Ready`/`Warning` both post (a fuzzy supplier match
/// or a new supplier stub is a note, not a skip).
pub fn commit_open_bills(
    conn: &mut Connection, company_id: &str, filename: &str, rows: Vec<OpenBillImportRow>,
    created_by: Option<&str>,
) -> R<ImportBatchResult> {
    let rows_total = rows.len();
    let mut rows_ok = 0usize;
    let mut exceptions = Vec::new();
    let ctx = PostCtx { user_id: created_by.map(String::from), confirm_soft_close: true };

    for mut row in rows {
        if row.status == RowStatus::Error {
            exceptions.push(row);
            continue;
        }
        let contact_id = match &row.resolved_contact_id {
            Some(id) => id.clone(),
            None => match create_supplier_stub(conn, company_id, &row.supplier_name) {
                Ok(id) => id,
                Err(e) => {
                    row.status = RowStatus::Error;
                    row.message = Some(format!("Could not create the supplier record: {e}"));
                    exceptions.push(row);
                    continue;
                }
            },
        };
        match engine::import_open_bill(
            conn, company_id, &contact_id, row.reference.as_deref(), &row.bill_date, &row.due_date, &row.bill_date,
            row.original_total_kobo, row.balance_outstanding_kobo, row.wht_applicable, row.wht_rate_bp, &ctx,
        ) {
            Ok(_) => rows_ok += 1,
            Err(e) => {
                row.status = RowStatus::Error;
                row.message = Some(format!("Could not be recorded: {e}"));
                exceptions.push(row);
            }
        }
    }

    let batch_id = new_id();
    let rows_error = rows_total - rows_ok;
    conn.execute(
        "INSERT INTO import_batches (id, company_id, kind, filename, rows_total, rows_ok, rows_error, created_by, created_at)
         VALUES (?1, ?2, 'open_bills', ?3, ?4, ?5, ?6, ?7, ?8)",
        params![batch_id, company_id, filename, rows_total as i64, rows_ok as i64, rows_error as i64, created_by, crate::ids::now_iso()],
    )?;
    Ok(ImportBatchResult { batch_id, rows_total, rows_ok, rows_error, exceptions })
}

fn create_supplier_stub(conn: &Connection, company_id: &str, name: &str) -> R<String> {
    let id = new_id();
    conn.execute(
        "INSERT INTO contacts (id, company_id, kind, name, created_at) VALUES (?1, ?2, 'supplier', ?3, ?4)",
        params![id, company_id, name, crate::ids::now_iso()],
    )?;
    Ok(id)
}
