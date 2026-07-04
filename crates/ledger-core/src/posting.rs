//! The posting protocol (Spec 01 §4): within ONE transaction, insert the entry
//! unposted, insert its lines, then flip `is_posted = 1` — which fires the
//! balance/hard-close triggers. Any RAISE(ABORT) rolls the whole event back.
//! An entry commits balanced or it does not commit.
//!
//! `post_entry_in` is the shared plumbing every Spec 01 §6 template function
//! (engine.rs) drives inside its own transaction; `post_entry` is the
//! standalone wrapper for callers without one.

use crate::ids::{new_id, now_iso};
use rusqlite::{params, Connection, TransactionBehavior};

#[derive(Debug, Clone)]
pub struct LineSpec {
    pub account_id: String,
    /// Signed kobo: positive = debit, negative = credit (Spec 01 §2). Never zero.
    pub amount_kobo: i64,
    /// Required on AR/AP lines (P8) — template functions enforce; plumbing carries it.
    pub contact_id: Option<String>,
    pub memo: Option<String>,
    /// FX metadata for foreign-currency bank lines (Spec 01 §3.6).
    pub fx_currency: Option<String>,
    pub fx_amount_kobo: Option<i64>,
}

impl LineSpec {
    pub fn new(account_id: &str, amount_kobo: i64) -> Self {
        Self {
            account_id: account_id.to_string(),
            amount_kobo,
            contact_id: None,
            memo: None,
            fx_currency: None,
            fx_amount_kobo: None,
        }
    }

    pub fn with_contact(account_id: &str, amount_kobo: i64, contact_id: &str) -> Self {
        let mut l = Self::new(account_id, amount_kobo);
        l.contact_id = Some(contact_id.to_string());
        l
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PostError {
    /// P3 validation, caught before SQL. The DB triggers remain the backstop.
    #[error("validation: {0}")]
    Validation(String),
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}

/// Post a balanced journal entry inside the caller's open transaction
/// (`Transaction` derefs to `Connection`). Returns the new entry id.
/// The caller owns commit/rollback — document rows and the entry land together (P1).
pub fn post_entry_in(
    conn: &Connection,
    company_id: &str,
    entry_date: &str,
    memo: &str,
    source_type: &str,
    source_id: Option<&str>,
    created_by: Option<&str>,
    lines: &[LineSpec],
) -> Result<String, PostError> {
    // P3: validate before SQL (friendly errors); triggers are the backstop, not the front line.
    if lines.len() < 2 {
        return Err(PostError::Validation(
            "a journal entry needs at least two lines".into(),
        ));
    }
    if lines.iter().any(|l| l.amount_kobo == 0) {
        return Err(PostError::Validation("journal lines cannot be zero".into()));
    }
    let sum: i64 = lines.iter().map(|l| l.amount_kobo).sum();
    if sum != 0 {
        return Err(PostError::Validation(format!(
            "journal entry does not balance (off by {sum} kobo)"
        )));
    }
    if memo.trim().is_empty() {
        return Err(PostError::Validation(
            "every entry gets a human-readable memo (P5)".into(),
        ));
    }

    let entry_id = new_id();
    let now = now_iso();

    conn.execute(
        "INSERT INTO journal_entries
           (id, company_id, entry_date, memo, source_type, source_id, is_posted, created_by, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, ?7, ?8)",
        params![entry_id, company_id, entry_date, memo, source_type, source_id, created_by, now],
    )?;

    for (i, line) in lines.iter().enumerate() {
        conn.execute(
            "INSERT INTO journal_lines
               (id, entry_id, line_no, account_id, amount_kobo, contact_id, memo, fx_currency, fx_amount_kobo)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                new_id(),
                entry_id,
                (i + 1) as i64,
                line.account_id,
                line.amount_kobo,
                line.contact_id,
                line.memo,
                line.fx_currency,
                line.fx_amount_kobo
            ],
        )?;
    }

    // The flip that fires T1 (balance, >= 2 lines) and T5 (hard close).
    conn.execute(
        "UPDATE journal_entries SET is_posted = 1, posted_at = ?2 WHERE id = ?1",
        params![entry_id, now],
    )?;

    Ok(entry_id)
}

/// Standalone posting: opens and commits its own transaction (P1).
pub fn post_entry(
    conn: &mut Connection,
    company_id: &str,
    entry_date: &str,
    memo: &str,
    source_type: &str,
    source_id: Option<&str>,
    created_by: Option<&str>,
    lines: &[LineSpec],
) -> Result<String, PostError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let id = post_entry_in(&tx, company_id, entry_date, memo, source_type, source_id, created_by, lines)?;
    tx.commit()?;
    Ok(id)
}
