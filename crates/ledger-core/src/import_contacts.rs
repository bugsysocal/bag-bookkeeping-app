//! Spec 06 §3 — Contacts onboarding importer. v1 is template-only (no
//! arbitrary-column mapping yet — Spec 06 §2 stage 2 for a non-template file
//! is not built): the fixed column order is name, kind, phone, email, tin,
//! address, terms_days. The first row is always a header and is skipped.

use crate::engine::EngineError;
use crate::ids::{new_id, now_iso};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

type R<T> = Result<T, EngineError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowStatus {
    Ready,
    Warning,
    Error,
}

#[derive(Debug, Clone)]
pub struct ContactImportRow {
    pub row_num: usize,
    pub name: String,
    pub kind: String,
    pub phone: Option<String>,
    pub email: Option<String>,
    pub tin: Option<String>,
    pub address: Option<String>,
    pub terms_days: i64,
    pub status: RowStatus,
    pub message: Option<String>,
}

/// Spec 06 §2 stages 2–4 (map/validate/preview), collapsed into one read-only
/// pass — the template's fixed column order IS the mapping in v1. Writes
/// nothing; `dup` lookups only read existing `contacts`.
pub fn preview_contacts(conn: &Connection, company_id: &str, rows: &[Vec<String>]) -> Vec<ContactImportRow> {
    rows.iter()
        .enumerate()
        .skip(1) // row 0 is the header
        .map(|(i, row)| {
            let row_num = i + 1;
            let cell = |idx: usize| row.get(idx).map(|s| s.trim().to_string()).unwrap_or_default();
            let name = cell(0);
            let kind_raw = cell(1).to_lowercase();
            let phone = opt(cell(2));
            let email = opt(cell(3));
            let tin = opt(cell(4));
            let address = opt(cell(5));
            let terms_raw = cell(6);

            if name.is_empty() {
                return error_row(row_num, name, kind_raw, phone, email, tin, address, "Name is required".into());
            }
            let kind = match kind_raw.as_str() {
                "customer" | "supplier" | "both" => kind_raw.clone(),
                "" => {
                    return error_row(
                        row_num, name, kind_raw, phone, email, tin, address,
                        "Kind is required (customer, supplier, or both)".into(),
                    )
                }
                other => {
                    return error_row(
                        row_num, name, other.into(), phone, email, tin, address,
                        format!("Unrecognized kind \"{other}\" — must be customer, supplier, or both"),
                    )
                }
            };
            let terms_days: i64 = if terms_raw.is_empty() {
                0
            } else {
                match terms_raw.parse() {
                    Ok(n) => n,
                    Err(_) => {
                        return error_row(
                            row_num, name, kind, phone, email, tin, address,
                            format!("Terms days \"{terms_raw}\" is not a number"),
                        )
                    }
                }
            };

            // Dedup rule (§3): name + phone match against an existing contact
            // warns and is skipped on commit — the guard that lets a fixed
            // exceptions file be re-imported without duplicating good rows.
            let dup = conn
                .query_row(
                    "SELECT 1 FROM contacts WHERE company_id = ?1 AND lower(trim(name)) = lower(trim(?2))
                     AND ifnull(lower(trim(phone)), '') = ifnull(lower(trim(?3)), '') LIMIT 1",
                    params![company_id, name, phone],
                    |_| Ok(()),
                )
                .optional()
                .ok()
                .flatten()
                .is_some();

            if dup {
                ContactImportRow {
                    row_num, name, kind, phone, email, tin, address, terms_days,
                    status: RowStatus::Warning,
                    message: Some("A contact with this name and phone already exists — will be skipped".into()),
                }
            } else {
                ContactImportRow {
                    row_num, name, kind, phone, email, tin, address, terms_days,
                    status: RowStatus::Ready,
                    message: None,
                }
            }
        })
        .collect()
}

/// Shared by the open-invoice/open-bill importers (Spec 06 §3): "Customer/
/// supplier names resolve against existing contacts (exact, then case/space-
/// insensitive with ⚠ confirm); unknown names auto-create contact stubs (⚠)."
/// Read-only — `None` means no match at all, and the caller creates the stub
/// at commit time, not here. `Some((id, true))` is an exact match (no note
/// needed); `Some((id, false))` matched only case/space-insensitively.
pub fn find_contact_match(
    conn: &Connection, company_id: &str, name: &str, kind: &str,
) -> R<Option<(String, bool)>> {
    let hit: Option<(String, String)> = conn
        .query_row(
            "SELECT id, name FROM contacts WHERE company_id = ?1 AND is_active = 1
             AND (kind = ?2 OR kind = 'both') AND lower(trim(name)) = lower(trim(?3))
             ORDER BY (trim(name) = trim(?3)) DESC LIMIT 1",
            params![company_id, kind, name],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(hit.map(|(id, matched_name)| (id, matched_name.trim() == name.trim())))
}

fn opt(s: String) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

#[allow(clippy::too_many_arguments)]
fn error_row(
    row_num: usize, name: String, kind: String, phone: Option<String>, email: Option<String>,
    tin: Option<String>, address: Option<String>, message: String,
) -> ContactImportRow {
    ContactImportRow {
        row_num, name, kind, phone, email, tin, address, terms_days: 0,
        status: RowStatus::Error,
        message: Some(message),
    }
}

pub struct ImportBatchResult {
    pub batch_id: String,
    pub rows_total: usize,
    pub rows_ok: usize,
    pub rows_error: usize,
    pub exceptions: Vec<ContactImportRow>,
}

/// Stage 5 (Commit): writes exactly the `Ready` rows in one transaction
/// (skip-bad-rows discipline, Spec 06 §2 Decision #1) plus an `import_batches`
/// row; `Warning`/`Error` rows come back as `exceptions` for the caller to
/// turn into a fix-and-reimport file. Re-validates nothing — the rows passed
/// in are assumed to be exactly what `preview_contacts` produced.
pub fn commit_contacts(
    conn: &mut Connection, company_id: &str, filename: &str, rows: Vec<ContactImportRow>,
    created_by: Option<&str>,
) -> R<ImportBatchResult> {
    let rows_total = rows.len();
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut rows_ok = 0usize;
    let mut exceptions = Vec::new();
    for row in rows {
        if row.status != RowStatus::Ready {
            exceptions.push(row);
            continue;
        }
        tx.execute(
            "INSERT INTO contacts (id, company_id, kind, name, phone, email, tin, address,
                                    payment_terms_days, is_active, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 1, ?10)",
            params![
                new_id(), company_id, row.kind, row.name, row.phone, row.email, row.tin,
                row.address, row.terms_days, now_iso()
            ],
        )?;
        rows_ok += 1;
    }
    let batch_id = new_id();
    let rows_error = rows_total - rows_ok;
    tx.execute(
        "INSERT INTO import_batches (id, company_id, kind, filename, rows_total, rows_ok, rows_error, created_by, created_at)
         VALUES (?1, ?2, 'contacts', ?3, ?4, ?5, ?6, ?7, ?8)",
        params![batch_id, company_id, filename, rows_total as i64, rows_ok as i64, rows_error as i64, created_by, now_iso()],
    )?;
    tx.commit()?;
    Ok(ImportBatchResult { batch_id, rows_total, rows_ok, rows_error, exceptions })
}
