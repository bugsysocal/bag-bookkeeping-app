//! Contacts onboarding importer tests (Spec 06 §3): preview validation,
//! the name+phone dedup warning, and commit's skip-bad-rows discipline.

use ledger_core::import_contacts::{commit_contacts, preview_contacts, RowStatus};
use ledger_core::rusqlite::{params, Connection};
use ledger_core::seed::{create_company, CompanyConfig};

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
    vec!["name", "kind", "phone", "email", "tin", "address", "terms_days"]
        .into_iter()
        .map(String::from)
        .collect()
}

fn row(cells: &[&str]) -> Vec<String> {
    cells.iter().map(|s| s.to_string()).collect()
}

#[test]
fn preview_flags_missing_name_and_bad_kind_as_errors() {
    let w = world();
    let rows = vec![
        header(),
        row(&["", "customer", "", "", "", "", ""]),
        row(&["Jide Ent.", "vendor", "", "", "", "", ""]),
        row(&["Ada Traders", "", "", "", "", "", ""]),
    ];
    let out = preview_contacts(&w.conn, &w.company, &rows);
    assert_eq!(out.len(), 3);
    assert_eq!(out[0].status, RowStatus::Error);
    assert_eq!(out[0].message.as_deref(), Some("Name is required"));
    assert_eq!(out[1].status, RowStatus::Error);
    assert!(out[1].message.as_ref().unwrap().contains("Unrecognized kind"));
    assert_eq!(out[2].status, RowStatus::Error);
    assert!(out[2].message.as_ref().unwrap().contains("Kind is required"));
}

#[test]
fn preview_accepts_a_clean_row_with_defaults() {
    let w = world();
    let rows = vec![header(), row(&["Ada Traders", "customer", "0803", "ada@x.com", "TIN1", "Lagos", "30"])];
    let out = preview_contacts(&w.conn, &w.company, &rows);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].status, RowStatus::Ready);
    assert_eq!(out[0].terms_days, 30);
    assert_eq!(out[0].kind, "customer");
}

#[test]
fn preview_warns_on_name_phone_duplicate_of_existing_contact() {
    let w = world();
    w.conn
        .execute(
            "INSERT INTO contacts (id, company_id, kind, name, phone, created_at)
             VALUES ('c1', ?1, 'customer', 'Ada Traders', '0803', '2026-01-01T00:00:00Z')",
            params![w.company],
        )
        .unwrap();
    let rows = vec![header(), row(&["ada traders", "customer", "0803", "", "", "", ""])];
    let out = preview_contacts(&w.conn, &w.company, &rows);
    assert_eq!(out[0].status, RowStatus::Warning);
    assert!(out[0].message.as_ref().unwrap().contains("already exists"));
}

#[test]
fn commit_writes_only_ready_rows_and_returns_exceptions() {
    let mut w = world();
    let rows = vec![
        header(),
        row(&["Ada Traders", "customer", "0803", "", "", "", ""]),
        row(&["", "customer", "", "", "", "", ""]), // error: no name
        row(&["Jide Ent.", "supplier", "", "", "", "", ""]),
    ];
    let preview = preview_contacts(&w.conn, &w.company, &rows);
    assert_eq!(preview.len(), 3);
    let result = commit_contacts(&mut w.conn, &w.company, "contacts.xlsx", preview, None).unwrap();
    assert_eq!(result.rows_total, 3);
    assert_eq!(result.rows_ok, 2);
    assert_eq!(result.rows_error, 1);
    assert_eq!(result.exceptions.len(), 1);
    assert_eq!(result.exceptions[0].status, RowStatus::Error);

    let count: i64 = w
        .conn
        .query_row("SELECT COUNT(*) FROM contacts WHERE company_id = ?1", params![w.company], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 2);

    let batch_kind: String = w
        .conn
        .query_row(
            "SELECT kind FROM import_batches WHERE id = ?1",
            params![result.batch_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(batch_kind, "contacts");
}

#[test]
fn commit_skips_duplicate_warning_rows_without_creating_a_second_contact() {
    let mut w = world();
    w.conn
        .execute(
            "INSERT INTO contacts (id, company_id, kind, name, phone, created_at)
             VALUES ('c1', ?1, 'customer', 'Ada Traders', '0803', '2026-01-01T00:00:00Z')",
            params![w.company],
        )
        .unwrap();
    let rows = vec![header(), row(&["Ada Traders", "customer", "0803", "", "", "", ""])];
    let preview = preview_contacts(&w.conn, &w.company, &rows);
    assert_eq!(preview[0].status, RowStatus::Warning);
    let result = commit_contacts(&mut w.conn, &w.company, "contacts.xlsx", preview, None).unwrap();
    assert_eq!(result.rows_ok, 0);
    assert_eq!(result.exceptions.len(), 1);

    let count: i64 = w
        .conn
        .query_row("SELECT COUNT(*) FROM contacts WHERE company_id = ?1", params![w.company], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1, "the duplicate must not create a second contact row");
}
