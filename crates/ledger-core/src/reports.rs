//! Statements & reports (Spec 05). **No schema, no engine surface, by design**
//! (spec §7 "Deltas: None") — every function here is a pure, read-only query
//! over `journal_lines` and the document tables. R1: auto-generated, never
//! assembled; nothing here stores a number, so nothing here can drift.
//!
//! Sign convention reminder (Spec 01 §2): `amount_kobo` is +debit/−credit.
//! Assets are displayed as their raw signed sum (positive when in their
//! normal debit state). Liabilities and equity are displayed **negated**
//! (`-raw`) so a normal credit balance reads as a positive number — the
//! usual T-account display flip. Every function that mixes the two is
//! explicit about which convention its output uses.

use crate::engine::EngineError;
use rusqlite::{params, Connection};

type R<T> = Result<T, EngineError>;

// ===== §R2 — fiscal-aware period helpers (pure string/integer date math; no chrono) =====

fn ymd(date: &str) -> (i64, u32, u32) {
    let y: i64 = date[0..4].parse().unwrap();
    let m: u32 = date[5..7].parse().unwrap();
    let d: u32 = date[8..10].parse().unwrap();
    (y, m, d)
}
fn fmt(y: i64, m: u32, d: u32) -> String {
    format!("{y:04}-{m:02}-{d:02}")
}
fn days_in_month(y: i64, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 { 29 } else { 28 },
        _ => unreachable!(),
    }
}
/// Add (possibly negative) whole months to a Y/M pair, clamping the day.
fn add_months(y: i64, m: u32, delta: i64) -> (i64, u32) {
    let idx = (y * 12 + m as i64 - 1) + delta;
    (idx.div_euclid(12), (idx.rem_euclid(12) + 1) as u32)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeriodRange {
    pub start: String, // inclusive
    pub end: String,   // inclusive
}

/// A plain calendar month, regardless of fiscal year (used for VAT — Spec 05 §5.1:
/// FIRS filing is calendar-monthly no matter the business's fiscal year).
pub fn calendar_month_range(year: i64, month: u32) -> PeriodRange {
    let end_d = days_in_month(year, month);
    PeriodRange { start: fmt(year, month, 1), end: fmt(year, month, end_d) }
}

/// The fiscal year's first day for the year that CONTAINS `date`.
pub fn fiscal_year_start(fiscal_start_month: u32, date: &str) -> String {
    let (y, m, _d) = ymd(date);
    if m >= fiscal_start_month {
        fmt(y, fiscal_start_month, 1)
    } else {
        fmt(y - 1, fiscal_start_month, 1)
    }
}

/// Year-to-date: from this fiscal year's start through `as_of` (inclusive).
pub fn ytd_range(fiscal_start_month: u32, as_of: &str) -> PeriodRange {
    PeriodRange { start: fiscal_year_start(fiscal_start_month, as_of), end: as_of.to_string() }
}

/// The fiscal quarter (1..=4) containing `as_of`.
pub fn fiscal_quarter_range(fiscal_start_month: u32, as_of: &str) -> PeriodRange {
    let fys = fiscal_year_start(fiscal_start_month, as_of);
    let (fy, fm, _) = ymd(&fys);
    let (ay, am, _) = ymd(as_of);
    let months_in = ((ay - fy) * 12 + am as i64 - fm as i64).rem_euclid(12);
    let quarter_idx = months_in / 3; // 0..=3
    let (qy, qm) = add_months(fy, fm, quarter_idx * 3);
    let (ey, em) = add_months(qy, qm, 2);
    PeriodRange { start: fmt(qy, qm, 1), end: fmt(ey, em, days_in_month(ey, em)) }
}

// ===== Company config helpers =====

fn fiscal_start_month_of(conn: &Connection, company_id: &str) -> R<u32> {
    Ok(conn.query_row(
        "SELECT fiscal_year_start_month FROM companies WHERE id = ?1",
        params![company_id], |r| r.get::<_, i64>(0),
    )? as u32)
}

// ===== §3.2 — Aging (AR / AP), deposits shown separately, never netted =====

#[derive(Debug, Clone, Default)]
pub struct AgingBuckets {
    pub current_kobo: i64,
    pub d1_30_kobo: i64,
    pub d31_60_kobo: i64,
    pub d61_90_kobo: i64,
    pub d90_plus_kobo: i64,
}
impl AgingBuckets {
    pub fn total(&self) -> i64 {
        self.current_kobo + self.d1_30_kobo + self.d31_60_kobo + self.d61_90_kobo + self.d90_plus_kobo
    }
}

#[derive(Debug, Clone)]
pub struct AgingRow {
    pub contact_id: String,
    pub contact_name: String,
    pub buckets: AgingBuckets,
    pub deposit_kobo: i64, // shown alongside, NEVER subtracted from buckets (Spec 05 §3.2 / decision #4)
}

/// Days-from-civil (Howard Hinnant), the same proleptic calendar throughout
/// the crate, no chrono. `pub(crate)` since `backup.rs` needs it too for
/// retention-ladder day/week bucketing; delegates to `ids::days_from_civil`
/// (the canonical implementation — `ids.rs` also needs the inverse,
/// `civil_from_days`, so the pair lives there) rather than duplicating it.
pub(crate) fn to_days(date: &str) -> i64 {
    let (y, m, d) = ymd(date);
    crate::ids::days_from_civil(y, m, d)
}

fn days_between(as_of: &str, due: &str) -> i64 {
    to_days(as_of) - to_days(due)
}

fn bucket_of(days_late: i64) -> u8 {
    if days_late <= 0 { 0 } else if days_late <= 30 { 1 } else if days_late <= 60 { 2 } else if days_late <= 90 { 3 } else { 4 }
}

fn aging(conn: &Connection, company_id: &str, as_of: &str, doc_table: &str, open_states: &[&str], is_ar: bool) -> R<Vec<AgingRow>> {
    let states = open_states.iter().map(|s| format!("'{s}'")).collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT c.id, c.name, d.due_date, d.total_kobo - d.amount_paid_kobo
         FROM {doc_table} d JOIN contacts c ON c.id = d.contact_id
         WHERE d.company_id = ?1 AND d.status IN ({states}) AND d.total_kobo > d.amount_paid_kobo
         ORDER BY c.name"
    );
    let mut q = conn.prepare(&sql)?;
    let rows: Vec<(String, String, String, i64)> = q
        .query_map(params![company_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<Result<_, _>>()?;
    drop(q);

    let mut by_contact: std::collections::BTreeMap<String, AgingRow> = Default::default();
    for (cid, cname, due, balance) in rows {
        let row = by_contact.entry(cid.clone()).or_insert_with(|| AgingRow {
            contact_id: cid.clone(), contact_name: cname, buckets: AgingBuckets::default(), deposit_kobo: 0,
        });
        match bucket_of(days_between(as_of, &due)) {
            0 => row.buckets.current_kobo += balance,
            1 => row.buckets.d1_30_kobo += balance,
            2 => row.buckets.d31_60_kobo += balance,
            3 => row.buckets.d61_90_kobo += balance,
            _ => row.buckets.d90_plus_kobo += balance,
        }
    }
    let mut out: Vec<AgingRow> = by_contact.into_values().collect();
    if is_ar {
        for row in &mut out {
            row.deposit_kobo = crate::engine::deposit_balance(conn, company_id, &row.contact_id)?;
        }
    }
    out.sort_by(|a, b| b.buckets.total().cmp(&a.buckets.total()));
    Ok(out)
}

/// Who owes me (Spec 05 §3.2). Deposits are informational — never netted.
pub fn aging_receivables(conn: &Connection, company_id: &str, as_of: &str) -> R<Vec<AgingRow>> {
    aging(conn, company_id, as_of, "invoices", &["sent", "partially_paid"], true)
}

/// Whom do I owe (Spec 05 §3.2).
pub fn aging_payables(conn: &Connection, company_id: &str, as_of: &str) -> R<Vec<AgingRow>> {
    aging(conn, company_id, as_of, "bills", &["open", "partially_paid"], false)
}

// ===== §3.4 — Sales by customer / by product =====

#[derive(Debug, Clone)]
pub struct SalesRow {
    pub label: String,
    pub qty_milli: i64,
    pub value_kobo: i64, // net of line discounts, ex-VAT
}

fn sales_by(conn: &Connection, company_id: &str, start: &str, end: &str, top_n: usize, by_product: bool) -> R<Vec<SalesRow>> {
    let group_expr = if by_product { "il.description" } else { "c.name" };
    let sql = format!(
        "SELECT {group_expr} AS label, SUM(il.quantity_milli), SUM(il.net_kobo)
         FROM invoice_lines il
         JOIN invoices i ON i.id = il.invoice_id
         JOIN contacts c ON c.id = i.contact_id
         WHERE i.company_id = ?1 AND i.kind = 'invoice' AND i.status != 'void'
           AND i.issue_date BETWEEN ?2 AND ?3
         GROUP BY label ORDER BY SUM(il.net_kobo) DESC"
    );
    let mut q = conn.prepare(&sql)?;
    let mut rows: Vec<SalesRow> = q
        .query_map(params![company_id, start, end], |r| {
            Ok(SalesRow { label: r.get(0)?, qty_milli: r.get(1)?, value_kobo: r.get(2)? })
        })?
        .collect::<Result<_, _>>()?;
    if top_n > 0 && rows.len() > top_n {
        let rest = rows.split_off(top_n);
        let rollup = SalesRow {
            label: format!("Everyone else ({} more)", rest.len()),
            qty_milli: rest.iter().map(|r| r.qty_milli).sum(),
            value_kobo: rest.iter().map(|r| r.value_kobo).sum(),
        };
        rows.push(rollup);
    }
    Ok(rows)
}

pub fn sales_by_customer(conn: &Connection, company_id: &str, start: &str, end: &str, top_n: usize) -> R<Vec<SalesRow>> {
    sales_by(conn, company_id, start, end, top_n, false)
}
pub fn sales_by_product(conn: &Connection, company_id: &str, start: &str, end: &str, top_n: usize) -> R<Vec<SalesRow>> {
    sales_by(conn, company_id, start, end, top_n, true)
}

/// Owner-tier "Profit this month" note line (Spec 05 §3.3): drawings taken in
/// the period, surfaced for honesty, excluded from profit, guilt-free wording
/// lives in the UI — this just supplies the number.
pub fn drawings_in_range(conn: &Connection, company_id: &str, start: &str, end: &str) -> R<i64> {
    Ok(conn.query_row(
        "SELECT COALESCE(SUM(l.amount_kobo), 0)
         FROM journal_lines l JOIN journal_entries e ON e.id = l.entry_id
         JOIN accounts a ON a.id = l.account_id
         WHERE e.company_id = ?1 AND e.is_posted = 1 AND e.entry_date BETWEEN ?2 AND ?3
           AND a.system_key = 'OWNER_DRAWINGS'",
        params![company_id, start, end],
        |r| r.get(0),
    )?)
}

// ===== §4.1/§4.3 — profit-and-loss over a class, the shared primitive =====

/// `-SUM(amount_kobo)` over posted lines in `[start, end]` whose account is in
/// `classes`. Income is credit(−) so this yields the *amount*, not the raw
/// ledger sign; cogs/expense are debit(+) so subtracting them falls out of
/// the same negation. This one primitive is what makes "profit = -(P&L raw
/// sum)" exact, and is the basis for both the income statement and the
/// balance sheet's computed earnings (Spec 05 §4.3).
fn pnl_amount(conn: &Connection, company_id: &str, start: &str, end: &str, classes: &[&str]) -> R<i64> {
    let list = classes.iter().map(|c| format!("'{c}'")).collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT COALESCE(-SUM(l.amount_kobo), 0)
         FROM journal_lines l JOIN journal_entries e ON e.id = l.entry_id
         JOIN accounts a ON a.id = l.account_id
         WHERE e.company_id = ?1 AND e.is_posted = 1 AND e.entry_date BETWEEN ?2 AND ?3
           AND a.class IN ({list})"
    );
    Ok(conn.query_row(&sql, params![company_id, start, end], |r| r.get(0))?)
}

/// `negate`: income is credit-normal, so `-raw` shows a positive revenue
/// amount; cogs/expense are already debit-normal (positive raw) and must
/// NOT be flipped, or a real cost would display as a negative number.
fn by_account(conn: &Connection, company_id: &str, start: &str, end: &str, class: &str, exclude_system_key: Option<&str>, negate: bool) -> R<Vec<AccountAmount>> {
    let sign_expr = if negate { "-SUM(l.amount_kobo)" } else { "SUM(l.amount_kobo)" };
    let sql = format!(
        "SELECT a.id, a.code, a.name, {sign_expr}
         FROM journal_lines l JOIN journal_entries e ON e.id = l.entry_id
         JOIN accounts a ON a.id = l.account_id
         WHERE e.company_id = ?1 AND e.is_posted = 1 AND e.entry_date BETWEEN ?2 AND ?3
           AND a.class = ?4 AND (?5 IS NULL OR a.system_key IS NULL OR a.system_key != ?5)
         GROUP BY a.id HAVING SUM(l.amount_kobo) != 0
         ORDER BY a.code"
    );
    let mut q = conn.prepare(&sql)?;
    let rows = q.query_map(params![company_id, start, end, class, exclude_system_key], |r| {
        Ok(AccountAmount { account_id: r.get(0)?, code: r.get(1)?, name: r.get(2)?, amount_kobo: r.get(3)? })
    })?.collect::<Result<_, _>>()?;
    Ok(rows)
}

#[derive(Debug, Clone)]
pub struct AccountAmount { pub account_id: String, pub code: String, pub name: String, pub amount_kobo: i64 }

#[derive(Debug, Clone)]
pub struct IncomeStatement {
    pub revenue: Vec<AccountAmount>, pub revenue_total_kobo: i64,
    pub cogs: Vec<AccountAmount>, pub cogs_total_kobo: i64,
    pub gross_profit_kobo: i64, pub gross_margin_bp: i64, // basis points; 0 if no revenue
    pub opex: Vec<AccountAmount>, pub opex_total_kobo: i64,
    pub operating_profit_kobo: i64,
    pub non_operating_kobo: i64, // FX gain/loss (4900)
    pub net_profit_kobo: i64,
}

/// Accrual income statement (Spec 05 §4.1): Revenue (4xxx excl. FX gain/loss)
/// → COGS → Gross Profit + margin → OpEx → Operating Profit → non-operating
/// (FX gain/loss) → Net Profit.
pub fn income_statement_accrual(conn: &Connection, company_id: &str, start: &str, end: &str) -> R<IncomeStatement> {
    let revenue = by_account(conn, company_id, start, end, "income", Some("FX_GAIN_LOSS"), true)?;
    let revenue_total_kobo: i64 = revenue.iter().map(|a| a.amount_kobo).sum();
    let cogs = by_account(conn, company_id, start, end, "cogs", None, false)?;
    let cogs_total_kobo: i64 = cogs.iter().map(|a| a.amount_kobo).sum();
    let gross_profit_kobo = revenue_total_kobo - cogs_total_kobo;
    let gross_margin_bp = if revenue_total_kobo != 0 {
        crate::money::round_ratio(gross_profit_kobo as i128 * 10_000, revenue_total_kobo as i128)
    } else { 0 };
    let opex = by_account(conn, company_id, start, end, "expense", None, false)?;
    let opex_total_kobo: i64 = opex.iter().map(|a| a.amount_kobo).sum();
    let operating_profit_kobo = gross_profit_kobo - opex_total_kobo;
    let non_operating_kobo = conn.query_row(
        "SELECT COALESCE(-SUM(l.amount_kobo), 0)
         FROM journal_lines l JOIN journal_entries e ON e.id = l.entry_id
         JOIN accounts a ON a.id = l.account_id
         WHERE e.company_id = ?1 AND e.is_posted = 1 AND e.entry_date BETWEEN ?2 AND ?3
           AND a.system_key = 'FX_GAIN_LOSS'",
        params![company_id, start, end], |r| r.get(0),
    )?;
    Ok(IncomeStatement {
        revenue, revenue_total_kobo, cogs, cogs_total_kobo, gross_profit_kobo, gross_margin_bp,
        opex, opex_total_kobo, operating_profit_kobo, non_operating_kobo,
        net_profit_kobo: operating_profit_kobo + non_operating_kobo,
    })
}

#[derive(Debug, Clone)]
pub struct CashBasisSummary {
    pub revenue_kobo: i64,
    pub expense_kobo: i64,
    pub net_profit_kobo: i64,
    pub caption: String, // Decision #2's fixed caption, supplied so the UI can't drop it
}

/// Cash-basis P&L (Spec 05 §4.2, the normative algorithm):
/// - Revenue recognized at payment/deposit-application date, proportional to
///   `allocated × (invoice net ÷ invoice total)` — VAT excluded. Customer-
///   withheld WHT counts as collected (it's already inside the allocation
///   amount, Spec 01 §6.2). Unallocated deposits are not revenue until applied.
/// - Bill-funded expenses recognized at supplier-payment date, same
///   proportional ex-VAT logic. Quick expenses recognized at their own date
///   (already a cash event) — read straight off the accrual ledger since
///   there is no timing gap for them.
pub fn income_statement_cash_basis(conn: &Connection, company_id: &str, start: &str, end: &str) -> R<CashBasisSummary> {
    // Every proportional recognition below uses integer half-away-from-zero
    // rounding (money::round_ratio) — never floats near money, no exception
    // for reports (Spec 01 §2/§8 discipline applies here too).
    let rev_allocs: i64 = {
        let mut q = conn.prepare(
            "SELECT pa.amount_kobo, i.subtotal_kobo, i.total_kobo
             FROM payment_allocations pa
             JOIN payments p ON p.id = pa.payment_id
             JOIN invoices i ON i.id = pa.target_id AND pa.target_type = 'invoice'
             WHERE p.company_id = ?1 AND p.voided = 0 AND p.direction = 'in'
               AND p.payment_date BETWEEN ?2 AND ?3 AND i.total_kobo > 0",
        )?;
        let rows: Vec<(i64, i64, i64)> = q.query_map(params![company_id, start, end], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?.collect::<Result<_, _>>()?;
        rows.into_iter().map(|(alloc, subtotal, total)| {
            crate::money::round_ratio(alloc as i128 * subtotal as i128, total as i128)
        }).sum()
    };
    // Revenue via deposit applications (recognized on application date, not receipt).
    let rev_deposits: i64 = {
        let mut q = conn.prepare(
            "SELECT -l.amount_kobo, i.subtotal_kobo, i.total_kobo
             FROM journal_lines l
             JOIN journal_entries e ON e.id = l.entry_id
             JOIN accounts a ON a.id = l.account_id
             JOIN invoices i ON i.id = e.source_id
             WHERE e.company_id = ?1 AND e.is_posted = 1 AND e.source_type = 'deposit_application'
               AND a.system_key = 'AR' AND e.entry_date BETWEEN ?2 AND ?3 AND i.total_kobo > 0",
        )?;
        let rows: Vec<(i64, i64, i64)> = q.query_map(params![company_id, start, end], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?.collect::<Result<_, _>>()?;
        rows.into_iter().map(|(applied, subtotal, total)| {
            crate::money::round_ratio(applied as i128 * subtotal as i128, total as i128)
        }).sum()
    };
    // Bill-funded expenses via supplier-payment allocations.
    let bill_expenses: i64 = {
        let mut q = conn.prepare(
            "SELECT pa.amount_kobo, b.subtotal_kobo, b.total_kobo
             FROM payment_allocations pa
             JOIN payments p ON p.id = pa.payment_id
             JOIN bills b ON b.id = pa.target_id AND pa.target_type = 'bill'
             WHERE p.company_id = ?1 AND p.voided = 0 AND p.direction = 'out'
               AND p.payment_date BETWEEN ?2 AND ?3 AND b.total_kobo > 0",
        )?;
        let rows: Vec<(i64, i64, i64)> = q.query_map(params![company_id, start, end], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?.collect::<Result<_, _>>()?;
        rows.into_iter().map(|(alloc, subtotal, total)| {
            crate::money::round_ratio(alloc as i128 * subtotal as i128, total as i128)
        }).sum()
    };
    // Quick expenses: already cash-dated, so the accrual ledger IS the cash
    // basis for them — read the net expense (ex claimed VAT) directly.
    let quick_expenses: i64 = conn.query_row(
        "SELECT COALESCE(SUM(l.amount_kobo), 0)
         FROM journal_lines l JOIN journal_entries e ON e.id = l.entry_id
         JOIN accounts a ON a.id = l.account_id
         WHERE e.company_id = ?1 AND e.is_posted = 1 AND e.source_type = 'expense'
           AND a.class IN ('expense','cogs') AND e.entry_date BETWEEN ?2 AND ?3",
        params![company_id, start, end], |r| r.get(0),
    )?;
    let revenue_kobo = rev_allocs + rev_deposits;
    let expense_kobo = bill_expenses + quick_expenses;
    Ok(CashBasisSummary {
        revenue_kobo, expense_kobo, net_profit_kobo: revenue_kobo - expense_kobo,
        caption: "Cash basis — derived from settled amounts; VAT excluded.".to_string(),
    })
}

// ===== §4.3 — Balance Sheet (always balanced by construction) =====

#[derive(Debug, Clone)]
pub struct BalanceSheet {
    pub cash_and_bank_kobo: i64,
    pub receivables_kobo: i64,
    pub inventory_kobo: i64,
    pub fixed_assets_net_kobo: i64,
    pub other_assets_kobo: i64,
    pub total_assets_kobo: i64,
    pub current_liabilities_kobo: i64,
    pub loans_kobo: i64,
    pub other_liabilities_kobo: i64,
    pub total_liabilities_kobo: i64,
    pub opening_balance_equity_kobo: i64,
    pub owner_capital_kobo: i64,
    pub drawings_kobo: i64, // negative: a deduction from equity
    pub retained_earnings_kobo: i64,     // 3900 posted + all P&L before this fiscal year
    pub current_year_earnings_kobo: i64, // P&L within this fiscal year, through as_of
    pub total_equity_kobo: i64,
    pub ties: bool, // total_assets == total_liabilities + total_equity — always true; exposed for the UI/tests
}

fn class_sum(conn: &Connection, company_id: &str, as_of: &str, class: &str, negate: bool) -> R<i64> {
    let sign = if negate { -1 } else { 1 };
    let v: i64 = conn.query_row(
        "SELECT COALESCE(SUM(l.amount_kobo), 0)
         FROM journal_lines l JOIN journal_entries e ON e.id = l.entry_id
         JOIN accounts a ON a.id = l.account_id
         WHERE e.company_id = ?1 AND e.is_posted = 1 AND e.entry_date <= ?2 AND a.class = ?3",
        params![company_id, as_of, class], |r| r.get(0),
    )?;
    Ok(v * sign)
}

fn sum_where(conn: &Connection, company_id: &str, as_of: &str, extra_sql: &str, negate: bool) -> R<i64> {
    let sign = if negate { -1 } else { 1 };
    let sql = format!(
        "SELECT COALESCE(SUM(l.amount_kobo), 0)
         FROM journal_lines l JOIN journal_entries e ON e.id = l.entry_id
         JOIN accounts a ON a.id = l.account_id
         WHERE e.company_id = ?1 AND e.is_posted = 1 AND e.entry_date <= ?2 AND ({extra_sql})"
    );
    let v: i64 = conn.query_row(&sql, params![company_id, as_of], |r| r.get(0))?;
    Ok(v * sign)
}

/// Spec 05 §4.3. Filters everything by `entry_date <= as_of`, consistently,
/// which is exactly what makes `total_assets == total_liabilities + total_equity`
/// hold by construction (see the module-level derivation in the design notes /
/// `tests/reports.rs`) — it is not a coincidence and is asserted in tests.
pub fn balance_sheet(conn: &Connection, company_id: &str, as_of: &str) -> R<BalanceSheet> {
    let cash_and_bank_kobo = sum_where(conn, company_id, as_of, "a.is_bank = 1", false)?;
    let receivables_kobo = sum_where(
        conn, company_id, as_of,
        "a.system_key = 'AR' OR a.system_key = 'WHT_RECEIVABLE' OR a.code IN ('1400','1450')",
        false,
    )?;
    let inventory_kobo = sum_where(conn, company_id, as_of, "a.system_key = 'INVENTORY'", false)?;
    let fixed_assets_net_kobo = sum_where(conn, company_id, as_of, "a.code BETWEEN '1500' AND '1599'", false)?;
    let total_assets_kobo = class_sum(conn, company_id, as_of, "asset", false)?;
    let other_assets_kobo = total_assets_kobo - cash_and_bank_kobo - receivables_kobo - inventory_kobo - fixed_assets_net_kobo;

    let current_liabilities_kobo = sum_where(conn, company_id, as_of, "a.code BETWEEN '2100' AND '2399' OR a.code = '2500'", true)?;
    let loans_kobo = sum_where(conn, company_id, as_of, "a.code IN ('2410','2420')", true)?;
    let total_liabilities_kobo = class_sum(conn, company_id, as_of, "liability", true)?;
    let other_liabilities_kobo = total_liabilities_kobo - current_liabilities_kobo - loans_kobo;

    let opening_balance_equity_kobo = sum_where(conn, company_id, as_of, "a.system_key = 'OPENING_BALANCE_EQUITY'", true)?;
    let owner_capital_kobo = sum_where(conn, company_id, as_of, "a.system_key = 'OWNER_CAPITAL'", true)?;
    let drawings_kobo = sum_where(conn, company_id, as_of, "a.system_key = 'OWNER_DRAWINGS'", true)?;
    let posted_retained_kobo = sum_where(conn, company_id, as_of, "a.system_key = 'RETAINED_EARNINGS'", true)?;

    let fy_start = fiscal_year_start(fiscal_start_month_of(conn, company_id)?, as_of);
    let prior_pnl_kobo = pnl_amount(conn, company_id, "0000-01-01", &prev_day(&fy_start), &["income", "cogs", "expense"])?;
    let retained_earnings_kobo = posted_retained_kobo + prior_pnl_kobo;
    let current_year_earnings_kobo = pnl_amount(conn, company_id, &fy_start, as_of, &["income", "cogs", "expense"])?;

    let total_equity_kobo = opening_balance_equity_kobo + owner_capital_kobo + drawings_kobo
        + retained_earnings_kobo + current_year_earnings_kobo;

    Ok(BalanceSheet {
        cash_and_bank_kobo, receivables_kobo, inventory_kobo, fixed_assets_net_kobo, other_assets_kobo,
        total_assets_kobo,
        current_liabilities_kobo, loans_kobo, other_liabilities_kobo, total_liabilities_kobo,
        opening_balance_equity_kobo, owner_capital_kobo, drawings_kobo,
        retained_earnings_kobo, current_year_earnings_kobo, total_equity_kobo,
        ties: total_assets_kobo == total_liabilities_kobo + total_equity_kobo,
    })
}

fn prev_day(date: &str) -> String {
    let (y, m, d) = ymd(date);
    if d > 1 { return fmt(y, m, d - 1); }
    let (py, pm) = add_months(y, m, -1);
    fmt(py, pm, days_in_month(py, pm))
}

// ===== §4.4 — Cash Flow Statement (indirect; ties by construction) =====

#[derive(Debug, Clone)]
pub struct CashFlowStatement {
    pub operating_kobo: i64,
    pub investing_kobo: i64,
    pub financing_kobo: i64,
    pub net_change_kobo: i64,
    pub opening_cash_kobo: i64,
    pub closing_cash_kobo: i64,
    pub ties: bool,
}

/// Indirect method. Investing/Financing are literal balance-sheet deltas
/// (Δ fixed assets at cost; Δ loans + capital + drawings); Operating is
/// everything else, computed as the residual that makes the statement tie —
/// which is mathematically forced by the §2 map partitioning all non-bank
/// accounts, so "ties by construction" is a property of the computation, not
/// a hope (Spec 05 §2/§4.4).
pub fn cash_flow_statement(conn: &Connection, company_id: &str, start: &str, end: &str) -> R<CashFlowStatement> {
    let opening_cash_kobo = sum_where(conn, company_id, &prev_day(start), "a.is_bank = 1", false)?;
    let closing_cash_kobo = sum_where(conn, company_id, end, "a.is_bank = 1", false)?;
    let net_change_kobo = closing_cash_kobo - opening_cash_kobo;

    let delta_fixed = |as_of: &str| sum_where(conn, company_id, as_of, "a.code BETWEEN '1500' AND '1599'", false);
    let investing_kobo = -(delta_fixed(end)? - delta_fixed(&prev_day(start))?);

    let delta_fin = |as_of: &str| -> R<i64> {
        let loans = sum_where(conn, company_id, as_of, "a.code IN ('2410','2420')", true)?;
        let cap = sum_where(conn, company_id, as_of, "a.system_key = 'OWNER_CAPITAL'", true)?;
        let draw = sum_where(conn, company_id, as_of, "a.system_key = 'OWNER_DRAWINGS'", true)?;
        Ok(loans + cap + draw)
    };
    let financing_kobo = delta_fin(end)? - delta_fin(&prev_day(start))?;

    let operating_kobo = net_change_kobo - investing_kobo - financing_kobo;

    Ok(CashFlowStatement {
        operating_kobo, investing_kobo, financing_kobo, net_change_kobo,
        opening_cash_kobo, closing_cash_kobo,
        ties: opening_cash_kobo + net_change_kobo == closing_cash_kobo,
    })
}

// ===== §4.5 — Trial Balance & General Ledger (Advisor Mode) =====

#[derive(Debug, Clone)]
pub struct TrialBalanceRow { pub account_id: String, pub code: String, pub name: String, pub class: String, pub debit_kobo: i64, pub credit_kobo: i64 }

/// As-of trial balance, every account, Dr/Cr rendered from sign (Spec 01 §2).
/// Zero-balance rows are included; the UI applies the suppression toggle.
pub fn trial_balance(conn: &Connection, company_id: &str, as_of: &str) -> R<Vec<TrialBalanceRow>> {
    let mut q = conn.prepare(
        "SELECT a.id, a.code, a.name, a.class,
                COALESCE(SUM(l.amount_kobo), 0)
         FROM accounts a
         LEFT JOIN journal_lines l ON l.account_id = a.id
         LEFT JOIN journal_entries e ON e.id = l.entry_id AND e.is_posted = 1 AND e.entry_date <= ?2
         WHERE a.company_id = ?1
         GROUP BY a.id ORDER BY a.code",
    )?;
    let rows = q.query_map(params![company_id, as_of], |r| {
        let bal: i64 = r.get(4)?;
        Ok(TrialBalanceRow {
            account_id: r.get(0)?, code: r.get(1)?, name: r.get(2)?, class: r.get(3)?,
            debit_kobo: bal.max(0), credit_kobo: (-bal).max(0),
        })
    })?.collect::<Result<_, _>>()?;
    Ok(rows)
}

#[derive(Debug, Clone)]
pub struct GlLine { pub entry_id: String, pub date: String, pub memo: String, pub amount_kobo: i64, pub running_balance_kobo: i64, pub contact_name: Option<String> }

#[derive(Debug, Clone)]
pub struct GeneralLedger { pub opening_balance_kobo: i64, pub lines: Vec<GlLine>, pub closing_balance_kobo: i64 }

/// Per-account GL detail (Spec 05 §4.5), optionally filtered to one contact
/// (the AR/AP subledger view P8 paid for).
pub fn general_ledger(conn: &Connection, company_id: &str, account_id: &str, start: &str, end: &str, contact_id: Option<&str>) -> R<GeneralLedger> {
    let opening_balance_kobo: i64 = conn.query_row(
        "SELECT COALESCE(SUM(l.amount_kobo), 0)
         FROM journal_lines l JOIN journal_entries e ON e.id = l.entry_id
         WHERE l.account_id = ?1 AND e.company_id = ?2 AND e.is_posted = 1 AND e.entry_date < ?3
           AND (?4 IS NULL OR l.contact_id = ?4)",
        params![account_id, company_id, start, contact_id], |r| r.get(0),
    )?;
    let mut q = conn.prepare(
        "SELECT e.id, e.entry_date, e.memo, l.amount_kobo, c.name
         FROM journal_lines l JOIN journal_entries e ON e.id = l.entry_id
         LEFT JOIN contacts c ON c.id = l.contact_id
         WHERE l.account_id = ?1 AND e.company_id = ?2 AND e.is_posted = 1
           AND e.entry_date BETWEEN ?3 AND ?4 AND (?5 IS NULL OR l.contact_id = ?5)
         ORDER BY e.entry_date, e.created_at",
    )?;
    let raw: Vec<(String, String, String, i64, Option<String>)> = q
        .query_map(params![account_id, company_id, start, end, contact_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?.collect::<Result<_, _>>()?;
    let mut running = opening_balance_kobo;
    let lines = raw.into_iter().map(|(entry_id, date, memo, amount_kobo, contact_name)| {
        running += amount_kobo;
        GlLine { entry_id, date, memo, amount_kobo, running_balance_kobo: running, contact_name }
    }).collect();
    Ok(GeneralLedger { opening_balance_kobo, lines, closing_balance_kobo: running })
}

// ===== §5 — Tax reports =====

#[derive(Debug, Clone)]
pub struct VatLine { pub doc_number: String, pub party_name: String, pub tin: Option<String>, pub net_kobo: i64, pub vat_kobo: i64 }

#[derive(Debug, Clone)]
pub struct VatReport {
    pub output: Vec<VatLine>, pub output_vat_kobo: i64,
    pub input: Vec<VatLine>, pub input_vat_kobo: i64,
    pub net_payable_kobo: i64,           // this month only
    pub credit_brought_forward_kobo: i64, // cumulative, computed live — never stored
    pub net_due_kobo: i64,               // net_payable - credit b/f (negative = carried forward again)
}

/// Monthly VAT report (Spec 05 §5.1). Hidden entirely by the caller when the
/// company isn't VAT-registered. Credit brought forward is a live query over
/// ALL prior activity, never a stored balance (Spec 05 §5.1/§5.2 discipline).
pub fn vat_report(conn: &Connection, company_id: &str, month: &PeriodRange) -> R<VatReport> {
    let mut oq = conn.prepare(
        "SELECT i.number, c.name, c.tin, i.subtotal_kobo, i.vat_kobo
         FROM invoices i JOIN contacts c ON c.id = i.contact_id
         WHERE i.company_id = ?1 AND i.kind = 'invoice' AND i.status != 'void'
           AND i.issue_date BETWEEN ?2 AND ?3 AND i.vat_kobo > 0
         ORDER BY i.number",
    )?;
    let output: Vec<VatLine> = oq.query_map(params![company_id, month.start, month.end], |r| {
        Ok(VatLine { doc_number: r.get(0)?, party_name: r.get(1)?, tin: r.get(2)?, net_kobo: r.get(3)?, vat_kobo: r.get(4)? })
    })?.collect::<Result<_, _>>()?;
    drop(oq);
    let output_vat_kobo: i64 = output.iter().map(|l| l.vat_kobo).sum();

    let mut iq = conn.prepare(
        "SELECT COALESCE(b.reference, 'N/A'), c.name, c.tin,
                SUM(bl.net_kobo), SUM(CASE WHEN bl.vat_claimable THEN bl.vat_kobo ELSE 0 END)
         FROM bill_lines bl JOIN bills b ON b.id = bl.bill_id
         JOIN contacts c ON c.id = b.contact_id
         WHERE b.company_id = ?1 AND b.status != 'void' AND b.status != 'draft'
           AND b.bill_date BETWEEN ?2 AND ?3
         GROUP BY b.id HAVING SUM(CASE WHEN bl.vat_claimable THEN bl.vat_kobo ELSE 0 END) > 0
         ORDER BY b.bill_date",
    )?;
    let input: Vec<VatLine> = iq.query_map(params![company_id, month.start, month.end], |r| {
        Ok(VatLine { doc_number: r.get(0)?, party_name: r.get(1)?, tin: r.get(2)?, net_kobo: r.get(3)?, vat_kobo: r.get(4)? })
    })?.collect::<Result<_, _>>()?;
    drop(iq);
    let input_vat_kobo: i64 = input.iter().map(|l| l.vat_kobo).sum();

    let net_payable_kobo = output_vat_kobo - input_vat_kobo;
    // Cumulative net (2210 - 1310) up to the day before this month started = credit b/f.
    let cumulative_before: i64 = conn.query_row(
        "SELECT COALESCE(SUM(
            CASE WHEN a.system_key = 'VAT_OUTPUT' THEN -l.amount_kobo
                 WHEN a.system_key = 'VAT_INPUT' THEN l.amount_kobo ELSE 0 END
         ), 0)
         FROM journal_lines l JOIN journal_entries e ON e.id = l.entry_id
         JOIN accounts a ON a.id = l.account_id
         WHERE e.company_id = ?1 AND e.is_posted = 1 AND e.entry_date < ?2
           AND a.system_key IN ('VAT_OUTPUT','VAT_INPUT')",
        params![company_id, month.start], |r| r.get(0),
    )?;
    // A positive cumulative-before means net input > output historically = credit available.
    let credit_brought_forward_kobo = cumulative_before.max(0);
    Ok(VatReport {
        output, output_vat_kobo, input, input_vat_kobo, net_payable_kobo,
        credit_brought_forward_kobo, net_due_kobo: net_payable_kobo - credit_brought_forward_kobo,
    })
}

#[derive(Debug, Clone)]
pub struct WhtRemittanceLine { pub payment_date: String, pub supplier_name: String, pub tin: Option<String>, pub bill_ref: Option<String>, pub base_kobo: i64, pub wht_kobo: i64 }

/// What we withheld from suppliers (Spec 05 §5.2) — the FIRS remittance sheet.
pub fn wht_remittance_schedule(conn: &Connection, company_id: &str, range: &PeriodRange) -> R<Vec<WhtRemittanceLine>> {
    let mut q = conn.prepare(
        "SELECT p.payment_date, c.name, c.tin, GROUP_CONCAT(b.reference, ', '),
                SUM(pa.amount_kobo), p.wht_kobo
         FROM payments p
         JOIN contacts c ON c.id = p.contact_id
         LEFT JOIN payment_allocations pa ON pa.payment_id = p.id AND pa.target_type = 'bill'
         LEFT JOIN bills b ON b.id = pa.target_id
         WHERE p.company_id = ?1 AND p.direction = 'out' AND p.voided = 0 AND p.wht_kobo > 0
           AND p.payment_date BETWEEN ?2 AND ?3
         GROUP BY p.id ORDER BY p.payment_date",
    )?;
    let rows = q.query_map(params![company_id, range.start, range.end], |r| {
        Ok(WhtRemittanceLine {
            payment_date: r.get(0)?, supplier_name: r.get(1)?, tin: r.get(2)?, bill_ref: r.get(3)?,
            base_kobo: r.get(4)?, wht_kobo: r.get(5)?,
        })
    })?.collect::<Result<_, _>>()?;
    Ok(rows)
}

#[derive(Debug, Clone)]
pub struct WhtCreditLine { pub receipt_date: String, pub customer_name: String, pub tin: Option<String>, pub base_kobo: i64, pub wht_kobo: i64 }

/// What customers withheld from us (Spec 05 §5.2) — the CIT-offset evidence pack.
pub fn wht_credit_schedule(conn: &Connection, company_id: &str, range: &PeriodRange) -> R<Vec<WhtCreditLine>> {
    let mut q = conn.prepare(
        "SELECT p.payment_date, c.name, c.tin,
                SUM(pa.amount_kobo), p.wht_kobo
         FROM payments p
         JOIN contacts c ON c.id = p.contact_id
         LEFT JOIN payment_allocations pa ON pa.payment_id = p.id AND pa.target_type = 'invoice'
         WHERE p.company_id = ?1 AND p.direction = 'in' AND p.voided = 0 AND p.wht_kobo > 0
           AND p.payment_date BETWEEN ?2 AND ?3
         GROUP BY p.id ORDER BY p.payment_date",
    )?;
    let rows = q.query_map(params![company_id, range.start, range.end], |r| {
        Ok(WhtCreditLine { receipt_date: r.get(0)?, customer_name: r.get(1)?, tin: r.get(2)?, base_kobo: r.get(3)?, wht_kobo: r.get(4)? })
    })?.collect::<Result<_, _>>()?;
    Ok(rows)
}

/// Cumulative WHT credit available toward CIT offset — a live query, never a
/// stored balance (Spec 05 §5.2, same no-drift discipline as VAT carry-forward).
pub fn wht_cumulative_credit(conn: &Connection, company_id: &str, as_of: &str) -> R<i64> {
    Ok(conn.query_row(
        "SELECT COALESCE(SUM(l.amount_kobo), 0)
         FROM journal_lines l JOIN journal_entries e ON e.id = l.entry_id
         JOIN accounts a ON a.id = l.account_id
         WHERE e.company_id = ?1 AND e.is_posted = 1 AND e.entry_date <= ?2
           AND a.system_key = 'WHT_RECEIVABLE'",
        params![company_id, as_of], |r| r.get(0),
    )?)
}

// ===== §6 — Contact Statement of Account =====

#[derive(Debug, Clone)]
pub struct StatementLine { pub date: String, pub description: String, pub debit_kobo: i64, pub credit_kobo: i64, pub running_balance_kobo: i64, pub voided: bool }

#[derive(Debug, Clone)]
pub struct ContactStatement { pub contact_name: String, pub opening_balance_kobo: i64, pub lines: Vec<StatementLine>, pub closing_balance_kobo: i64 }

/// Per contact (Spec 05 §6): reads the AR or AP subledger directly — whichever
/// system account the contact's documents post to — via `general_ledger`'s
/// contact filter, then relabels rows in plain document language.
pub fn contact_statement(conn: &Connection, company_id: &str, contact_id: &str, start: &str, end: &str) -> R<ContactStatement> {
    let (contact_name, kind): (String, String) = conn.query_row(
        "SELECT name, kind FROM contacts WHERE id = ?1", params![contact_id], |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let system_key = if kind == "supplier" { "AP" } else { "AR" };
    let account_id: String = conn.query_row(
        "SELECT id FROM accounts WHERE company_id = ?1 AND system_key = ?2",
        params![company_id, system_key], |r| r.get(0),
    )?;
    let gl = general_ledger(conn, company_id, &account_id, start, end, Some(contact_id))?;
    let mut running = gl.opening_balance_kobo;
    let lines = gl.lines.into_iter().map(|l| {
        running += 0; // running already tracked by general_ledger; recompute debit/credit split for display
        StatementLine {
            date: l.date, description: l.memo,
            debit_kobo: l.amount_kobo.max(0), credit_kobo: (-l.amount_kobo).max(0),
            running_balance_kobo: l.running_balance_kobo, voided: false,
        }
    }).collect();
    Ok(ContactStatement { contact_name, opening_balance_kobo: gl.opening_balance_kobo, lines, closing_balance_kobo: gl.closing_balance_kobo })
}

/// Optional helper for `contact_statement`: a live deposit balance line the
/// UI can show alongside (never merged into the AR running balance).
pub fn contact_deposit_balance(conn: &Connection, company_id: &str, contact_id: &str) -> R<i64> {
    crate::engine::deposit_balance(conn, company_id, contact_id)
}
