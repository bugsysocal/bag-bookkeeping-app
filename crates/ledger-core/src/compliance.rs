//! Spec 07 §2.3/§5 — Advisor-only company settings (tax flags, hard period
//! lock, write-off routing) and the Compliance banner's threshold-crossing
//! check + acknowledgment. No posting; the only schema this module owns is
//! the two ack columns from migration 0004.

use crate::engine::EngineError;
use crate::reports;
use rusqlite::{params, Connection};

type R<T> = Result<T, EngineError>;

/// Spec 02 §5.9 / Spec 07 capability table: tax flags, VAT rate, WHT presets
/// are Advisor-only settings, editable after setup (unlike at wizard time,
/// where they're derived from the turnover/assets/professional questions).
pub fn update_tax_settings(
    conn: &Connection, company_id: &str,
    vat_registered: bool, vat_exempt: bool, cit_exempt: bool, vat_rate_bp: i64,
) -> R<()> {
    if !(0..=10_000).contains(&vat_rate_bp) {
        return Err(EngineError::Validation("the VAT rate must be between 0% and 100%".into()));
    }
    conn.execute(
        "UPDATE companies SET vat_registered = ?2, vat_exempt = ?3, cit_exempt = ?4, vat_rate_bp = ?5 WHERE id = ?1",
        params![company_id, vat_registered as i64, vat_exempt as i64, cit_exempt as i64, vat_rate_bp],
    )?;
    Ok(())
}

/// Spec 01 §3.1/T5: the hard period lock. `None` clears it.
pub fn update_hard_close(conn: &Connection, company_id: &str, hard_close_through: Option<&str>) -> R<()> {
    conn.execute(
        "UPDATE companies SET hard_close_through = ?2 WHERE id = ?1",
        params![company_id, hard_close_through],
    )?;
    Ok(())
}

/// Spec 04 §7.5: the write-off limit and routing accounts, seeded once at
/// company creation and never editable since — the "write-off routing
/// settings" half of the Spec 07 §5 capability table row.
pub fn update_writeoff_settings(
    conn: &Connection, company_id: &str, limit_kobo: i64, debit_account_id: &str, credit_account_id: &str,
) -> R<()> {
    if limit_kobo < 0 {
        return Err(EngineError::Validation("the write-off limit can't be negative".into()));
    }
    conn.execute(
        "UPDATE companies SET writeoff_limit_kobo = ?2, writeoff_debit_account_id = ?3, writeoff_credit_account_id = ?4
         WHERE id = ?1",
        params![company_id, limit_kobo, debit_account_id, credit_account_id],
    )?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct ComplianceBanner {
    pub kind: &'static str, // "vat_threshold" | "cit_threshold"
    pub message: String,
    pub ytd_revenue_kobo: i64,
}

fn fy_start_for(conn: &Connection, company_id: &str, as_of: &str) -> R<String> {
    let fiscal_start_month: i64 =
        conn.query_row("SELECT fiscal_year_start_month FROM companies WHERE id = ?1", params![company_id], |r| r.get(0))?;
    Ok(reports::fiscal_year_start(fiscal_start_month as u32, as_of))
}

/// Spec 07 §2.3 banner slot #1 / Spec 02 §5.9: fires when live YTD revenue
/// has outgrown the small-business relief assumed at setup. Owner-visible,
/// non-editable; clearing it is Advisor Mode only (`ack_vat_threshold` /
/// `ack_cit_threshold`, or simply updating the flag via `update_tax_settings`,
/// which naturally stops the banner since it'll no longer contradict reality).
///
/// The CIT check is revenue-only. The statutory small-company test is
/// turnover <= N100M **and** fixed assets <= N250M (NTA 2025), but this app
/// has no fixed-assets ledger — there's nothing to check that leg against.
/// Documented here rather than silently assumed; an advisor still has to
/// apply judgment on the assets leg before touching `cit_exempt`.
pub fn compliance_banners(conn: &Connection, company_id: &str) -> R<Vec<ComplianceBanner>> {
    let (vat_exempt, cit_exempt, vat_ack, cit_ack): (bool, bool, Option<String>, Option<String>) = conn.query_row(
        "SELECT vat_exempt, cit_exempt, vat_threshold_acked_fy_start, cit_threshold_acked_fy_start
         FROM companies WHERE id = ?1",
        params![company_id],
        |r| Ok((r.get::<_, i64>(0)? != 0, r.get::<_, i64>(1)? != 0, r.get(2)?, r.get(3)?)),
    )?;
    if !vat_exempt && !cit_exempt {
        return Ok(vec![]);
    }
    let today = &crate::ids::now_iso()[0..10];
    let fy = fy_start_for(conn, company_id, today)?;
    let stmt = reports::income_statement_accrual(conn, company_id, &fy, today)?;
    let ytd = stmt.revenue_total_kobo;

    let mut out = Vec::new();
    if vat_exempt && ytd > 50_000_000_00 && vat_ack.as_deref() != Some(fy.as_str()) {
        out.push(ComplianceBanner {
            kind: "vat_threshold",
            message: "Sales this fiscal year have passed ₦50,000,000 — you may now need to register for VAT. Ask your advisor to review.".into(),
            ytd_revenue_kobo: ytd,
        });
    }
    if cit_exempt && ytd > 100_000_000_00 && cit_ack.as_deref() != Some(fy.as_str()) {
        out.push(ComplianceBanner {
            kind: "cit_threshold",
            message: "Sales this fiscal year have passed ₦100,000,000 — the small-company tax relief may no longer apply. Ask your advisor to review.".into(),
            ytd_revenue_kobo: ytd,
        });
    }
    Ok(out)
}

pub fn ack_vat_threshold(conn: &Connection, company_id: &str) -> R<()> {
    let fy = fy_start_for(conn, company_id, &crate::ids::now_iso()[0..10])?;
    conn.execute("UPDATE companies SET vat_threshold_acked_fy_start = ?2 WHERE id = ?1", params![company_id, fy])?;
    Ok(())
}

pub fn ack_cit_threshold(conn: &Connection, company_id: &str) -> R<()> {
    let fy = fy_start_for(conn, company_id, &crate::ids::now_iso()[0..10])?;
    conn.execute("UPDATE companies SET cit_threshold_acked_fy_start = ?2 WHERE id = ?1", params![company_id, fy])?;
    Ok(())
}
