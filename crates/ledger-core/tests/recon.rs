//! Reconciliation tests (Spec 04 §6–7): CSV parse, import/dedup, auto-match
//! uniqueness, needs-review carry-forward, write-off routing + threshold,
//! completion states, and the P6 lock landing after completion.

use ledger_core::engine::PostCtx;
use ledger_core::ids::new_id;
use ledger_core::posting::{post_entry, LineSpec};
use ledger_core::recon::*;
use ledger_core::rusqlite::{params, Connection};
use ledger_core::seed::{add_bank_account, create_company, CompanyConfig};

struct W {
    conn: Connection,
    company: String,
    bank: String,     // bank_accounts.id
    bank_coa: String, // its COA account id
    sales: String,
}

fn world() -> W {
    let mut conn = ledger_core::open(":memory:").unwrap();
    let company = create_company(&mut conn, &CompanyConfig::default()).unwrap();
    let bank = add_bank_account(&mut conn, &company, "GTBank Current", "bank", "NGN").unwrap();
    let bank_coa: String = conn
        .query_row("SELECT account_id FROM bank_accounts WHERE id = ?1", params![bank], |r| r.get(0))
        .unwrap();
    let sales: String = conn
        .query_row(
            "SELECT id FROM accounts WHERE company_id = ?1 AND system_key = 'SALES_DEFAULT'",
            params![company], |r| r.get(0),
        )
        .unwrap();
    W { conn, company, bank, bank_coa, sales }
}

fn bank_move(w: &mut W, date: &str, amount: i64) {
    // + = money in (Dr bank / Cr sales), − = money out (Cr bank / Dr sales-as-dummy)
    post_entry(
        &mut w.conn, &w.company, date, "test movement", "manual", None, None,
        &[LineSpec::new(&w.bank_coa, amount), LineSpec::new(&w.sales, -amount)],
    )
    .unwrap();
}

fn trial_balance(conn: &Connection) -> i64 {
    conn.query_row(
        "SELECT COALESCE(SUM(l.amount_kobo),0) FROM journal_lines l
         JOIN journal_entries e ON e.id = l.entry_id WHERE e.is_posted = 1",
        [], |r| r.get(0),
    )
    .unwrap()
}

const CSV: &str = "\
Date,Description,Debit,Credit
02/07/2026,POS PURCHASE DIESEL,\"50,000.00\",
03/07/2026,TRANSFER MAINLAND SUPP,\"20,000.00\",
05/07/2026,SMS ALERT CHARGE,\"1,500.00\",
06/07/2026,UNKNOWN CREDIT REF 8841,,\"300,000.00\"
";

fn mapping() -> CsvMapping {
    CsvMapping {
        header_rows: 1, date_col: 0, desc_col: 1,
        amount_col: None, debit_col: Some(2), credit_col: Some(3),
        date_format: "DMY".into(), flip_sign: false,
    }
}

#[test]
fn full_reconciliation_flow_with_needs_review_carry_forward() {
    let mut w = world();
    let ctx = PostCtx::default();

    // Ledger has the two payments the owner recorded; the bank charge and the
    // mystery credit exist only on the statement.
    bank_move(&mut w, "2026-07-02", -50_000_00);
    bank_move(&mut w, "2026-07-03", -20_000_00);

    let (rows, errors) = parse_csv(CSV, &mapping());
    assert_eq!(errors.len(), 0, "{errors:?}");
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0].amount_kobo, -50_000_00); // debit = money out
    assert_eq!(rows[3].amount_kobo, 300_000_00); // credit = money in
    assert_eq!(rows[0].date, "2026-07-02");

    let recon = start_reconciliation(&mut w.conn, &w.company, &w.bank, "2026-07-31", 228_500_00).unwrap();
    let (imported, skipped) = import_rows(&mut w.conn, &recon, &rows).unwrap();
    assert_eq!((imported, skipped), (4, 0));

    // Overlapping re-import: skip-and-report, nothing duplicated (Spec 04 §6.3).
    let (imported2, skipped2) = import_rows(&mut w.conn, &recon, &rows).unwrap();
    assert_eq!((imported2, skipped2), (0, 4));

    // Auto-match found exactly the two recorded payments.
    let matched: i64 = w.conn.query_row(
        "SELECT COUNT(*) FROM reconciliation_lines WHERE reconciliation_id = ?1 AND state = 'matched'",
        params![recon], |r| r.get(0),
    ).unwrap();
    assert_eq!(matched, 2);

    // Bank charge (₦1,500 ≤ ₦5,000 limit): write off → REAL entry, balanced books.
    let charge_line: String = w.conn.query_row(
        "SELECT id FROM reconciliation_lines WHERE reconciliation_id = ?1 AND stmt_amount_kobo = -150000",
        params![recon], |r| r.get(0),
    ).unwrap();
    write_off(&mut w.conn, &charge_line, "bank SMS charge", &ctx).unwrap();
    assert_eq!(trial_balance(&w.conn), 0);

    // Mystery ₦300,000 credit: write-off blocked above the limit; flag needs
    // a note (empty rejected); then flagged — nothing posted anywhere.
    let mystery: String = w.conn.query_row(
        "SELECT id FROM reconciliation_lines WHERE reconciliation_id = ?1 AND stmt_amount_kobo = 30000000",
        params![recon], |r| r.get(0),
    ).unwrap();
    assert!(write_off(&mut w.conn, &mystery, "?", &ctx).is_err());
    assert!(flag_needs_review(&w.conn, &mystery, "   ", None).is_err());
    let entries_before: i64 = w.conn.query_row(
        "SELECT COUNT(*) FROM journal_entries", [], |r| r.get(0)).unwrap();
    flag_needs_review(&w.conn, &mystery, "no idea — GTB app shows nothing", None).unwrap();
    let entries_after: i64 = w.conn.query_row(
        "SELECT COUNT(*) FROM journal_entries", [], |r| r.get(0)).unwrap();
    assert_eq!(entries_before, entries_after, "needs-review posts NOTHING");

    // Equation reads sanely.
    let eq = equation(&w.conn, &recon).unwrap();
    assert_eq!(eq.matched_kobo, -71_500_00);
    assert_eq!(eq.unresolved_kobo, 300_000_00);

    // Complete around the flagged line → completed_with_exceptions + P6 stamp.
    let status = complete(&mut w.conn, &recon).unwrap();
    assert_eq!(status, "completed_with_exceptions");
    let lock: String = w.conn.query_row(
        "SELECT last_reconciled_date FROM bank_accounts WHERE id = ?1",
        params![w.bank], |r| r.get(0),
    ).unwrap();
    assert_eq!(lock, "2026-07-31");

    // P6: the engine now rejects postings to this account on/before the lock.
    let err = ledger_core::engine::post_drawing(
        &mut w.conn, &w.company, &w.bank, "2026-07-15", 10_000_00, true, &ctx,
    ).unwrap_err();
    assert!(err.to_string().contains("reconciled"));

    // Next session: the flagged line carries forward, linked to its origin.
    let recon2 = start_reconciliation(&mut w.conn, &w.company, &w.bank, "2026-08-31", 0).unwrap();
    let (n, from): (i64, Option<String>) = w.conn.query_row(
        "SELECT COUNT(*), MAX(carried_from_id) FROM reconciliation_lines
         WHERE reconciliation_id = ?1 AND state = 'needs_review'",
        params![recon2], |r| Ok((r.get(0)?, r.get(1)?)),
    ).unwrap();
    assert_eq!(n, 1);
    assert_eq!(from.as_deref(), Some(mystery.as_str()));
}

#[test]
fn manual_match_is_sum_exact_one_to_many() {
    let mut w = world();
    bank_move(&mut w, "2026-07-10", 40_000_00);
    bank_move(&mut w, "2026-07-10", 30_000_00);

    let recon = start_reconciliation(&mut w.conn, &w.company, &w.bank, "2026-07-31", 70_000_00).unwrap();
    // One bulk statement credit covering both ledger entries (Spec 04 §7.3).
    import_rows(&mut w.conn, &recon, &[StmtRow {
        date: "2026-07-10".into(), description: "BULK TRANSFER".into(), amount_kobo: 70_000_00,
    }]).unwrap();
    let line: String = w.conn.query_row(
        "SELECT id FROM reconciliation_lines WHERE reconciliation_id = ?1", params![recon], |r| r.get(0),
    ).unwrap();
    // Auto-match must NOT have guessed (no single 70k candidate; two partials don't qualify).
    let state: String = w.conn.query_row(
        "SELECT state FROM reconciliation_lines WHERE id = ?1", params![line], |r| r.get(0),
    ).unwrap();
    assert_eq!(state, "unmatched");

    let legs: Vec<String> = {
        let mut q = w.conn.prepare(
            "SELECT l.id FROM journal_lines l WHERE l.account_id = ?1 AND l.amount_kobo > 0",
        ).unwrap();
        let it = q.query_map(params![w.bank_coa], |r| r.get(0)).unwrap();
        it.collect::<Result<_, _>>().unwrap()
    };
    assert_eq!(legs.len(), 2);

    // Partial (one leg only) fails sum-exact; both legs tie.
    assert!(manual_match(&mut w.conn, &line, &legs[..1].to_vec(), "manual").is_err());
    manual_match(&mut w.conn, &line, &legs, "manual").unwrap();
    let state: String = w.conn.query_row(
        "SELECT state FROM reconciliation_lines WHERE id = ?1", params![line], |r| r.get(0),
    ).unwrap();
    assert_eq!(state, "matched");

    // Fully decided, no flags → plain 'completed'.
    assert_eq!(complete(&mut w.conn, &recon).unwrap(), "completed");
}
