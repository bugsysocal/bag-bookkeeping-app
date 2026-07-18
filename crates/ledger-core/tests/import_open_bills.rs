//! Open Bills onboarding importer tests (Spec 06 §3/§3.1 mirror on the AP
//! side): preview validation, WHT flag/rate parsing, supplier resolution, the
//! anti-double-count guard, and commit's Dr OBE / Cr AP posting.

use ledger_core::engine::{has_wizard_opening_line, post_journal, PostCtx};
use ledger_core::import_open_bills::{commit_open_bills, preview_open_bills, RowStatus};
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
    vec!["supplier", "reference", "bill_date", "due_date", "original_total", "balance_outstanding", "wht_applicable", "wht_rate"]
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

fn make_supplier(w: &mut W, name: &str) -> String {
    let id = ledger_core::ids::new_id();
    w.conn
        .execute(
            "INSERT INTO contacts (id, company_id, kind, name, created_at) VALUES (?1, ?2, 'supplier', ?3, ?4)",
            params![id, w.company, name, ledger_core::ids::now_iso()],
        )
        .unwrap();
    id
}

#[test]
fn preview_flags_missing_required_fields_as_errors() {
    let w = world();
    let rows = vec![
        header(),
        row(&["", "SUP-1", "01/03/2026", "31/03/2026", "300000", "300000", "no", ""]),
    ];
    let out = preview_open_bills(&w.conn, &w.company, &rows);
    assert_eq!(out[0].status, RowStatus::Error);
    assert!(out[0].message.as_ref().unwrap().contains("Supplier"));
}

#[test]
fn preview_rejects_balance_exceeding_original_total() {
    let w = world();
    let rows = vec![header(), row(&["Okoro Supplies", "SUP-1", "01/03/2026", "31/03/2026", "100000", "150000", "no", ""])];
    let out = preview_open_bills(&w.conn, &w.company, &rows);
    assert_eq!(out[0].status, RowStatus::Error);
    assert!(out[0].message.as_ref().unwrap().contains("Balance outstanding"));
}

#[test]
fn preview_requires_wht_rate_when_wht_applies() {
    let w = world();
    let rows = vec![header(), row(&["Okoro Supplies", "SUP-1", "01/03/2026", "31/03/2026", "300000", "300000", "yes", ""])];
    let out = preview_open_bills(&w.conn, &w.company, &rows);
    assert_eq!(out[0].status, RowStatus::Error);
    assert!(out[0].message.as_ref().unwrap().contains("WHT rate"));
}

#[test]
fn preview_parses_wht_rate_percentage_into_basis_points() {
    let w = world();
    let rows = vec![header(), row(&["Okoro Supplies", "SUP-1", "01/03/2026", "31/03/2026", "300000", "300000", "yes", "5"])];
    let out = preview_open_bills(&w.conn, &w.company, &rows);
    assert_eq!(out[0].wht_applicable, true);
    assert_eq!(out[0].wht_rate_bp, Some(500));
}

#[test]
fn preview_resolves_exact_supplier_match_as_ready() {
    let mut w = world();
    make_supplier(&mut w, "Okoro Supplies");
    let rows = vec![header(), row(&["Okoro Supplies", "SUP-1", "01/03/2026", "31/03/2026", "300000", "300000", "no", ""])];
    let out = preview_open_bills(&w.conn, &w.company, &rows);
    assert_eq!(out[0].status, RowStatus::Ready);
    assert!(out[0].resolved_contact_id.is_some());
    assert_eq!(out[0].bill_date, "2026-03-01");
}

#[test]
fn preview_warns_and_leaves_unresolved_for_unknown_supplier() {
    let w = world();
    let rows = vec![header(), row(&["Brand New Supplier", "SUP-1", "01/03/2026", "31/03/2026", "300000", "300000", "no", ""])];
    let out = preview_open_bills(&w.conn, &w.company, &rows);
    assert_eq!(out[0].status, RowStatus::Warning);
    assert_eq!(out[0].resolved_contact_id, None);
    assert!(out[0].message.as_ref().unwrap().contains("new supplier"));
}

#[test]
fn preview_flags_duplicate_supplier_ref_and_date_as_error() {
    let mut w = world();
    let ctx = PostCtx::default();
    let contact_id = make_supplier(&mut w, "Okoro Supplies");
    ledger_core::engine::import_open_bill(
        &mut w.conn, &w.company, &contact_id, Some("SUP-1"), "2026-03-01", "2026-03-31",
        "2026-03-01", 300_000_00, 300_000_00, false, None, &ctx,
    )
    .unwrap();
    let rows = vec![header(), row(&["Okoro Supplies", "SUP-1", "01/03/2026", "31/03/2026", "300000", "300000", "no", ""])];
    let out = preview_open_bills(&w.conn, &w.company, &rows);
    assert_eq!(out[0].status, RowStatus::Error);
    assert!(out[0].message.as_ref().unwrap().contains("already been imported"));
}

#[test]
fn preview_blocks_supplier_with_existing_wizard_opening_line() {
    let mut w = world();
    let ctx = PostCtx::default();
    let contact_id = make_supplier(&mut w, "Okoro Supplies");
    let ap = sys_account(&w.conn, &w.company, "AP");
    let obe = sys_account(&w.conn, &w.company, "OPENING_BALANCE_EQUITY");
    post_journal(
        &mut w.conn, &w.company, "2026-01-01", "Opening balances at setup", "opening_balance", &ctx,
        &[LineSpec::with_contact(&ap, -900_000_00, &contact_id), LineSpec::new(&obe, 900_000_00)],
    )
    .unwrap();
    assert!(has_wizard_opening_line(&w.conn, &w.company, &contact_id, "AP").unwrap());

    let rows = vec![header(), row(&["Okoro Supplies", "SUP-1", "01/03/2026", "31/03/2026", "300000", "300000", "no", ""])];
    let out = preview_open_bills(&w.conn, &w.company, &rows);
    assert_eq!(out[0].status, RowStatus::Error);
    assert!(out[0].message.as_ref().unwrap().contains("opening balance recorded during setup"));
}

#[test]
fn commit_posts_dr_obe_cr_ap_and_creates_a_stub_for_an_unknown_supplier() {
    let mut w = world();
    let rows = vec![header(), row(&["Brand New Supplier", "SUP-1", "01/03/2026", "31/03/2026", "300000", "300000", "no", ""])];
    let preview = preview_open_bills(&w.conn, &w.company, &rows);
    assert_eq!(preview[0].status, RowStatus::Warning);
    let result = commit_open_bills(&mut w.conn, &w.company, "bills.xlsx", preview, None).unwrap();
    assert_eq!(result.rows_ok, 1);

    let (contact_id, total, paid): (String, i64, i64) = w
        .conn
        .query_row(
            "SELECT contact_id, total_kobo, amount_paid_kobo FROM bills WHERE company_id = ?1",
            params![w.company],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(total, 300_000_00);
    assert_eq!(paid, 0);

    let contact_name: String =
        w.conn.query_row("SELECT name FROM contacts WHERE id = ?1", params![contact_id], |r| r.get(0)).unwrap();
    assert_eq!(contact_name, "Brand New Supplier");
    assert_eq!(trial_balance(&w.conn), 0);

    let batch_kind: String = w
        .conn
        .query_row("SELECT kind FROM import_batches WHERE id = ?1", params![result.batch_id], |r| r.get(0))
        .unwrap();
    assert_eq!(batch_kind, "open_bills");
}

#[test]
fn commit_skips_error_rows_and_reports_them_as_exceptions() {
    let mut w = world();
    let rows = vec![
        header(),
        row(&["Okoro Supplies", "SUP-1", "01/03/2026", "31/03/2026", "300000", "300000", "no", ""]),
        row(&["", "SUP-2", "01/03/2026", "31/03/2026", "100000", "50000", "no", ""]), // error: no supplier
    ];
    let preview = preview_open_bills(&w.conn, &w.company, &rows);
    let result = commit_open_bills(&mut w.conn, &w.company, "bills.xlsx", preview, None).unwrap();
    assert_eq!(result.rows_total, 2);
    assert_eq!(result.rows_ok, 1);
    assert_eq!(result.rows_error, 1);
    assert_eq!(result.exceptions.len(), 1);
}
