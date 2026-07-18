//! Products onboarding importer tests (Spec 06 §3): preview validation,
//! the name/SKU dedup warning, and commit's opening-stock posting
//! (only when the company tracks inventory and a quantity is given).

use ledger_core::import_products::{commit_products, preview_products, RowStatus};
use ledger_core::rusqlite::{params, Connection};
use ledger_core::seed::{create_company, CompanyConfig};

struct W {
    conn: Connection,
    company: String,
}

fn world(inventory_enabled: bool) -> W {
    let mut conn = ledger_core::open(":memory:").unwrap();
    let cfg = CompanyConfig { inventory_enabled, ..CompanyConfig::default() };
    let company = create_company(&mut conn, &cfg).unwrap();
    W { conn, company }
}

fn header() -> Vec<String> {
    vec!["name", "kind", "sku", "sale_price", "is_vatable", "qty_on_hand", "unit_cost"]
        .into_iter()
        .map(String::from)
        .collect()
}

fn row(cells: &[&str]) -> Vec<String> {
    cells.iter().map(|s| s.to_string()).collect()
}

fn trial_balance(conn: &Connection) -> i64 {
    conn.query_row("SELECT COALESCE(SUM(amount_kobo), 0) FROM journal_lines", [], |r| r.get(0))
        .unwrap()
}

#[test]
fn preview_flags_missing_name_and_bad_kind_as_errors() {
    let w = world(false);
    let rows = vec![
        header(),
        row(&["", "product", "", "", "", "", ""]),
        row(&["Widget", "gadget", "", "", "", "", ""]),
    ];
    let out = preview_products(&w.conn, &w.company, false, &rows);
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].status, RowStatus::Error);
    assert_eq!(out[0].message.as_deref(), Some("Name is required"));
    assert_eq!(out[1].status, RowStatus::Error);
    assert!(out[1].message.as_ref().unwrap().contains("Unrecognized kind"));
}

#[test]
fn preview_accepts_a_clean_row_with_defaults() {
    let w = world(false);
    let rows = vec![header(), row(&["Widget", "", "SKU1", "500", "", "", ""])];
    let out = preview_products(&w.conn, &w.company, false, &rows);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].status, RowStatus::Ready);
    assert_eq!(out[0].kind, "product", "blank kind defaults to product");
    assert!(out[0].is_vatable, "blank vatable defaults to yes");
    assert_eq!(out[0].sale_price_kobo, 500_00);
}

#[test]
fn preview_rejects_qty_without_unit_cost_when_inventory_enabled() {
    let w = world(true);
    let rows = vec![header(), row(&["Widget", "product", "SKU1", "500", "yes", "10", ""])];
    let out = preview_products(&w.conn, &w.company, true, &rows);
    assert_eq!(out[0].status, RowStatus::Error);
    assert!(out[0].message.as_ref().unwrap().contains("unit cost"));
}

#[test]
fn preview_warns_on_name_or_sku_duplicate_of_existing_product() {
    let w = world(false);
    w.conn
        .execute(
            "INSERT INTO products (id, company_id, kind, name, sku, sale_price_kobo, is_vatable, is_active)
             VALUES ('p1', ?1, 'product', 'Widget', 'SKU1', 500, 1, 1)",
            params![w.company],
        )
        .unwrap();
    let rows = vec![header(), row(&["widget", "product", "SKU9", "", "", "", ""])];
    let out = preview_products(&w.conn, &w.company, false, &rows);
    assert_eq!(out[0].status, RowStatus::Warning, "name matches, even though SKU differs");

    let rows2 = vec![header(), row(&["Gadget", "product", "sku1", "", "", "", ""])];
    let out2 = preview_products(&w.conn, &w.company, false, &rows2);
    assert_eq!(out2[0].status, RowStatus::Warning, "SKU matches, even though name differs");
}

#[test]
fn commit_writes_ready_rows_and_skips_exceptions_no_inventory() {
    let mut w = world(false);
    let rows = vec![
        header(),
        row(&["Widget", "product", "SKU1", "500", "yes", "", ""]),
        row(&["", "product", "", "", "", "", ""]), // error: no name
    ];
    let preview = preview_products(&w.conn, &w.company, false, &rows);
    let result = commit_products(&mut w.conn, &w.company, "products.xlsx", "2026-01-01", false, preview, None).unwrap();
    assert_eq!(result.rows_total, 2);
    assert_eq!(result.rows_ok, 1);
    assert_eq!(result.rows_error, 1);

    let count: i64 = w
        .conn
        .query_row("SELECT COUNT(*) FROM products WHERE company_id = ?1", params![w.company], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);

    let batch_kind: String = w
        .conn
        .query_row("SELECT kind FROM import_batches WHERE id = ?1", params![result.batch_id], |r| r.get(0))
        .unwrap();
    assert_eq!(batch_kind, "products");
    assert_eq!(trial_balance(&w.conn), 0, "no inventory posting when the company doesn't track it");
}

#[test]
fn commit_posts_opening_stock_dr_inventory_cr_obe_when_tracked() {
    let mut w = world(true);
    let rows = vec![header(), row(&["Widget", "product", "SKU1", "500", "yes", "10", "300"])];
    let preview = preview_products(&w.conn, &w.company, true, &rows);
    assert_eq!(preview[0].status, RowStatus::Ready);
    let result = commit_products(&mut w.conn, &w.company, "products.xlsx", "2026-01-01", true, preview, None).unwrap();
    assert_eq!(result.rows_ok, 1);

    let product_id: String = w
        .conn
        .query_row(
            "SELECT id FROM products WHERE company_id = ?1 AND name = 'Widget'",
            params![w.company],
            |r| r.get(0),
        )
        .unwrap();
    let track_inventory: i64 = w
        .conn
        .query_row("SELECT track_inventory FROM products WHERE id = ?1", params![product_id], |r| r.get(0))
        .unwrap();
    assert_eq!(track_inventory, 1);

    let (qty, unit_cost, total_cost): (i64, i64, i64) = w
        .conn
        .query_row(
            "SELECT quantity_milli, unit_cost_kobo, total_cost_kobo FROM inventory_movements WHERE product_id = ?1",
            params![product_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(qty, 10_000, "10 units in milli-units");
    assert_eq!(unit_cost, 300_00);
    assert_eq!(total_cost, 3_000_00);

    assert_eq!(trial_balance(&w.conn), 0, "opening stock entry must balance");

    let inv_line: i64 = w
        .conn
        .query_row(
            "SELECT jl.amount_kobo FROM journal_lines jl
             JOIN accounts a ON a.id = jl.account_id
             WHERE a.system_key = 'INVENTORY' AND a.company_id = ?1
             ORDER BY jl.rowid DESC LIMIT 1",
            params![w.company],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(inv_line, 3_000_00, "inventory account is debited for the opening stock value");
}

#[test]
fn commit_skips_duplicate_warning_rows_without_creating_a_second_product() {
    let mut w = world(false);
    w.conn
        .execute(
            "INSERT INTO products (id, company_id, kind, name, sku, sale_price_kobo, is_vatable, is_active)
             VALUES ('p1', ?1, 'product', 'Widget', 'SKU1', 500, 1, 1)",
            params![w.company],
        )
        .unwrap();
    let rows = vec![header(), row(&["Widget", "product", "SKU1", "500", "yes", "", ""])];
    let preview = preview_products(&w.conn, &w.company, false, &rows);
    assert_eq!(preview[0].status, RowStatus::Warning);
    let result = commit_products(&mut w.conn, &w.company, "products.xlsx", "2026-01-01", false, preview, None).unwrap();
    assert_eq!(result.rows_ok, 0);
    assert_eq!(result.exceptions.len(), 1);

    let count: i64 = w
        .conn
        .query_row("SELECT COUNT(*) FROM products WHERE company_id = ?1", params![w.company], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1, "the duplicate must not create a second product row");
}
