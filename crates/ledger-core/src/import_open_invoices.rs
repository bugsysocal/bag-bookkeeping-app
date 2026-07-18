//! Spec 06 §3/§3.1 — Open Invoices onboarding importer. Template-only, fixed
//! column order: invoice_no, customer, issue_date, due_date, original_total,
//! balance_outstanding. The first row is always a header and is skipped.
//!
//! Every row that survives preview posts through `engine::import_open_invoice`
//! (Dr AR at the outstanding balance / Cr 3000 OPENING_BALANCE_EQUITY, never
//! Revenue or VAT — §3.1's correctness rule), so nothing here re-derives that
//! posting shape; this module is purely parse/resolve/validate/commit-loop.

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
pub struct OpenInvoiceImportRow {
    pub row_num: usize,
    pub invoice_no: String,
    pub customer_name: String,
    pub issue_date: String,
    pub due_date: String,
    pub original_total_kobo: i64,
    pub balance_outstanding_kobo: i64,
    /// `Some` when an existing contact was matched (exactly or fuzzily);
    /// `None` means a new contact stub will be created for `customer_name` at commit.
    pub resolved_contact_id: Option<String>,
    pub status: RowStatus,
    pub message: Option<String>,
}

/// Spec 06 §2 stages 2–4, read-only: parses, resolves the customer name
/// against existing contacts, and runs the §3.1 anti-double-count guard
/// against the wizard's lump opening-AR line for any *existing* contact match
/// (a brand-new stub can't already have one).
pub fn preview_open_invoices(conn: &Connection, company_id: &str, rows: &[Vec<String>]) -> Vec<OpenInvoiceImportRow> {
    rows.iter()
        .enumerate()
        .skip(1) // row 0 is the header
        .map(|(i, row)| {
            let row_num = i + 1;
            let cell = |idx: usize| row.get(idx).map(|s| s.trim().to_string()).unwrap_or_default();
            let invoice_no = cell(0);
            let customer_name = cell(1);
            let issue_raw = cell(2);
            let due_raw = cell(3);
            let total_raw = cell(4);
            let balance_raw = cell(5);

            let blank = |field: &str| {
                error_row(row_num, invoice_no.clone(), customer_name.clone(), format!("{field} is required"))
            };
            if invoice_no.is_empty() {
                return blank("Invoice number");
            }
            if customer_name.is_empty() {
                return blank("Customer");
            }
            if issue_raw.is_empty() {
                return blank("Issue date");
            }
            if due_raw.is_empty() {
                return blank("Due date");
            }

            let issue_date = match crate::csv_util::parse_date_cell(&issue_raw) {
                Some(d) => d,
                None => return error_row(row_num, invoice_no, customer_name, format!("Issue date \"{issue_raw}\" isn't a recognizable date")),
            };
            let due_date = match crate::csv_util::parse_date_cell(&due_raw) {
                Some(d) => d,
                None => return error_row(row_num, invoice_no, customer_name, format!("Due date \"{due_raw}\" isn't a recognizable date")),
            };
            let original_total_kobo = match crate::csv_util::parse_kobo_cell(&total_raw) {
                Some(k) if k > 0 => k,
                _ => return error_row(row_num, invoice_no, customer_name, format!("Original total \"{total_raw}\" must be a positive number")),
            };
            let balance_outstanding_kobo = match crate::csv_util::parse_kobo_cell(&balance_raw) {
                Some(k) if k > 0 && k <= original_total_kobo => k,
                _ => return error_row(
                    row_num, invoice_no, customer_name,
                    "Balance outstanding must be positive and not more than the original total".into(),
                ),
            };

            // Dedup rule (§3): company + invoice no.
            let dup: bool = conn
                .query_row(
                    "SELECT 1 FROM invoices WHERE company_id = ?1 AND number = ?2 LIMIT 1",
                    params![company_id, invoice_no],
                    |_| Ok(()),
                )
                .optional()
                .ok()
                .flatten()
                .is_some();
            if dup {
                return error_row(row_num, invoice_no, customer_name, "This invoice number has already been imported".into());
            }

            let matched = find_contact_match(conn, company_id, &customer_name, "customer").ok().flatten();
            match matched {
                Some((contact_id, exact)) => {
                    let guarded = engine::has_wizard_opening_line(conn, company_id, &contact_id, "AR").unwrap_or(false);
                    if guarded {
                        return error_row(
                            row_num, invoice_no, customer_name,
                            "This customer already has an opening balance recorded during setup — \
                             ask your advisor to remove that opening entry before importing detailed invoices for them"
                                .into(),
                        );
                    }
                    if exact {
                        OpenInvoiceImportRow {
                            row_num, invoice_no, customer_name, issue_date, due_date,
                            original_total_kobo, balance_outstanding_kobo,
                            resolved_contact_id: Some(contact_id),
                            status: RowStatus::Ready, message: None,
                        }
                    } else {
                        OpenInvoiceImportRow {
                            row_num, invoice_no, customer_name: customer_name.clone(), issue_date, due_date,
                            original_total_kobo, balance_outstanding_kobo,
                            resolved_contact_id: Some(contact_id),
                            status: RowStatus::Warning,
                            message: Some(format!("Matched to an existing customer close to \"{customer_name}\" — will use that record")),
                        }
                    }
                }
                None => OpenInvoiceImportRow {
                    row_num, invoice_no, customer_name, issue_date, due_date,
                    original_total_kobo, balance_outstanding_kobo,
                    resolved_contact_id: None,
                    status: RowStatus::Warning,
                    message: Some("No matching customer found — a new customer record will be created".into()),
                },
            }
        })
        .collect()
}

fn error_row(row_num: usize, invoice_no: String, customer_name: String, message: String) -> OpenInvoiceImportRow {
    OpenInvoiceImportRow {
        row_num, invoice_no, customer_name,
        issue_date: String::new(), due_date: String::new(),
        original_total_kobo: 0, balance_outstanding_kobo: 0,
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
    pub exceptions: Vec<OpenInvoiceImportRow>,
}

/// Stage 5 (Commit). `Error` rows never post. `Ready`/`Warning` rows both
/// post — a `Warning` here means "will still import, with a note" (a fuzzy
/// customer match, or a new customer stub), not "skip", unlike the Contacts/
/// Products dedup warning. Each row calls the real posting function
/// (`engine::import_open_invoice`), so a row that somehow still fails there
/// (rather than in this preview) becomes an exception too instead of aborting
/// the whole batch — skip-bad-rows discipline all the way through.
pub fn commit_open_invoices(
    conn: &mut Connection, company_id: &str, filename: &str, rows: Vec<OpenInvoiceImportRow>,
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
            None => match create_customer_stub(conn, company_id, &row.customer_name) {
                Ok(id) => id,
                Err(e) => {
                    row.status = RowStatus::Error;
                    row.message = Some(format!("Could not create the customer record: {e}"));
                    exceptions.push(row);
                    continue;
                }
            },
        };
        match engine::import_open_invoice(
            conn, company_id, &contact_id, &row.invoice_no, &row.issue_date, &row.due_date, &row.issue_date,
            row.original_total_kobo, row.balance_outstanding_kobo, &ctx,
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
         VALUES (?1, ?2, 'open_invoices', ?3, ?4, ?5, ?6, ?7, ?8)",
        params![batch_id, company_id, filename, rows_total as i64, rows_ok as i64, rows_error as i64, created_by, crate::ids::now_iso()],
    )?;
    Ok(ImportBatchResult { batch_id, rows_total, rows_ok, rows_error, exceptions })
}

fn create_customer_stub(conn: &Connection, company_id: &str, name: &str) -> R<String> {
    let id = new_id();
    conn.execute(
        "INSERT INTO contacts (id, company_id, kind, name, created_at) VALUES (?1, ?2, 'customer', ?3, ?4)",
        params![id, company_id, name, crate::ids::now_iso()],
    )?;
    Ok(id)
}
