//! Open Invoices onboarding importer tests (Spec 06 §3/§3.1): preview
//! validation, customer name resolution (exact/fuzzy/auto-create), the
//! anti-double-count guard against a wizard lump opening line, and commit's
//! Dr AR / Cr OBE posting.

use ledger_core::engine::{has_wizard_opening_line, post_journal, PostCtx};
use ledger_core::import_open_invoices::{commit_open_invoices, preview_open_invoices, RowStatus};
use ledger_core::rusqlite::{params, Connection};
use ledger_core::seed::{create_company, CompanyConfig};
use ledger_core::LineSpec;

struct W {
    conn: Connection,
    company: String,
}

fn world() -> W {
    let mut conn = ledger_core::open(":memory:").unwrap();
    let company = create_company(&mut conn, &CompanyConfig::default()).unwrap();
    W { conn, company }
}

fn header() -> Vec<String> {
    vec!["invoice_no", "customer", "issue_date", "due_date", "original_total", "balance_outstanding"]
        .into_iter()
        .map(String::from)
        .collect()
}

fn row(cells: &[&str]) -> Vec<String> {
    cells.iter().map(|s| s.to_string()).collect()
}

fn trial_balance(conn: &Connection) -> i64 {
    conn.query_row("SELECT COALESCE(SUM(amount_kobo), 0) FROM journal_lines", [], |r| r.get(0)).unwrap()
}

fn sys_account(conn: &Connection, company: &str, key: &str) -> String {
    conn.query_row(
        "SELECT id FROM accounts WHERE company_id = ?1 AND system_key = ?2",
        params![company, key],
        |r| r.get(0),
    )
    .unwrap()
}

#[test]
fn preview_flags_missing_required_fields_as_errors() {
    let w = world();
    let rows = vec![
        header(),
        row(&["", "Zenith Traders", "01/03/2026", "15/03/2026", "500000", "200000"]),
        row(&["INV-1", "", "01/03/2026", "15/03/2026", "500000", "200000"]),
    ];
    let out = preview_open_invoices(&w.conn, &w.company, &rows);
    assert_eq!(out[0].status, RowStatus::Error);
    assert!(out[0].message.as_ref().unwrap().contains("Invoice number"));
    assert_eq!(out[1].status, RowStatus::Error);
    assert!(out[1].message.as_ref().unwrap().contains("Customer"));
}

#[test]
fn preview_rejects_balance_exceeding_original_total() {
    let w = world();
    let rows = vec![header(), row(&["INV-1", "Zenith Traders", "01/03/2026", "15/03/2026", "100000", "150000"])];
    let out = preview_open_invoices(&w.conn, &w.company, &rows);
    assert_eq!(out[0].status, RowStatus::Error);
    assert!(out[0].message.as_ref().unwrap().contains("Balance outstanding"));
}

#[test]
fn preview_resolves_exact_customer_match_as_ready() {
    let w = world();
    let ctx = PostCtx::default();
    w.conn
        .execute(
            "INSERT INTO contacts (id, company_id, kind, name, created_at)
             VALUES ('c1', ?1, 'customer', 'Zenith Traders', '2026-01-01T00:00:00Z')",
            params![w.company],
        )
        .unwrap();
    let rows = vec![header(), row(&["INV-1", "Zenith Traders", "01/03/2026", "15/03/2026", "500000", "200000"])];
    let out = preview_open_invoices(&w.conn, &w.company, &rows);
    assert_eq!(out[0].status, RowStatus::Ready);
    assert_eq!(out[0].resolved_contact_id.as_deref(), Some("c1"));
    assert_eq!(out[0].issue_date, "2026-03-01");
    let _ = ctx;
}

#[test]
fn preview_warns_on_fuzzy_customer_match_but_still_resolves_it() {
    let w = world();
    w.conn
        .execute(
            "INSERT INTO contacts (id, company_id, kind, name, created_at)
             VALUES ('c1', ?1, 'customer', 'Zenith Traders', '2026-01-01T00:00:00Z')",
            params![w.company],
        )
        .unwrap();
    let rows = vec![header(), row(&["INV-1", "zenith traders", "01/03/2026", "15/03/2026", "500000", "200000"])];
    let out = preview_open_invoices(&w.conn, &w.company, &rows);
    assert_eq!(out[0].status, RowStatus::Warning);
    assert_eq!(out[0].resolved_contact_id.as_deref(), Some("c1"));
    assert!(out[0].message.as_ref().unwrap().contains("Matched"));
}

#[test]
fn preview_warns_and_leaves_unresolved_for_unknown_customer() {
    let w = world();
    let rows = vec![header(), row(&["INV-1", "Brand New Customer", "01/03/2026", "15/03/2026", "500000", "200000"])];
    let out = preview_open_invoices(&w.conn, &w.company, &rows);
    assert_eq!(out[0].status, RowStatus::Warning);
    assert_eq!(out[0].resolved_contact_id, None);
    assert!(out[0].message.as_ref().unwrap().contains("new customer"));
}

#[test]
fn preview_flags_duplicate_invoice_number_as_error() {
    let mut w = world();
    let ctx = PostCtx::default();
    let contact_id = make_customer(&mut w, "Zenith Traders");
    ledger_core::engine::import_open_invoice(
        &mut w.conn, &w.company, &contact_id, "INV-1",
        "2026-03-01", "2026-03-15", "2026-03-01", 500_000_00, 200_000_00, &ctx,
    )
    .unwrap();
    let rows = vec![header(), row(&["INV-1", "Zenith Traders", "01/03/2026", "15/03/2026", "500000", "200000"])];
    let out = preview_open_invoices(&w.conn, &w.company, &rows);
    assert_eq!(out[0].status, RowStatus::Error);
    assert!(out[0].message.as_ref().unwrap().contains("already been imported"));
}

#[test]
fn preview_blocks_customer_with_existing_wizard_opening_line() {
    let mut w = world();
    let ctx = PostCtx::default();
    let contact_id = make_customer(&mut w, "Zenith Traders");
    let ar = sys_account(&w.conn, &w.company, "AR");
    let obe = sys_account(&w.conn, &w.company, "OPENING_BALANCE_EQUITY");
    post_journal(
        &mut w.conn, &w.company, "2026-01-01", "Opening balances at setup", "opening_balance", &ctx,
        &[LineSpec::with_contact(&ar, 1_100_000_00, &contact_id), LineSpec::new(&obe, -1_100_000_00)],
    )
    .unwrap();
    assert!(has_wizard_opening_line(&w.conn, &w.company, &contact_id, "AR").unwrap());

    let rows = vec![header(), row(&["INV-1", "Zenith Traders", "01/03/2026", "15/03/2026", "500000", "200000"])];
    let out = preview_open_invoices(&w.conn, &w.company, &rows);
    assert_eq!(out[0].status, RowStatus::Error);
    assert!(out[0].message.as_ref().unwrap().contains("opening balance recorded during setup"));
}

#[test]
fn commit_posts_dr_ar_cr_obe_and_creates_a_stub_for_an_unknown_customer() {
    let mut w = world();
    let rows = vec![header(), row(&["INV-1", "Brand New Customer", "01/03/2026", "15/03/2026", "500000", "200000"])];
    let preview = preview_open_invoices(&w.conn, &w.company, &rows);
    assert_eq!(preview[0].status, RowStatus::Warning);
    let result = commit_open_invoices(&mut w.conn, &w.company, "invoices.xlsx", preview, None).unwrap();
    assert_eq!(result.rows_ok, 1);
    assert_eq!(result.rows_total, 1);

    let (contact_id, total, paid, number): (String, i64, i64, String) = w
        .conn
        .query_row(
            "SELECT contact_id, total_kobo, amount_paid_kobo, number FROM invoices WHERE company_id = ?1",
            params![w.company],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(number, "INV-1");
    assert_eq!(total, 500_000_00);
    assert_eq!(paid, 300_000_00, "the paid portion is original total minus balance outstanding");

    let contact_name: String =
        w.conn.query_row("SELECT name FROM contacts WHERE id = ?1", params![contact_id], |r| r.get(0)).unwrap();
    assert_eq!(contact_name, "Brand New Customer");
    assert_eq!(trial_balance(&w.conn), 0);

    let batch_kind: String = w
        .conn
        .query_row("SELECT kind FROM import_batches WHERE id = ?1", params![result.batch_id], |r| r.get(0))
        .unwrap();
    assert_eq!(batch_kind, "open_invoices");
}

#[test]
fn commit_skips_error_rows_and_reports_them_as_exceptions() {
    let mut w = world();
    let rows = vec![
        header(),
        row(&["INV-1", "Zenith Traders", "01/03/2026", "15/03/2026", "500000", "200000"]),
        row(&["", "Nobody", "01/03/2026", "15/03/2026", "100000", "50000"]), // error: no invoice no.
    ];
    let preview = preview_open_invoices(&w.conn, &w.company, &rows);
    let result = commit_open_invoices(&mut w.conn, &w.company, "invoices.xlsx", preview, None).unwrap();
    assert_eq!(result.rows_total, 2);
    assert_eq!(result.rows_ok, 1);
    assert_eq!(result.rows_error, 1);
    assert_eq!(result.exceptions.len(), 1);
    assert_eq!(result.exceptions[0].status, RowStatus::Error);
}

fn make_customer(w: &mut W, name: &str) -> String {
    let id = ledger_core::ids::new_id();
    w.conn
        .execute(
            "INSERT INTO contacts (id, company_id, kind, name, created_at) VALUES (?1, ?2, 'customer', ?3, ?4)",
            params![id, w.company, name, ledger_core::ids::now_iso()],
        )
        .unwrap();
    id
}
