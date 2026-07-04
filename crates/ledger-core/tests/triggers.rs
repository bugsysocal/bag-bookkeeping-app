//! T1–T8 trigger tests (Spec 01 §4): the core promises, proven at the DB level.
//! These deliberately attack the database *directly* (raw SQL, bypassing the
//! posting harness) where the point is that the TRIGGER stops the write — the
//! app layer being polite is not the guarantee; the schema is.

use ledger_core::ids::{new_id, now_iso};
use ledger_core::{post_entry, LineSpec, PostError};
use rusqlite::{params, Connection};

/// Fresh in-memory db with one company, one user, and a minimal COA.
fn test_db() -> (Connection, Ctx) {
    let mut conn = ledger_core::open(":memory:").expect("open");
    let ctx = seed(&mut conn);
    (conn, ctx)
}

struct Ctx {
    company: String,
    user: String,
    bank: String,    // 1010 asset
    ar: String,      // 1100 asset, system AR
    sales: String,   // 4000 income, system SALES_DEFAULT
}

fn seed(conn: &mut Connection) -> Ctx {
    let company = new_id();
    let user = new_id();
    let now = now_iso();
    conn.execute(
        "INSERT INTO companies (id, name, created_at) VALUES (?1, 'Test Traders Ltd', ?2)",
        params![company, now],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO users (id, company_id, name, role, created_at)
         VALUES (?1, ?2, 'Owner', 'owner', ?3)",
        params![user, company, now],
    )
    .unwrap();

    let mut acct = |code: &str, name: &str, class: &str, system_key: Option<&str>, is_bank: i64| {
        let id = new_id();
        conn.execute(
            "INSERT INTO accounts (id, company_id, code, name, class, system_key, is_bank, is_system)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![id, company, code, name, class, system_key, is_bank, system_key.is_some() as i64],
        )
        .unwrap();
        id
    };

    let bank = acct("1010", "GTBank Current", "asset", None, 1);
    let ar = acct("1100", "Accounts Receivable", "asset", Some("AR"), 0);
    let sales = acct("4000", "Sales Revenue", "income", Some("SALES_DEFAULT"), 0);

    Ctx { company, user, bank, ar, sales }
}

fn ok_lines(ctx: &Ctx, amount: i64) -> Vec<LineSpec> {
    vec![
        LineSpec::new(&ctx.ar, amount),
        LineSpec::new(&ctx.sales, -amount),
    ]
}

fn post_ok(conn: &mut Connection, ctx: &Ctx) -> String {
    post_entry(
        conn, &ctx.company, "2026-07-03", "Invoice INV-000001 — Test Customer",
        "invoice", None, Some(&ctx.user), &ok_lines(ctx, 150_000_00),
    )
    .expect("balanced entry should post")
}

// ===== T1: balanced or not at all =====

#[test]
fn t1_balanced_entry_posts() {
    let (mut conn, ctx) = test_db();
    let entry = post_ok(&mut conn, &ctx);
    let (posted, sum): (i64, i64) = conn
        .query_row(
            "SELECT je.is_posted, (SELECT SUM(amount_kobo) FROM journal_lines WHERE entry_id = je.id)
             FROM journal_entries je WHERE je.id = ?1",
            params![entry],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(posted, 1);
    assert_eq!(sum, 0);
}

#[test]
fn t1_unbalanced_entry_rejected_by_trigger_not_just_harness() {
    let (mut conn, ctx) = test_db();
    // Harness catches it first (P3)…
    let unbalanced = vec![LineSpec::new(&ctx.ar, 100), LineSpec::new(&ctx.sales, -99)];
    let err = post_entry(
        &mut conn, &ctx.company, "2026-07-03", "bad", "manual", None, None, &unbalanced,
    )
    .unwrap_err();
    assert!(matches!(err, PostError::Validation(_)));

    // …but the TRIGGER is the real guarantee: insert unbalanced rows raw, flip is_posted.
    let entry = new_id();
    let now = now_iso();
    conn.execute(
        "INSERT INTO journal_entries (id, company_id, entry_date, memo, source_type, is_posted, created_at)
         VALUES (?1, ?2, '2026-07-03', 'attack', 'manual', 0, ?3)",
        params![entry, ctx.company, now],
    )
    .unwrap();
    for (acct, amt) in [(&ctx.ar, 100i64), (&ctx.sales, -99i64)] {
        conn.execute(
            "INSERT INTO journal_lines (id, entry_id, line_no, account_id, amount_kobo)
             VALUES (?1, ?2, 1, ?3, ?4)",
            params![new_id(), entry, acct, amt],
        )
        .unwrap();
    }
    let err = conn
        .execute("UPDATE journal_entries SET is_posted = 1 WHERE id = ?1", params![entry])
        .unwrap_err();
    assert!(err.to_string().contains("does not balance"));
}

#[test]
fn t1_single_line_entry_rejected() {
    let (mut conn, ctx) = test_db();
    let entry = new_id();
    conn.execute(
        "INSERT INTO journal_entries (id, company_id, entry_date, memo, source_type, is_posted, created_at)
         VALUES (?1, ?2, '2026-07-03', 'one-legged', 'manual', 0, ?3)",
        params![entry, ctx.company, now_iso()],
    )
    .unwrap();
    // A single zero-sum line is impossible (amount != 0 CHECK), so a lone line can't balance —
    // but the two-line floor must hold independently. Insert nothing and flip:
    let err = conn
        .execute("UPDATE journal_entries SET is_posted = 1 WHERE id = ?1", params![entry])
        .unwrap_err();
    assert!(err.to_string().contains("at least two lines"));
}

#[test]
fn zero_amount_line_rejected_by_check() {
    let (conn, ctx) = test_db();
    let entry = new_id();
    conn.execute(
        "INSERT INTO journal_entries (id, company_id, entry_date, memo, source_type, is_posted, created_at)
         VALUES (?1, ?2, '2026-07-03', 'zero line', 'manual', 0, ?3)",
        params![entry, ctx.company, now_iso()],
    )
    .unwrap();
    let err = conn
        .execute(
            "INSERT INTO journal_lines (id, entry_id, line_no, account_id, amount_kobo)
             VALUES (?1, ?2, 1, ?3, 0)",
            params![new_id(), entry, ctx.bank],
        )
        .unwrap_err();
    assert!(err.to_string().to_lowercase().contains("check"));
}

// ===== T2/T3: posted lines are immutable =====

#[test]
fn t2_posted_line_update_rejected() {
    let (mut conn, ctx) = test_db();
    let entry = post_ok(&mut conn, &ctx);
    let err = conn
        .execute(
            "UPDATE journal_lines SET amount_kobo = amount_kobo + 1 WHERE entry_id = ?1 AND line_no = 1",
            params![entry],
        )
        .unwrap_err();
    assert!(err.to_string().contains("immutable"));
}

#[test]
fn t3_posted_line_delete_rejected() {
    let (mut conn, ctx) = test_db();
    let entry = post_ok(&mut conn, &ctx);
    let err = conn
        .execute("DELETE FROM journal_lines WHERE entry_id = ?1", params![entry])
        .unwrap_err();
    assert!(err.to_string().contains("cannot be deleted"));
}

// ===== T4: posted entries are never deleted =====

#[test]
fn t4_posted_entry_delete_rejected_unposted_draft_deletable() {
    let (mut conn, ctx) = test_db();
    let entry = post_ok(&mut conn, &ctx);
    let err = conn
        .execute("DELETE FROM journal_entries WHERE id = ?1", params![entry])
        .unwrap_err();
    assert!(err.to_string().contains("post a reversal"));

    // An unposted draft (never flipped) may be cleaned up.
    let draft = new_id();
    conn.execute(
        "INSERT INTO journal_entries (id, company_id, entry_date, memo, source_type, is_posted, created_at)
         VALUES (?1, ?2, '2026-07-03', 'draft', 'manual', 0, ?3)",
        params![draft, ctx.company, now_iso()],
    )
    .unwrap();
    let n = conn
        .execute("DELETE FROM journal_entries WHERE id = ?1", params![draft])
        .unwrap();
    assert_eq!(n, 1);
}

// ===== T5: hard close blocks posting into the locked period =====

#[test]
fn t5_hard_close_blocks_posting_into_period() {
    let (mut conn, ctx) = test_db();
    conn.execute(
        "UPDATE companies SET hard_close_through = '2026-06-30' WHERE id = ?1",
        params![ctx.company],
    )
    .unwrap();

    let err = post_entry(
        &mut conn, &ctx.company, "2026-06-15", "into closed period", "manual",
        None, None, &ok_lines(&ctx, 1_000_00),
    )
    .unwrap_err();
    assert!(err.to_string().contains("hard-closed"));

    // Day after the lock: posts fine. The lock is an edit-permission control —
    // there are no closing entries in this system (Spec 05 §4.3 terminology guard).
    post_entry(
        &mut conn, &ctx.company, "2026-07-01", "open period", "manual",
        None, None, &ok_lines(&ctx, 1_000_00),
    )
    .expect("open-period entry should post");
}

// ===== T6/T7: audit log is append-only =====

#[test]
fn t6_t7_audit_log_append_only() {
    let (conn, ctx) = test_db();
    let audit = new_id();
    conn.execute(
        "INSERT INTO audit_log (id, company_id, user_id, action, entity_type, entity_id, created_at)
         VALUES (?1, ?2, ?3, 'invoice.posted', 'invoice', 'x', ?4)",
        params![audit, ctx.company, ctx.user, now_iso()],
    )
    .unwrap();

    let err = conn
        .execute("UPDATE audit_log SET action = 'tampered' WHERE id = ?1", params![audit])
        .unwrap_err();
    assert!(err.to_string().contains("append-only"));

    let err = conn
        .execute("DELETE FROM audit_log WHERE id = ?1", params![audit])
        .unwrap_err();
    assert!(err.to_string().contains("append-only"));
}

// ===== T8: account class immutable; system accounts locked =====

#[test]
fn t8_account_class_immutable() {
    let (conn, ctx) = test_db();
    let err = conn
        .execute("UPDATE accounts SET class = 'expense' WHERE id = ?1", params![ctx.sales])
        .unwrap_err();
    assert!(err.to_string().contains("class is immutable"));
}

#[test]
fn t8_system_account_cannot_be_deactivated_or_rekeyed() {
    let (conn, ctx) = test_db();
    let err = conn
        .execute("UPDATE accounts SET is_active = 0 WHERE id = ?1", params![ctx.ar])
        .unwrap_err();
    assert!(err.to_string().contains("system accounts"));

    let err = conn
        .execute("UPDATE accounts SET system_key = 'HIJACKED' WHERE id = ?1", params![ctx.ar])
        .unwrap_err();
    assert!(err.to_string().contains("system accounts"));

    // Rename (name only) is allowed — within-class-constraints editing (Spec 01 §3.2).
    let n = conn
        .execute("UPDATE accounts SET name = 'Trade Receivables' WHERE id = ?1", params![ctx.ar])
        .unwrap();
    assert_eq!(n, 1);

    // Non-system accounts may deactivate.
    let n = conn
        .execute("UPDATE accounts SET is_active = 0 WHERE id = ?1", params![ctx.bank])
        .unwrap();
    assert_eq!(n, 1);
}

// ===== The induction step: trial balance is zero after arbitrary posted activity =====

#[test]
fn trial_balance_sums_to_zero_always() {
    let (mut conn, ctx) = test_db();
    post_ok(&mut conn, &ctx);
    post_entry(
        &mut conn, &ctx.company, "2026-07-03", "Payment RCT-000001", "payment", None,
        Some(&ctx.user),
        &[LineSpec::new(&ctx.bank, 150_000_00), LineSpec::new(&ctx.ar, -150_000_00)],
    )
    .unwrap();

    let sum: i64 = conn
        .query_row(
            "SELECT COALESCE(SUM(l.amount_kobo), 0)
             FROM journal_lines l JOIN journal_entries e ON e.id = l.entry_id
             WHERE e.is_posted = 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(sum, 0, "trial balance must be zero by induction (Spec 01 §7)");
}
