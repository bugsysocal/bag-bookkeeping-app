//! Compliance banner + advisor settings tests (Spec 07 §2.3/§5).

use ledger_core::compliance::{
    ack_cit_threshold, ack_vat_threshold, compliance_banners, update_hard_close, update_tax_settings,
    update_writeoff_settings,
};
use ledger_core::engine::PostCtx;
use ledger_core::ids::now_iso;
use ledger_core::rusqlite::{params, Connection};
use ledger_core::seed::{create_company, CompanyConfig};
use ledger_core::LineSpec;

struct W {
    conn: Connection,
    company: String,
}

fn world(vat_exempt: bool, cit_exempt: bool) -> W {
    let mut conn = ledger_core::open(":memory:").unwrap();
    let cfg = CompanyConfig { vat_exempt, cit_exempt, ..CompanyConfig::default() };
    let company = create_company(&mut conn, &cfg).unwrap();
    W { conn, company }
}

fn post_revenue(w: &mut W, naira_kobo: i64) {
    let ctx = PostCtx::default();
    let ar: String = w
        .conn
        .query_row("SELECT id FROM accounts WHERE company_id = ?1 AND system_key = 'AR'", params![w.company], |r| r.get(0))
        .unwrap();
    let sales: String = w
        .conn
        .query_row("SELECT id FROM accounts WHERE company_id = ?1 AND system_key = 'SALES_DEFAULT'", params![w.company], |r| r.get(0))
        .unwrap();
    let customer_id = ledger_core::ids::new_id();
    w.conn
        .execute(
            "INSERT INTO contacts (id, company_id, kind, name, created_at) VALUES (?1, ?2, 'customer', 'Test Customer', ?3)",
            params![customer_id, w.company, now_iso()],
        )
        .unwrap();
    let today = &now_iso()[0..10];
    ledger_core::engine::post_journal(
        &mut w.conn, &w.company, today, "test revenue", "manual", &ctx,
        &[LineSpec::with_contact(&ar, naira_kobo, &customer_id), LineSpec::new(&sales, -naira_kobo)],
    )
    .unwrap();
}

#[test]
fn no_banners_when_neither_relief_flag_is_set() {
    let mut w = world(false, false);
    post_revenue(&mut w, 200_000_000_00); // even with huge revenue, nothing to cross
    let banners = compliance_banners(&w.conn, &w.company).unwrap();
    assert!(banners.is_empty());
}

#[test]
fn vat_banner_fires_once_ytd_revenue_passes_50m() {
    let mut w = world(true, false);
    post_revenue(&mut w, 60_000_000_00);
    let banners = compliance_banners(&w.conn, &w.company).unwrap();
    assert_eq!(banners.len(), 1);
    assert_eq!(banners[0].kind, "vat_threshold");
    assert_eq!(banners[0].ytd_revenue_kobo, 60_000_000_00);
}

#[test]
fn no_vat_banner_below_the_threshold() {
    let mut w = world(true, false);
    post_revenue(&mut w, 10_000_000_00);
    assert!(compliance_banners(&w.conn, &w.company).unwrap().is_empty());
}

#[test]
fn cit_banner_fires_independently_of_vat() {
    let mut w = world(false, true);
    post_revenue(&mut w, 150_000_000_00);
    let banners = compliance_banners(&w.conn, &w.company).unwrap();
    assert_eq!(banners.len(), 1);
    assert_eq!(banners[0].kind, "cit_threshold");
}

#[test]
fn both_banners_can_fire_together() {
    let mut w = world(true, true);
    post_revenue(&mut w, 150_000_000_00);
    let banners = compliance_banners(&w.conn, &w.company).unwrap();
    assert_eq!(banners.len(), 2);
}

#[test]
fn acknowledging_the_vat_threshold_suppresses_only_that_banner_this_fiscal_year() {
    let mut w = world(true, true);
    post_revenue(&mut w, 150_000_000_00);
    ack_vat_threshold(&w.conn, &w.company).unwrap();
    let banners = compliance_banners(&w.conn, &w.company).unwrap();
    assert_eq!(banners.len(), 1);
    assert_eq!(banners[0].kind, "cit_threshold");
}

#[test]
fn acknowledging_cit_leaves_vat_banner_alone() {
    let mut w = world(true, true);
    post_revenue(&mut w, 150_000_000_00);
    ack_cit_threshold(&w.conn, &w.company).unwrap();
    let banners = compliance_banners(&w.conn, &w.company).unwrap();
    assert_eq!(banners.len(), 1);
    assert_eq!(banners[0].kind, "vat_threshold");
}

#[test]
fn updating_the_vat_exempt_flag_stops_the_banner_without_a_separate_ack() {
    let mut w = world(true, false);
    post_revenue(&mut w, 60_000_000_00);
    assert_eq!(compliance_banners(&w.conn, &w.company).unwrap().len(), 1);
    update_tax_settings(&w.conn, &w.company, true, false, false, 750).unwrap();
    assert!(compliance_banners(&w.conn, &w.company).unwrap().is_empty());
}

#[test]
fn update_tax_settings_rejects_an_out_of_range_vat_rate() {
    let w = world(false, false);
    assert!(update_tax_settings(&w.conn, &w.company, true, false, false, 10_001).is_err());
    assert!(update_tax_settings(&w.conn, &w.company, true, false, false, -1).is_err());
}

#[test]
fn update_hard_close_persists_and_clears() {
    let w = world(false, false);
    update_hard_close(&w.conn, &w.company, Some("2026-06-30")).unwrap();
    let v: Option<String> = w.conn.query_row("SELECT hard_close_through FROM companies WHERE id = ?1", params![w.company], |r| r.get(0)).unwrap();
    assert_eq!(v.as_deref(), Some("2026-06-30"));
    update_hard_close(&w.conn, &w.company, None).unwrap();
    let v: Option<String> = w.conn.query_row("SELECT hard_close_through FROM companies WHERE id = ?1", params![w.company], |r| r.get(0)).unwrap();
    assert_eq!(v, None);
}

#[test]
fn update_writeoff_settings_persists_and_rejects_negative_limit() {
    let w = world(false, false);
    let acct: String = w.conn.query_row("SELECT id FROM accounts WHERE company_id = ?1 LIMIT 1", params![w.company], |r| r.get(0)).unwrap();
    update_writeoff_settings(&w.conn, &w.company, 10_000_00, &acct, &acct).unwrap();
    let limit: i64 = w.conn.query_row("SELECT writeoff_limit_kobo FROM companies WHERE id = ?1", params![w.company], |r| r.get(0)).unwrap();
    assert_eq!(limit, 10_000_00);
    assert!(update_writeoff_settings(&w.conn, &w.company, -1, &acct, &acct).is_err());
}
