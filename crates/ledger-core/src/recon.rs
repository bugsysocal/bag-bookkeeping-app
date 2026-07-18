//! Bank statement import + reconciliation (Spec 04 §6–7).
//!
//! The needs-review state is the no-Suspense mechanism: an unclear line is a
//! WORKFLOW problem, not a ledger problem — nothing posts while a line is
//! flagged. The quarantine lives on the statement line, visibly, and carries
//! forward across sessions so it cannot age out of sight.

use crate::csv_util::split_csv_line;
use crate::engine::{EngineError, PostCtx};
use crate::ids::{new_id, now_iso};
use crate::posting::{post_entry_in, LineSpec};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use sha2::{Digest, Sha256};

type R<T> = Result<T, EngineError>;

// ===== CSV parsing (Spec 04 §6.1–2) =====

#[derive(Debug, Clone)]
pub struct CsvMapping {
    pub header_rows: usize,
    pub date_col: usize,
    pub desc_col: usize,
    /// Some(col) = single signed amount column; None = separate debit/credit columns.
    pub amount_col: Option<usize>,
    pub debit_col: Option<usize>,
    pub credit_col: Option<usize>,
    /// "DMY" | "YMD" | "MDY"
    pub date_format: String,
    /// Flip the sign of the single amount column (banks disagree on conventions).
    pub flip_sign: bool,
}

#[derive(Debug, Clone)]
pub struct StmtRow {
    pub date: String, // ISO
    pub description: String,
    pub amount_kobo: i64, // signed: + money into the account
}

/// Minimal quoted-field CSV splitter — Nigerian bank exports are simple, but
/// descriptions do contain commas inside quotes.
fn parse_date(raw: &str, fmt: &str) -> Option<String> {
    let seps: &[char] = &['/', '-', '.', ' '];
    let parts: Vec<&str> = raw.trim().split(seps).filter(|p| !p.is_empty()).collect();
    if parts.len() < 3 { return None; }
    let (d, m, y) = match fmt {
        "YMD" => (parts[2], parts[1], parts[0]),
        "MDY" => (parts[1], parts[0], parts[2]),
        _ => (parts[0], parts[1], parts[2]), // DMY — the Nigerian default
    };
    let (d, m, mut y): (u32, u32, i64) = (d.parse().ok()?, m.parse().ok()?, y.parse().ok()?);
    if y < 100 { y += 2000; }
    if !(1..=31).contains(&d) || !(1..=12).contains(&m) { return None; }
    Some(format!("{y:04}-{m:02}-{d:02}"))
}

fn parse_amount(raw: &str) -> Option<i64> {
    let s: String = raw.chars().filter(|c| c.is_ascii_digit() || *c == '.' || *c == '-').collect();
    if s.is_empty() || s == "-" { return None; }
    let neg = s.starts_with('-');
    let s = s.trim_start_matches('-');
    let (whole, frac) = match s.split_once('.') {
        Some((w, f)) => (w, format!("{:0<2}", f.chars().take(2).collect::<String>())),
        None => (s, "00".to_string()),
    };
    let v: i64 = format!("{}{}", if whole.is_empty() { "0" } else { whole }, frac).parse().ok()?;
    Some(if neg { -v } else { v })
}

/// Parse raw CSV text against a mapping. Returns good rows + per-row errors
/// (skip-bad-rows discipline, Spec 06 §2 stage 5 mirrored).
pub fn parse_csv(text: &str, map: &CsvMapping) -> (Vec<StmtRow>, Vec<String>) {
    let mut rows = Vec::new();
    let mut errors = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if i < map.header_rows || line.trim().is_empty() { continue; }
        let cells = split_csv_line(line);
        let cell = |c: usize| cells.get(c).map(|s| s.as_str()).unwrap_or("");
        let date = match parse_date(cell(map.date_col), &map.date_format) {
            Some(d) => d,
            None => { errors.push(format!("Row {}: couldn't read the date '{}'", i + 1, cell(map.date_col))); continue; }
        };
        let amount = if let Some(ac) = map.amount_col {
            match parse_amount(cell(ac)) {
                Some(a) => if map.flip_sign { -a } else { a },
                None => { errors.push(format!("Row {}: couldn't read the amount '{}'", i + 1, cell(ac))); continue; }
            }
        } else {
            let dr = map.debit_col.and_then(|c| parse_amount(cell(c))).unwrap_or(0);
            let cr = map.credit_col.and_then(|c| parse_amount(cell(c))).unwrap_or(0);
            if dr == 0 && cr == 0 {
                errors.push(format!("Row {}: no amount in either column", i + 1));
                continue;
            }
            cr - dr // credit = money in (+), debit = money out (−), bank's perspective
        };
        if amount == 0 { errors.push(format!("Row {}: zero amount", i + 1)); continue; }
        rows.push(StmtRow { date, description: cell(map.desc_col).to_string(), amount_kobo: amount });
    }
    (rows, errors)
}

// ===== Sessions (Spec 04 §7.1) =====

/// Start a session; carries forward unresolved needs-review lines from earlier
/// sessions on this account (Spec 04 §7.5 — they cannot age out of sight).
pub fn start_reconciliation(
    conn: &mut Connection,
    company_id: &str,
    bank_account_id: &str,
    statement_date: &str,
    statement_balance_kobo: i64,
) -> R<String> {
    let open: Option<String> = conn.query_row(
        "SELECT id FROM reconciliations WHERE bank_account_id = ?1 AND status = 'in_progress'",
        params![bank_account_id], |r| r.get(0),
    ).optional()?;
    if open.is_some() {
        return Err(EngineError::Validation("finish the reconciliation already in progress first".into()));
    }
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let id = new_id();
    tx.execute(
        "INSERT INTO reconciliations (id, company_id, bank_account_id, statement_date,
                                      statement_balance_kobo, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![id, company_id, bank_account_id, statement_date, statement_balance_kobo, now_iso()],
    )?;
    // Carry forward: needs_review lines from this account's prior sessions not yet carried.
    let carried: Vec<(String, String, Option<String>, i64, String, Option<String>, Option<String>, Option<String>)> = {
        let mut q = tx.prepare(
            "SELECT l.id, l.stmt_date, l.stmt_description, l.stmt_amount_kobo, l.import_hash,
                    l.review_note, l.flagged_by, l.flagged_at
             FROM reconciliation_lines l
             JOIN reconciliations r ON r.id = l.reconciliation_id
             WHERE r.bank_account_id = ?1 AND l.state = 'needs_review'
               AND NOT EXISTS (SELECT 1 FROM reconciliation_lines c WHERE c.carried_from_id = l.id)",
        )?;
        let it = q.query_map(params![bank_account_id], |r| Ok((
            r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?,
        )))?;
        it.collect::<Result<_, _>>()?
    };
    for (old_id, d, desc, amt, hash, note, fby, fat) in carried {
        tx.execute(
            "INSERT INTO reconciliation_lines
               (id, reconciliation_id, stmt_date, stmt_description, stmt_amount_kobo,
                state, import_hash, review_note, flagged_by, flagged_at, carried_from_id)
             VALUES (?1, ?2, ?3, ?4, ?5, 'needs_review', ?6, ?7, ?8, ?9, ?10)",
            params![new_id(), id, d, desc, amt, hash, note, fby, fat, old_id],
        )?;
    }
    tx.commit()?;
    Ok(id)
}

/// Import parsed rows: content-hash dedup (skip-and-report), then auto-match.
pub fn import_rows(conn: &mut Connection, recon_id: &str, rows: &[StmtRow]) -> R<(usize, usize)> {
    let bank_account_id: String = conn.query_row(
        "SELECT bank_account_id FROM reconciliations WHERE id = ?1 AND status = 'in_progress'",
        params![recon_id], |r| r.get(0),
    ).optional()?
     .ok_or_else(|| EngineError::Validation("this reconciliation is not open".into()))?;

    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut seen: std::collections::HashMap<String, u32> = Default::default();
    let (mut imported, mut skipped) = (0usize, 0usize);
    for row in rows {
        // occurrence_index disambiguates genuinely identical same-day lines (Spec 04 §6.3):
        // count within this batch plus already-stored originals with the same base key.
        let base = format!("{bank_account_id}|{}|{}|{}", row.date, row.amount_kobo, row.description);
        let batch_n = seen.entry(base.clone()).or_insert(0);
        let stored_n: u32 = {
            let mut n = 0u32;
            loop {
                let h = hash_line(&base, n);
                let exists: i64 = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM reconciliation_lines WHERE import_hash = ?1 AND carried_from_id IS NULL)",
                    params![h], |r| r.get(0),
                )?;
                if exists == 0 { break n; }
                n += 1;
            }
        };
        if *batch_n < stored_n {
            // this occurrence already exists from a prior overlapping import
            *batch_n += 1;
            skipped += 1;
            continue;
        }
        let hash = hash_line(&base, *batch_n);
        *batch_n += 1;
        tx.execute(
            "INSERT INTO reconciliation_lines
               (id, reconciliation_id, stmt_date, stmt_description, stmt_amount_kobo, state, import_hash)
             VALUES (?1, ?2, ?3, ?4, ?5, 'unmatched', ?6)",
            params![new_id(), recon_id, row.date, row.description, row.amount_kobo, hash],
        )?;
        imported += 1;
    }
    tx.commit()?;
    auto_match(conn, recon_id)?;
    Ok((imported, skipped))
}

fn hash_line(base: &str, occurrence: u32) -> String {
    let mut h = Sha256::new();
    h.update(base.as_bytes());
    h.update([b'|']);
    h.update(occurrence.to_le_bytes());
    format!("{:x}", h.finalize())
}

/// Auto-match (Spec 04 §7.2): exact amount + date within ±3 days + UNIQUE candidate.
/// Ambiguity is never guessed. Auto-matches are revocable via unmatch before completion.
pub fn auto_match(conn: &mut Connection, recon_id: &str) -> R<usize> {
    let coa_account: String = conn.query_row(
        "SELECT b.account_id FROM reconciliations r JOIN bank_accounts b ON b.id = r.bank_account_id
         WHERE r.id = ?1", params![recon_id], |r| r.get(0),
    )?;
    let lines: Vec<(String, String, i64)> = {
        let mut q = conn.prepare(
            "SELECT id, stmt_date, stmt_amount_kobo FROM reconciliation_lines
             WHERE reconciliation_id = ?1 AND state = 'unmatched'",
        )?;
        let it = q.query_map(params![recon_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        it.collect::<Result<_, _>>()?
    };
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut matched = 0usize;
    for (line_id, date, amount) in lines {
        let candidates: Vec<String> = {
            let mut q = tx.prepare(
                "SELECT l.id FROM journal_lines l
                 JOIN journal_entries e ON e.id = l.entry_id
                 WHERE l.account_id = ?1 AND e.is_posted = 1 AND l.amount_kobo = ?2
                   AND ABS(julianday(e.entry_date) - julianday(?3)) <= 3
                   AND NOT EXISTS (SELECT 1 FROM reconciliation_matches m WHERE m.journal_line_id = l.id)",
            )?;
            let it = q.query_map(params![coa_account, amount, date], |r| r.get(0))?;
            it.collect::<Result<_, _>>()?
        };
        if candidates.len() == 1 {
            tx.execute(
                "INSERT INTO reconciliation_matches (reconciliation_line_id, journal_line_id) VALUES (?1, ?2)",
                params![line_id, candidates[0]],
            )?;
            tx.execute(
                "UPDATE reconciliation_lines SET state = 'matched', match_kind = 'auto' WHERE id = ?1",
                params![line_id],
            )?;
            matched += 1;
        }
    }
    tx.commit()?;
    Ok(matched)
}

/// Manual match, 1:N sum-exact (Spec 04 §7.3). `kind`: 'manual' or 'created'.
pub fn manual_match(conn: &mut Connection, line_id: &str, journal_line_ids: &[String], kind: &str) -> R<()> {
    if journal_line_ids.is_empty() {
        return Err(EngineError::Validation("pick at least one entry to match".into()));
    }
    let (state, amount): (String, i64) = conn.query_row(
        "SELECT state, stmt_amount_kobo FROM reconciliation_lines WHERE id = ?1",
        params![line_id], |r| Ok((r.get(0)?, r.get(1)?)),
    ).optional()?
     .ok_or_else(|| EngineError::Validation("unknown statement line".into()))?;
    if state != "unmatched" && state != "needs_review" {
        return Err(EngineError::Validation("this line is already settled".into()));
    }
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut sum = 0i64;
    for jl in journal_line_ids {
        let (amt, taken): (i64, i64) = tx.query_row(
            "SELECT l.amount_kobo,
                    EXISTS(SELECT 1 FROM reconciliation_matches m WHERE m.journal_line_id = l.id)
             FROM journal_lines l WHERE l.id = ?1",
            params![jl], |r| Ok((r.get(0)?, r.get(1)?)),
        ).optional()?
         .ok_or_else(|| EngineError::Validation("unknown entry line".into()))?;
        if taken != 0 {
            return Err(EngineError::Validation("one of those entries is already matched elsewhere".into()));
        }
        sum += amt;
        tx.execute(
            "INSERT INTO reconciliation_matches (reconciliation_line_id, journal_line_id) VALUES (?1, ?2)",
            params![line_id, jl],
        )?;
    }
    if sum != amount {
        return Err(EngineError::Validation(
            "amounts must tie exactly — no partial matches (Spec 04 §7.3)".into(),
        ));
    }
    let new_state = if kind == "created" { "entry_created" } else { "matched" };
    let resolution: Option<&str> = if state == "needs_review" { Some(new_state) } else { None };
    tx.execute(
        "UPDATE reconciliation_lines SET state = ?2, match_kind = ?3,
           resolved_at = CASE WHEN ?4 IS NOT NULL THEN ?5 ELSE resolved_at END,
           resolution  = COALESCE(?4, resolution)
         WHERE id = ?1",
        params![line_id, new_state, kind, resolution, now_iso()],
    )?;
    tx.commit()?;
    Ok(())
}

/// Revoke a match before completion (auto-matches are individually revocable).
pub fn unmatch(conn: &mut Connection, line_id: &str) -> R<()> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute("DELETE FROM reconciliation_matches WHERE reconciliation_line_id = ?1", params![line_id])?;
    tx.execute(
        "UPDATE reconciliation_lines SET state = 'unmatched', match_kind = NULL WHERE id = ?1
           AND state IN ('matched','entry_created')",
        params![line_id],
    )?;
    tx.commit()?;
    Ok(())
}

/// Flag needs-review: mandatory note; NOTHING posts (the whole point).
pub fn flag_needs_review(conn: &Connection, line_id: &str, note: &str, user_id: Option<&str>) -> R<()> {
    if note.trim().is_empty() {
        return Err(EngineError::Validation(
            "say what you know about this line — even 'no idea' helps your advisor".into(),
        ));
    }
    let n = conn.execute(
        "UPDATE reconciliation_lines
         SET state = 'needs_review',
             review_note = COALESCE(review_note || char(10), '') || ?2,
             flagged_by = COALESCE(flagged_by, ?3), flagged_at = COALESCE(flagged_at, ?4)
         WHERE id = ?1 AND state IN ('unmatched','needs_review')",
        params![line_id, note.trim(), user_id, now_iso()],
    )?;
    if n == 0 {
        return Err(EngineError::Validation("this line is already settled".into()));
    }
    Ok(())
}

/// Write off an unexplainable residual (Spec 04 §7.5 path 3): posts a REAL entry
/// via the company's routing settings; above the limit it needs the advisor.
pub fn write_off(conn: &mut Connection, line_id: &str, note: &str, ctx: &PostCtx) -> R<String> {
    let (recon_id, state, date, desc, amount): (String, String, String, Option<String>, i64) = conn
        .query_row(
            "SELECT reconciliation_id, state, stmt_date, stmt_description, stmt_amount_kobo
             FROM reconciliation_lines WHERE id = ?1",
            params![line_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        ).optional()?
        .ok_or_else(|| EngineError::Validation("unknown statement line".into()))?;
    if state != "unmatched" && state != "needs_review" {
        return Err(EngineError::Validation("this line is already settled".into()));
    }
    let (company_id, coa_account): (String, String) = conn.query_row(
        "SELECT r.company_id, b.account_id FROM reconciliations r
         JOIN bank_accounts b ON b.id = r.bank_account_id WHERE r.id = ?1",
        params![recon_id], |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let (limit, dr_acct, cr_acct): (i64, Option<String>, Option<String>) = conn.query_row(
        "SELECT writeoff_limit_kobo, writeoff_debit_account_id, writeoff_credit_account_id
         FROM companies WHERE id = ?1",
        params![company_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    // Spec 04 §9 Decision #11 (final, no bypass): above the limit, this
    // function refuses unconditionally — for every role, elevated or not.
    // The resolution path is a manual journal entry (Advisor Mode) matched
    // back to this line via the ordinary manual-match flow, not an override
    // here.
    if amount.abs() > limit {
        return Err(EngineError::WriteOffAboveLimit { limit_kobo: limit });
    }
    let by_code = |code: &str, tx: &Connection| -> R<String> {
        Ok(tx.query_row(
            "SELECT id FROM accounts WHERE company_id = ?1 AND code = ?2",
            params![company_id, code], |r| r.get(0),
        )?)
    };
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let memo = format!(
        "Bank line written off — {} ({})",
        desc.as_deref().unwrap_or("no description"),
        note.trim(),
    );
    // Statement +amount = money into the bank → Dr bank / Cr other-income route;
    // −amount = money out → Dr misc-expense route / Cr bank.
    let lines = if amount > 0 {
        let cr = match &cr_acct { Some(a) => a.clone(), None => by_code("4200", &tx)? };
        vec![LineSpec::new(&coa_account, amount), LineSpec::new(&cr, -amount)]
    } else {
        let dr = match &dr_acct { Some(a) => a.clone(), None => by_code("6980", &tx)? };
        vec![LineSpec::new(&dr, -amount), LineSpec::new(&coa_account, amount)]
    };
    let entry = post_entry_in(
        &tx, &company_id, &date, &memo, "reconciliation_writeoff",
        Some(line_id), ctx.user_id.as_deref(), &lines,
    ).map_err(EngineError::from)?;
    // The entry's bank leg matches its own statement line.
    let bank_leg: String = tx.query_row(
        "SELECT id FROM journal_lines WHERE entry_id = ?1 AND account_id = ?2",
        params![entry, coa_account], |r| r.get(0),
    )?;
    tx.execute(
        "INSERT INTO reconciliation_matches (reconciliation_line_id, journal_line_id) VALUES (?1, ?2)",
        params![line_id, bank_leg],
    )?;
    tx.execute(
        "UPDATE reconciliation_lines SET state = 'written_off', resolution = 'written_off',
           resolved_by = ?2, resolved_at = ?3,
           review_note = COALESCE(review_note || char(10), '') || ?4
         WHERE id = ?1",
        params![line_id, ctx.user_id, now_iso(), note.trim()],
    )?;
    tx.commit()?;
    Ok(entry)
}

/// Mark a line as import garbage (Spec 04 §7.5 path 4) — excluded, no posting.
pub fn mark_import_error(conn: &Connection, line_id: &str, ctx: &PostCtx) -> R<()> {
    let n = conn.execute(
        "UPDATE reconciliation_lines SET state = 'import_error', resolution = 'import_error',
           resolved_by = ?2, resolved_at = ?3
         WHERE id = ?1 AND state IN ('unmatched','needs_review')",
        params![line_id, ctx.user_id, now_iso()],
    )?;
    if n == 0 {
        return Err(EngineError::Validation("this line is already settled".into()));
    }
    Ok(())
}

#[derive(Debug)]
pub struct Equation {
    pub statement_balance_kobo: i64,
    pub matched_kobo: i64,     // matched + entry_created + written_off statement amounts
    pub unresolved_kobo: i64,  // unmatched + needs_review statement amounts
    pub ledger_at_date_kobo: i64,
    pub outstanding_kobo: i64, // in the books, not on the statement
}

pub fn equation(conn: &Connection, recon_id: &str) -> R<Equation> {
    let (stmt_bal, stmt_date, coa): (i64, String, String) = conn.query_row(
        "SELECT r.statement_balance_kobo, r.statement_date, b.account_id
         FROM reconciliations r JOIN bank_accounts b ON b.id = r.bank_account_id
         WHERE r.id = ?1",
        params![recon_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let sum_state = |states: &str| -> R<i64> {
        Ok(conn.query_row(
            &format!(
                "SELECT COALESCE(SUM(stmt_amount_kobo),0) FROM reconciliation_lines
                 WHERE reconciliation_id = ?1 AND state IN ({states})"
            ),
            params![recon_id], |r| r.get(0),
        )?)
    };
    let matched = sum_state("'matched','entry_created','written_off'")?;
    let unresolved = sum_state("'unmatched','needs_review'")?;
    let ledger: i64 = conn.query_row(
        "SELECT COALESCE(SUM(l.amount_kobo),0) FROM journal_lines l
         JOIN journal_entries e ON e.id = l.entry_id
         WHERE l.account_id = ?1 AND e.is_posted = 1 AND e.entry_date <= ?2",
        params![coa, stmt_date], |r| r.get(0),
    )?;
    Ok(Equation {
        statement_balance_kobo: stmt_bal,
        matched_kobo: matched,
        unresolved_kobo: unresolved,
        ledger_at_date_kobo: ledger,
        outstanding_kobo: ledger - matched,
    })
}

/// Complete (Spec 04 §7.5): every line must carry A decision — 'unclear' counts,
/// merely 'unmatched' does not. Needs-review lines produce completed_with_exceptions
/// and carry into the next session. Stamps the P6 lock either way.
pub fn complete(conn: &mut Connection, recon_id: &str) -> R<String> {
    let (bank_account_id, stmt_date, status): (String, String, String) = conn.query_row(
        "SELECT bank_account_id, statement_date, status FROM reconciliations WHERE id = ?1",
        params![recon_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    ).optional()?
     .ok_or_else(|| EngineError::Validation("unknown reconciliation".into()))?;
    if status != "in_progress" {
        return Err(EngineError::Validation("already completed".into()));
    }
    let (undecided, flagged): (i64, i64) = conn.query_row(
        "SELECT SUM(CASE WHEN state = 'unmatched' THEN 1 ELSE 0 END),
                SUM(CASE WHEN state = 'needs_review' THEN 1 ELSE 0 END)
         FROM reconciliation_lines WHERE reconciliation_id = ?1",
        params![recon_id], |r| Ok((r.get::<_, Option<i64>>(0)?.unwrap_or(0), r.get::<_, Option<i64>>(1)?.unwrap_or(0))),
    )?;
    if undecided > 0 {
        return Err(EngineError::Validation(format!(
            "{undecided} line(s) still need a decision — match them, record them, or flag them 'not sure'"
        )));
    }
    let new_status = if flagged > 0 { "completed_with_exceptions" } else { "completed" };
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute(
        "UPDATE reconciliations SET status = ?2, completed_at = ?3 WHERE id = ?1",
        params![recon_id, new_status, now_iso()],
    )?;
    tx.execute(
        "UPDATE bank_accounts SET last_reconciled_date = ?2 WHERE id = ?1",
        params![bank_account_id, stmt_date],
    )?;
    tx.commit()?;
    Ok(new_status.to_string())
}
