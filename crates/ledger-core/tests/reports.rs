//! Spec 05 tests. The two highest-stakes claims get scenario-level proof:
//! the balance sheet ties (Assets = Liabilities + Equity) after a realistic
//! mix of transactions spanning a fiscal-year boundary, and the cash-basis
//! P&L matches the exact §4.2 allocation-proportional formula by hand. The
//! rest get focused correctness tests, not exhaustive coverage.

use ledger_core::engine::{self, InvoiceLineInput, PostCtx};
use ledger_core::ids::new_id;
use ledger_core::reports::*;
use ledger_core::rusqlite::{params, Connection};
use ledger_core::seed::{add_bank_account, create_company, CompanyConfig};
use ledger_core::LineSpec;

struct W { conn: Connection, company: String, bank: String, customer: String, supplier: String }

fn world() -> W {
    let mut conn = ledger_core::open(":memory:").unwrap();
    let company = create_company(&mut conn, &CompanyConfig::default()).unwrap(); // fiscal_year_start_month = 1
    let bank = add_bank_account(&mut conn, &company, "GTBank Current", "bank", "NGN").unwrap();
    let customer = contact(&conn, &company, "Chidinma Stores", "customer", Some("TIN-C-1"));
    let supplier = contact(&conn, &company, "Mainland Suppliers", "supplier", Some("TIN-S-1"));
    W { conn, company, bank, customer, supplier }
}

fn contact(conn: &Connection, company: &str, name: &str, kind: &str, tin: Option<&str>) -> String {
    let id = new_id();
    conn.execute(
        "INSERT INTO contacts (id, company_id, kind, name, tin, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![id, company, kind, name, tin, ledger_core::ids::now_iso()],
    ).unwrap();
    id
}

fn line(desc: &str, qty_milli: i64, price_kobo: i64, vat: bool) -> InvoiceLineInput {
    InvoiceLineInput {
        product_id: None, description: desc.into(), quantity_milli: qty_milli,
        unit_price_kobo: price_kobo, discount_bp: 0, vat_applied: vat, income_account_id: None,
    }
}

fn account_id(conn: &Connection, company: &str, system_key: &str) -> String {
    conn.query_row(
        "SELECT id FROM accounts WHERE company_id = ?1 AND system_key = ?2",
        params![company, system_key], |r| r.get(0),
    ).unwrap()
}

// ===== Period helpers =====

#[test]
fn fiscal_period_math_is_exact() {
    // Fiscal year starting April: a date in Feb 2027 belongs to the FY that started April 2026.
    assert_eq!(fiscal_year_start(4, "2027-02-15"), "2026-04-01");
    assert_eq!(fiscal_year_start(4, "2026-04-01"), "2026-04-01");
    assert_eq!(fiscal_year_start(1, "2026-07-06"), "2026-01-01");

    let ytd = ytd_range(1, "2026-07-06");
    assert_eq!((ytd.start.as_str(), ytd.end.as_str()), ("2026-01-01", "2026-07-06"));

    // Calendar-year company: Q1=Jan-Mar, Q3=Jul-Sep.
    let q = fiscal_quarter_range(1, "2026-08-15");
    assert_eq!((q.start.as_str(), q.end.as_str()), ("2026-07-01", "2026-09-30"));

    // April-start fiscal year: the quarter containing July 2026 is FY-Q2 (Jul-Sep).
    let q2 = fiscal_quarter_range(4, "2026-07-06");
    assert_eq!((q2.start.as_str(), q2.end.as_str()), ("2026-07-01", "2026-09-30"));

    let feb = calendar_month_range(2028, 2); // leap year
    assert_eq!((feb.start.as_str(), feb.end.as_str()), ("2028-02-01", "2028-02-29"));
}

// ===== Aging (§3.2) =====

#[test]
fn aging_buckets_correctly_and_deposits_never_netted() {
    let mut w = world();
    let ctx = PostCtx::default();
    // Two invoices: one 45 days overdue (31-60 bucket), one not yet due (current).
    let inv1 = engine::create_invoice(
        &mut w.conn, &w.company, &w.customer, "invoice", "2026-05-01", "2026-05-15",
        &[line("Goods", 1000, 100_000_00, false)], &ctx,
    ).unwrap();
    engine::post_invoice(&mut w.conn, &inv1, &ctx).unwrap();
    let inv2 = engine::create_invoice(
        &mut w.conn, &w.company, &w.customer, "invoice", "2026-06-25", "2026-07-20",
        &[line("Goods", 1000, 50_000_00, false)], &ctx,
    ).unwrap();
    engine::post_invoice(&mut w.conn, &inv2, &ctx).unwrap();
    // An overpayment creates a deposit — must show ALONGSIDE, never reduce the buckets.
    engine::post_payment_in(
        &mut w.conn, &w.company, &w.customer, &w.bank, "2026-06-01",
        120_000_00, 0, &[engine::Allocation { target_id: inv1.clone(), amount_kobo: 100_000_00 }], None, &ctx,
    ).unwrap(); // 100k clears inv1, 20k becomes a deposit

    let as_of = "2026-07-06"; // 45 days after inv1 due (2026-05-15); inv2 not yet due
    let rows = aging_receivables(&w.conn, &w.company, as_of).unwrap();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.buckets.d31_60_kobo, 0, "inv1 is fully paid, contributes nothing");
    assert_eq!(row.buckets.current_kobo, 50_000_00, "inv2 not yet due");
    assert_eq!(row.deposit_kobo, 20_000_00, "the 20k overpayment shows as a deposit");
    assert_eq!(row.buckets.total(), 50_000_00, "deposit must NOT be subtracted from the balance owed");
}

// ===== Sales by customer/product, with top-N rollup (§3.4) =====

#[test]
fn sales_by_customer_rolls_up_beyond_top_n() {
    let mut w = world();
    let ctx = PostCtx::default();
    let names = ["Ada", "Bola", "Chika"];
    for (i, name) in names.iter().enumerate() {
        let cid = contact(&w.conn, &w.company, name, "customer", None);
        let inv = engine::create_invoice(
            &mut w.conn, &w.company, &cid, "invoice", "2026-07-01", "2026-07-15",
            &[line("Item", 1000, ((i as i64) + 1) * 10_000_00, false)], &ctx,
        ).unwrap();
        engine::post_invoice(&mut w.conn, &inv, &ctx).unwrap();
    }
    let rows = sales_by_customer(&w.conn, &w.company, "2026-07-01", "2026-07-31", 2).unwrap();
    assert_eq!(rows.len(), 3); // top 2 + 1 rollup row
    assert_eq!(rows[0].value_kobo, 30_000_00); // Chika, highest
    assert!(rows[2].label.starts_with("Everyone else"));
    assert_eq!(rows[2].value_kobo, 10_000_00); // Ada, rolled up
}

// ===== Accrual income statement (§4.1) =====

#[test]
fn accrual_income_statement_matches_hand_computation() {
    let mut w = world();
    let ctx = PostCtx::default();
    let inv = engine::create_invoice(
        &mut w.conn, &w.company, &w.customer, "invoice", "2026-07-01", "2026-07-15",
        &[line("Goods", 1000, 200_000_00, false)], &ctx,
    ).unwrap();
    engine::post_invoice(&mut w.conn, &inv, &ctx).unwrap(); // Revenue 200,000

    let rent_acct: String = w.conn.query_row(
        "SELECT id FROM accounts WHERE company_id = ?1 AND code = '6100'", params![w.company], |r| r.get(0),
    ).unwrap();
    engine::post_expense(
        &mut w.conn, &w.company, &w.bank, "Landlord", &rent_acct,
        "2026-07-02", 30_000_00, false, 0, &ctx,
    ).unwrap(); // OpEx 30,000, no VAT

    let is = income_statement_accrual(&w.conn, &w.company, "2026-07-01", "2026-07-31").unwrap();
    assert_eq!(is.revenue_total_kobo, 200_000_00);
    assert_eq!(is.cogs_total_kobo, 0);
    assert_eq!(is.gross_profit_kobo, 200_000_00);
    assert_eq!(is.gross_margin_bp, 10_000); // 100.00%
    assert_eq!(is.opex_total_kobo, 30_000_00);
    assert_eq!(is.operating_profit_kobo, 170_000_00);
    assert_eq!(is.net_profit_kobo, 170_000_00);
}

// ===== Cash-basis P&L — exact formula proof (§4.2, Decision #2) =====

#[test]
fn cash_basis_recognizes_proportionally_at_payment_and_application_date() {
    let mut w = world();
    let ctx = PostCtx::default();
    // Invoice: net 100,000 + VAT 7,500 = total 107,500. Company is VAT-registered by default.
    let inv = engine::create_invoice(
        &mut w.conn, &w.company, &w.customer, "invoice", "2026-06-01", "2026-06-15",
        &[line("Goods", 1000, 100_000_00, true)], &ctx,
    ).unwrap();
    engine::post_invoice(&mut w.conn, &inv, &ctx).unwrap();
    let total: i64 = w.conn.query_row("SELECT total_kobo FROM invoices WHERE id = ?1", params![inv], |r| r.get(0)).unwrap();
    let subtotal: i64 = w.conn.query_row("SELECT subtotal_kobo FROM invoices WHERE id = ?1", params![inv], |r| r.get(0)).unwrap();
    assert_eq!((subtotal, total), (100_000_00, 107_500_00));

    // Partial payment of 53,750 (half) on 2026-07-10 — should recognize exactly half the net.
    engine::post_payment_in(
        &mut w.conn, &w.company, &w.customer, &w.bank, "2026-07-10",
        53_750_00, 0, &[engine::Allocation { target_id: inv.clone(), amount_kobo: 53_750_00 }], None, &ctx,
    ).unwrap();

    let cb = income_statement_cash_basis(&w.conn, &w.company, "2026-07-01", "2026-07-31").unwrap();
    let expected = ledger_core::money::round_ratio(53_750_00i128 * 100_000_00i128, 107_500_00i128);
    assert_eq!(cb.revenue_kobo, expected);
    assert_eq!(cb.revenue_kobo, 50_000_00, "half the allocation should recognize half the net");
    assert_eq!(cb.caption, "Cash basis — derived from settled amounts; VAT excluded.");

    // June's payment-free window recognizes nothing (accrual date != cash date).
    let cb_june = income_statement_cash_basis(&w.conn, &w.company, "2026-06-01", "2026-06-30").unwrap();
    assert_eq!(cb_june.revenue_kobo, 0, "cash basis ignores the accrual issue date entirely");
}

#[test]
fn cash_basis_deposit_application_recognized_on_application_not_receipt() {
    let mut w = world();
    let ctx = PostCtx::default();
    // Overpay with no open invoice at all: the whole amount becomes a deposit.
    engine::post_payment_in(
        &mut w.conn, &w.company, &w.customer, &w.bank, "2026-06-01", 50_000_00, 0, &[], None, &ctx,
    ).unwrap();
    let cb_june = income_statement_cash_basis(&w.conn, &w.company, "2026-06-01", "2026-06-30").unwrap();
    assert_eq!(cb_june.revenue_kobo, 0, "an unapplied deposit is not revenue yet");

    let inv = engine::create_invoice(
        &mut w.conn, &w.company, &w.customer, "invoice", "2026-07-01", "2026-07-15",
        &[line("Goods", 1000, 50_000_00, false)], &ctx,
    ).unwrap();
    engine::post_invoice(&mut w.conn, &inv, &ctx).unwrap();
    engine::apply_deposit(&mut w.conn, &w.company, &w.customer, &inv, 50_000_00, "2026-07-20", &ctx).unwrap();

    let cb_july = income_statement_cash_basis(&w.conn, &w.company, "2026-07-01", "2026-07-31").unwrap();
    assert_eq!(cb_july.revenue_kobo, 50_000_00, "recognized on the APPLICATION date, in July");
}

// ===== Balance sheet — ties by construction, across a fiscal-year boundary =====

#[test]
fn balance_sheet_ties_across_a_fiscal_year_boundary() {
    let mut w = world(); // fiscal_year_start_month = 1 (calendar year)
    let ctx = PostCtx::default();

    // Prior year: a sale and an expense.
    let inv_2025 = engine::create_invoice(
        &mut w.conn, &w.company, &w.customer, "invoice", "2025-11-01", "2025-11-15",
        &[line("Goods", 1000, 300_000_00, false)], &ctx,
    ).unwrap();
    engine::post_invoice(&mut w.conn, &inv_2025, &ctx).unwrap();
    engine::post_payment_in(
        &mut w.conn, &w.company, &w.customer, &w.bank, "2025-11-20",
        300_000_00, 0, &[engine::Allocation { target_id: inv_2025.clone(), amount_kobo: 300_000_00 }], None, &ctx,
    ).unwrap();
    let misc: String = w.conn.query_row(
        "SELECT id FROM accounts WHERE company_id = ?1 AND code = '6980'", params![w.company], |r| r.get(0),
    ).unwrap();
    engine::post_expense(&mut w.conn, &w.company, &w.bank, "Sundry", &misc, "2025-12-01", 50_000_00, false, 0, &ctx).unwrap();

    // Current year: another sale (still open, unpaid — a real receivable), a bill (unpaid payable),
    // and a drawing.
    let inv_2026 = engine::create_invoice(
        &mut w.conn, &w.company, &w.customer, "invoice", "2026-03-01", "2026-03-15",
        &[line("Goods", 1000, 150_000_00, false)], &ctx,
    ).unwrap();
    engine::post_invoice(&mut w.conn, &inv_2026, &ctx).unwrap();

    let purchases_acct: String = w.conn.query_row(
        "SELECT id FROM accounts WHERE company_id = ?1 AND code = '5100'", params![w.company], |r| r.get(0),
    ).unwrap();
    let bill = engine::create_bill(
        &mut w.conn, &w.company, &w.supplier, "2026-04-01", "2026-04-30", false, None,
        &[engine::BillLineInput {
            product_id: None, description: "Stock".into(), quantity_milli: 1000, unit_cost_kobo: 80_000_00,
            vat_charged: false, vat_claimable: false, expense_account_id: purchases_acct,
        }], &ctx,
    ).unwrap();
    engine::post_bill(&mut w.conn, &bill, &ctx).unwrap();

    engine::post_drawing(&mut w.conn, &w.company, &w.bank, "2026-05-01", 40_000_00, true, &ctx).unwrap();

    // An advisor adjustment directly to Retained Earnings — the one case where 3900
    // is posted to directly (Spec 05 §4.3: "advisor adjustments only"), e.g. reclassifying
    // ₦20,000 of Opening Balance Equity into Retained Earnings after review.
    let re = account_id(&w.conn, &w.company, "RETAINED_EARNINGS");
    let obe = account_id(&w.conn, &w.company, "OPENING_BALANCE_EQUITY");
    engine::post_journal(
        &mut w.conn, &w.company, "2026-01-15", "Reclassify OBE into Retained Earnings", "manual", &ctx,
        &[LineSpec::new(&obe, 20_000_00), LineSpec::new(&re, -20_000_00)],
    ).unwrap();

    let bs = balance_sheet(&w.conn, &w.company, "2026-07-06").unwrap();
    assert!(bs.ties, "assets must equal liabilities + equity, always");
    assert_eq!(bs.total_assets_kobo, bs.total_liabilities_kobo + bs.total_equity_kobo);

    // Sanity on the pieces we can hand-verify:
    assert_eq!(bs.receivables_kobo, 150_000_00, "the 2026 invoice is still fully open");
    assert_eq!(bs.current_liabilities_kobo, 80_000_00, "the unpaid bill");
    assert_eq!(bs.drawings_kobo, -40_000_00, "drawings must show as a NEGATIVE (deduction) in equity");
    // Prior-year profit (300,000 revenue − 50,000 expense = 250,000) must land in
    // Retained Earnings, NOT Current Year Earnings, once the fiscal year has rolled over —
    // plus the ₦20,000 manual reclassification posted directly to 3900.
    assert_eq!(bs.retained_earnings_kobo, 270_000_00);
    // 2026: the invoice is accrual-recognized at posting regardless of payment status
    // (150,000 revenue) against the bill's 80,000 cogs = 70,000 profit so far this year.
    assert_eq!(bs.current_year_earnings_kobo, 70_000_00);
}

// ===== Cash flow statement =====

#[test]
fn cash_flow_statement_ties_to_the_bank_delta() {
    let mut w = world();
    let ctx = PostCtx::default();
    engine::post_drawing(&mut w.conn, &w.company, &w.bank, "2026-01-05", 500_00, false, &ctx).unwrap(); // owner puts in 500 (capital)
    let inv = engine::create_invoice(
        &mut w.conn, &w.company, &w.customer, "invoice", "2026-07-01", "2026-07-10",
        &[line("Goods", 1000, 20_000_00, false)], &ctx,
    ).unwrap();
    engine::post_invoice(&mut w.conn, &inv, &ctx).unwrap();
    engine::post_payment_in(
        &mut w.conn, &w.company, &w.customer, &w.bank, "2026-07-12",
        20_000_00, 0, &[engine::Allocation { target_id: inv, amount_kobo: 20_000_00 }], None, &ctx,
    ).unwrap();

    let cf = cash_flow_statement(&w.conn, &w.company, "2026-07-01", "2026-07-31").unwrap();
    assert!(cf.ties);
    assert_eq!(cf.closing_cash_kobo - cf.opening_cash_kobo, cf.net_change_kobo);
}

// ===== Trial balance =====

#[test]
fn trial_balance_debits_equal_credits() {
    let mut w = world();
    let ctx = PostCtx::default();
    engine::post_drawing(&mut w.conn, &w.company, &w.bank, "2026-07-01", 10_000_00, false, &ctx).unwrap();
    let tb = trial_balance(&w.conn, &w.company, "2026-07-31").unwrap();
    let total_dr: i64 = tb.iter().map(|r| r.debit_kobo).sum();
    let total_cr: i64 = tb.iter().map(|r| r.credit_kobo).sum();
    assert_eq!(total_dr, total_cr);
    assert!(total_dr > 0);
}

// ===== General ledger (contact-filtered) =====

#[test]
fn general_ledger_running_balance_and_contact_filter() {
    let mut w = world();
    let ctx = PostCtx::default();
    let inv = engine::create_invoice(
        &mut w.conn, &w.company, &w.customer, "invoice", "2026-07-01", "2026-07-15",
        &[line("Goods", 1000, 75_000_00, false)], &ctx,
    ).unwrap();
    engine::post_invoice(&mut w.conn, &inv, &ctx).unwrap();
    let ar = account_id(&w.conn, &w.company, "AR");
    let gl = general_ledger(&w.conn, &w.company, &ar, "2026-07-01", "2026-07-31", Some(&w.customer)).unwrap();
    assert_eq!(gl.opening_balance_kobo, 0);
    assert_eq!(gl.lines.len(), 1);
    assert_eq!(gl.lines[0].running_balance_kobo, 75_000_00);
    assert_eq!(gl.closing_balance_kobo, 75_000_00);
}

// ===== VAT report =====

#[test]
fn vat_report_nets_output_against_claimable_input() {
    let mut w = world();
    let ctx = PostCtx::default();
    let inv = engine::create_invoice(
        &mut w.conn, &w.company, &w.customer, "invoice", "2026-07-05", "2026-07-19",
        &[line("Goods", 1000, 100_000_00, true)], &ctx, // VAT 7,500
    ).unwrap();
    engine::post_invoice(&mut w.conn, &inv, &ctx).unwrap();

    let stock_acct: String = w.conn.query_row(
        "SELECT id FROM accounts WHERE company_id = ?1 AND code = '5100'", params![w.company], |r| r.get(0),
    ).unwrap();
    let bill = engine::create_bill(
        &mut w.conn, &w.company, &w.supplier, "2026-07-10", "2026-07-24", false, None,
        &[engine::BillLineInput {
            product_id: None, description: "Stock".into(), quantity_milli: 1000, unit_cost_kobo: 40_000_00,
            vat_charged: true, vat_claimable: true, expense_account_id: stock_acct,
        }], &ctx,
    ).unwrap();
    engine::post_bill(&mut w.conn, &bill, &ctx).unwrap(); // input VAT 3,000

    let month = calendar_month_range(2026, 7);
    let vat = vat_report(&w.conn, &w.company, &month).unwrap();
    assert_eq!(vat.output_vat_kobo, 7_500_00);
    assert_eq!(vat.input_vat_kobo, 3_000_00);
    assert_eq!(vat.net_payable_kobo, 4_500_00);
    assert_eq!(vat.credit_brought_forward_kobo, 0);
    assert_eq!(vat.net_due_kobo, 4_500_00);
}

// ===== WHT schedules =====

#[test]
fn wht_schedules_capture_both_directions() {
    let mut w = world();
    let ctx = PostCtx::default();
    // Customer withholds from us.
    let inv = engine::create_invoice(
        &mut w.conn, &w.company, &w.customer, "invoice", "2026-07-01", "2026-07-15",
        &[line("Services", 1000, 100_000_00, false)], &ctx,
    ).unwrap();
    engine::post_invoice(&mut w.conn, &inv, &ctx).unwrap();
    engine::post_payment_in(
        &mut w.conn, &w.company, &w.customer, &w.bank, "2026-07-10",
        95_000_00, 5_000_00, &[engine::Allocation { target_id: inv, amount_kobo: 100_000_00 }], None, &ctx,
    ).unwrap();

    let range = PeriodRange { start: "2026-07-01".into(), end: "2026-07-31".into() };
    let credit = wht_credit_schedule(&w.conn, &w.company, &range).unwrap();
    assert_eq!(credit.len(), 1);
    assert_eq!(credit[0].wht_kobo, 5_000_00);
    assert_eq!(wht_cumulative_credit(&w.conn, &w.company, "2026-07-31").unwrap(), 5_000_00);

    // We withhold from a supplier (small-company exemption off by default => cit_exempt=false in
    // CompanyConfig::default(), so a flagged bill actually withholds).
    let acct: String = w.conn.query_row(
        "SELECT id FROM accounts WHERE company_id = ?1 AND code = '6600'", params![w.company], |r| r.get(0),
    ).unwrap();
    let bill = engine::create_bill(
        &mut w.conn, &w.company, &w.supplier, "2026-07-05", "2026-07-19", true, Some(500),
        &[engine::BillLineInput {
            product_id: None, description: "Consulting".into(), quantity_milli: 1000, unit_cost_kobo: 200_000_00,
            vat_charged: false, vat_claimable: false, expense_account_id: acct,
        }], &ctx,
    ).unwrap();
    engine::post_bill(&mut w.conn, &bill, &ctx).unwrap();
    engine::post_payment_out(
        &mut w.conn, &w.company, &w.supplier, &w.bank, "2026-07-12",
        &[engine::Allocation { target_id: bill, amount_kobo: 200_000_00 }], engine::WhtMode::Auto, &ctx,
    ).unwrap();

    let remittance = wht_remittance_schedule(&w.conn, &w.company, &range).unwrap();
    assert_eq!(remittance.len(), 1);
    assert_eq!(remittance[0].wht_kobo, 10_000_00); // 5% of 200,000
}

// ===== Contact statement =====

#[test]
fn contact_statement_shows_running_balance_and_deposit_alongside() {
    let mut w = world();
    let ctx = PostCtx::default();
    let inv = engine::create_invoice(
        &mut w.conn, &w.company, &w.customer, "invoice", "2026-07-01", "2026-07-15",
        &[line("Goods", 1000, 60_000_00, false)], &ctx,
    ).unwrap();
    engine::post_invoice(&mut w.conn, &inv, &ctx).unwrap();
    engine::post_payment_in(
        &mut w.conn, &w.company, &w.customer, &w.bank, "2026-07-10",
        70_000_00, 0, &[engine::Allocation { target_id: inv, amount_kobo: 60_000_00 }], None, &ctx,
    ).unwrap();

    let st = contact_statement(&w.conn, &w.company, &w.customer, "2026-07-01", "2026-07-31").unwrap();
    assert_eq!(st.contact_name, "Chidinma Stores");
    assert_eq!(st.closing_balance_kobo, 0, "fully paid, AR back to zero");
    assert_eq!(contact_deposit_balance(&w.conn, &w.company, &w.customer).unwrap(), 10_000_00);
}
