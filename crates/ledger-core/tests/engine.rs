//! Template tests: every posting function checked against its exact Dr/Cr
//! table from Spec 01 §6, plus the tax edge cases (WHT exemption, no-TIN 2×,
//! VAT-unregistered, zero-total free samples, WAC + P7).

use ledger_core::engine::*;
use ledger_core::ids::{new_id, now_iso};
use ledger_core::seed::{add_bank_account, create_company, CompanyConfig, COA_SEED_COUNT};
use ledger_core::PostCtx;
use rusqlite::{params, Connection};

struct World {
    conn: Connection,
    company: String,
    customer: String,
    supplier: String,
    bank: String, // bank_accounts.id (GTBank, kind bank)
    cash: String, // bank_accounts.id (petty cash, kind cash)
}

fn world_with(cfg: CompanyConfig) -> World {
    let mut conn = ledger_core::open(":memory:").unwrap();
    let company = create_company(&mut conn, &cfg).unwrap();
    let bank = add_bank_account(&mut conn, &company, "GTBank Current", "bank", "NGN").unwrap();
    let cash = add_bank_account(&mut conn, &company, "Petty Cash", "cash", "NGN").unwrap();
    let customer = contact(&conn, &company, "Chidinma Stores", "customer", Some("TIN-C-1"));
    let supplier = contact(&conn, &company, "Mainland Suppliers", "supplier", Some("TIN-S-1"));
    World { conn, company, customer, supplier, bank, cash }
}

fn world() -> World {
    world_with(CompanyConfig::default())
}

fn contact(conn: &Connection, company: &str, name: &str, kind: &str, tin: Option<&str>) -> String {
    let id = new_id();
    conn.execute(
        "INSERT INTO contacts (id, company_id, kind, name, tin, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![id, company, kind, name, tin, now_iso()],
    )
    .unwrap();
    id
}

fn acct_by_code(conn: &Connection, company: &str, code: &str) -> String {
    conn.query_row(
        "SELECT id FROM accounts WHERE company_id = ?1 AND code = ?2",
        params![company, code],
        |r| r.get(0),
    )
    .unwrap()
}

/// Entry lines as (account_code, amount, has_contact), sorted — for exact template asserts.
fn entry_lines(conn: &Connection, entry_id: &str) -> Vec<(String, i64, bool)> {
    let mut q = conn
        .prepare(
            "SELECT a.code, l.amount_kobo, l.contact_id IS NOT NULL
             FROM journal_lines l JOIN accounts a ON a.id = l.account_id
             WHERE l.entry_id = ?1 ORDER BY a.code, l.amount_kobo",
        )
        .unwrap();
    q.query_map(params![entry_id], |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)? != 0)))
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
}

fn invoice_status(conn: &Connection, id: &str) -> String {
    conn.query_row("SELECT status FROM invoices WHERE id = ?1", params![id], |r| r.get(0))
        .unwrap()
}

fn trial_balance(conn: &Connection) -> i64 {
    conn.query_row(
        "SELECT COALESCE(SUM(l.amount_kobo),0) FROM journal_lines l
         JOIN journal_entries e ON e.id = l.entry_id WHERE e.is_posted = 1",
        [],
        |r| r.get(0),
    )
    .unwrap()
}

fn line(desc: &str, qty_milli: i64, price: i64, vat: bool) -> InvoiceLineInput {
    InvoiceLineInput {
        product_id: None,
        description: desc.into(),
        quantity_milli: qty_milli,
        unit_price_kobo: price,
        discount_bp: 0,
        vat_applied: vat,
        income_account_id: None,
    }
}

// ===== seed =====

#[test]
fn seed_creates_full_coa_with_resolvable_system_keys() {
    let w = world();
    let n: i64 = w.conn.query_row(
        "SELECT COUNT(*) FROM accounts WHERE company_id = ?1 AND is_bank = 0",
        params![w.company], |r| r.get(0),
    ).unwrap();
    assert_eq!(n as usize, COA_SEED_COUNT);

    for key in [
        "AR", "INVENTORY", "VAT_INPUT", "WHT_RECEIVABLE", "AP", "VAT_OUTPUT", "WHT_PAYABLE",
        "UNEARNED_REVENUE", "OPENING_BALANCE_EQUITY", "OWNER_CAPITAL", "OWNER_DRAWINGS",
        "RETAINED_EARNINGS", "SALES_DEFAULT", "FX_GAIN_LOSS", "COGS_DEFAULT", "BANK_CHARGES", "ROUNDING",
    ] {
        let found: i64 = w.conn.query_row(
            "SELECT COUNT(*) FROM accounts WHERE company_id = ?1 AND system_key = ?2 AND is_system = 1",
            params![w.company, key], |r| r.get(0),
        ).unwrap();
        assert_eq!(found, 1, "system key {key}");
    }

    // Write-off routing seeded to 6980 / 4200 (Spec 04 #7).
    let (dr, cr): (String, String) = w.conn.query_row(
        "SELECT (SELECT code FROM accounts WHERE id = writeoff_debit_account_id),
                (SELECT code FROM accounts WHERE id = writeoff_credit_account_id)
         FROM companies WHERE id = ?1",
        params![w.company], |r| Ok((r.get(0)?, r.get(1)?)),
    ).unwrap();
    assert_eq!((dr.as_str(), cr.as_str()), ("6980", "4200"));
}

// ===== postInvoice (Spec 01 §6.1) =====

#[test]
fn invoice_template_dr_ar_cr_income_cr_vat() {
    let mut w = world();
    let ctx = PostCtx::default();
    // ₦100,000 vatable + ₦50,000 non-vatable → VAT ₦7,500, total ₦157,500.
    let inv = create_invoice(
        &mut w.conn, &w.company, &w.customer, "invoice", "2026-07-04", "2026-08-03",
        &[line("Goods", 1_000, 100_000_00, true), line("Delivery", 1_000, 50_000_00, false)],
        &ctx,
    ).unwrap();
    let entry = post_invoice(&mut w.conn, inv.as_str(), &ctx).unwrap().unwrap();

    assert_eq!(
        entry_lines(&w.conn, &entry),
        vec![
            ("1100".into(), 157_500_00, true),  // Dr AR, carries contact (P8)
            ("2210".into(), -7_500_00, false),  // Cr VAT Output
            ("4000".into(), -150_000_00, false) // Cr Sales (grouped)
        ]
    );
    assert_eq!(invoice_status(&w.conn, &inv), "sent");

    // Number sequencing + immutability of the posted invoice's path:
    let number: String = w.conn.query_row(
        "SELECT number FROM invoices WHERE id = ?1", params![inv], |r| r.get(0),
    ).unwrap();
    assert_eq!(number, "INV-000001");
    // Re-posting a sent invoice is refused (void & reissue is the only correction path).
    assert!(post_invoice(&mut w.conn, inv.as_str(), &ctx).is_err());
}

#[test]
fn invoice_no_vat_lines_when_unregistered() {
    let mut w = world_with(CompanyConfig {
        vat_registered: false, vat_exempt: true, ..Default::default()
    });
    let ctx = PostCtx::default();
    let inv = create_invoice(
        &mut w.conn, &w.company, &w.customer, "invoice", "2026-07-04", "2026-07-18",
        &[line("Goods", 1_000, 100_000_00, true)], // VAT toggle on, but company exempt
        &ctx,
    ).unwrap();
    let entry = post_invoice(&mut w.conn, inv.as_str(), &ctx).unwrap().unwrap();
    assert_eq!(
        entry_lines(&w.conn, &entry),
        vec![("1100".into(), 100_000_00, true), ("4000".into(), -100_000_00, false)]
    );
}

#[test]
fn zero_total_invoice_posts_nothing_and_settles_paid() {
    let mut w = world();
    let ctx = PostCtx::default();
    let inv = create_invoice(
        &mut w.conn, &w.company, &w.customer, "invoice", "2026-07-04", "2026-07-04",
        &[line("Free sample", 1_000, 0, false)], &ctx,
    ).unwrap();
    let entry = post_invoice(&mut w.conn, inv.as_str(), &ctx).unwrap();
    assert!(entry.is_none(), "no JE for zero-total without stock lines (Spec 03 V7)");
    assert_eq!(invoice_status(&w.conn, &inv), "paid");
}

#[test]
fn inventory_cogs_at_wac_and_p7_negative_stock() {
    let mut w = world_with(CompanyConfig { inventory_enabled: true, ..Default::default() });
    let ctx = PostCtx::default();
    let inventory_acct = acct_by_code(&w.conn, &w.company, "1200");

    let product = new_id();
    w.conn.execute(
        "INSERT INTO products (id, company_id, kind, name, sale_price_kobo, track_inventory)
         VALUES (?1, ?2, 'product', 'Peak Milk Carton', 90000, 1)",
        params![product, w.company],
    ).unwrap();

    // Purchase 100 units @ ₦500 into stock (bill → inventory).
    let bill = create_bill(
        &mut w.conn, &w.company, &w.supplier, "2026-07-01", "2026-07-31", false, None,
        &[BillLineInput {
            product_id: Some(product.clone()),
            description: "Stock".into(),
            quantity_milli: 100_000,
            unit_cost_kobo: 500_00,
            vat_charged: false,
            vat_claimable: false,
            expense_account_id: inventory_acct.clone(),
        }],
        &ctx,
    ).unwrap();
    post_bill(&mut w.conn, bill.as_str(), &ctx).unwrap();

    // Sell 40 units @ ₦900: COGS = 40 × WAC(₦500) = ₦20,000.
    let inv = create_invoice(
        &mut w.conn, &w.company, &w.customer, "invoice", "2026-07-04", "2026-07-18",
        &[InvoiceLineInput {
            product_id: Some(product.clone()),
            description: "Peak Milk Carton".into(),
            quantity_milli: 40_000,
            unit_price_kobo: 900_00,
            discount_bp: 0,
            vat_applied: true,
            income_account_id: None,
        }],
        &ctx,
    ).unwrap();
    let entry = post_invoice(&mut w.conn, inv.as_str(), &ctx).unwrap().unwrap();
    assert_eq!(
        entry_lines(&w.conn, &entry),
        vec![
            ("1100".into(), 38_700_00, true),   // 36,000 + 2,700 VAT
            ("1200".into(), -20_000_00, false), // Cr Inventory at WAC
            ("2210".into(), -2_700_00, false),
            ("4000".into(), -36_000_00, false),
            ("5000".into(), 20_000_00, false),  // Dr COGS
        ]
    );

    // P7: selling 200 with 60 on hand is rejected, in one piece.
    let inv2 = create_invoice(
        &mut w.conn, &w.company, &w.customer, "invoice", "2026-07-05", "2026-07-19",
        &[InvoiceLineInput {
            product_id: Some(product),
            description: "Peak Milk Carton".into(),
            quantity_milli: 200_000,
            unit_price_kobo: 900_00,
            discount_bp: 0,
            vat_applied: true,
            income_account_id: None,
        }],
        &ctx,
    ).unwrap();
    match post_invoice(&mut w.conn, inv2.as_str(), &ctx) {
        Err(EngineError::InsufficientStock { short_milli }) => assert_eq!(short_milli, 140_000),
        other => panic!("expected InsufficientStock, got {other:?}"),
    }
    assert_eq!(invoice_status(&w.conn, &inv2), "draft", "failed post leaves draft untouched");
    assert_eq!(trial_balance(&w.conn), 0);
}

// ===== postBill (Spec 01 §6.4) =====

#[test]
fn bill_template_claimable_vs_absorbed_vat() {
    let mut w = world();
    let ctx = PostCtx::default();
    let rent = acct_by_code(&w.conn, &w.company, "6100");
    let power = acct_by_code(&w.conn, &w.company, "6200");

    // Line 1: ₦200,000 rent, VAT charged, claimable (NTA 2025 default).
    // Line 2: ₦80,000 diesel, VAT charged, advisor overrode claimable → absorbed.
    let bill = create_bill(
        &mut w.conn, &w.company, &w.supplier, "2026-07-04", "2026-08-03", false, None,
        &[
            BillLineInput {
                product_id: None, description: "Office rent".into(),
                quantity_milli: 1_000, unit_cost_kobo: 200_000_00,
                vat_charged: true, vat_claimable: true, expense_account_id: rent,
            },
            BillLineInput {
                product_id: None, description: "Diesel".into(),
                quantity_milli: 1_000, unit_cost_kobo: 80_000_00,
                vat_charged: true, vat_claimable: false, expense_account_id: power,
            },
        ],
        &ctx,
    ).unwrap();
    let entry = post_bill(&mut w.conn, bill.as_str(), &ctx).unwrap();

    // VAT: 15,000 on rent (claimed) + 6,000 on diesel (absorbed into 6200).
    assert_eq!(
        entry_lines(&w.conn, &entry),
        vec![
            ("1310".into(), 15_000_00, false),   // Dr VAT Input (claimable only)
            ("2100".into(), -301_000_00, true),  // Cr AP gross, contact (P8)
            ("6100".into(), 200_000_00, false),  // Dr rent net
            ("6200".into(), 86_000_00, false),   // Dr diesel net + absorbed VAT
        ]
    );
}

// ===== postPayment in (Spec 01 §6.2, Spec 03 §5) =====

#[test]
fn payment_in_partial_then_full_with_wht_and_deposit() {
    let mut w = world();
    let ctx = PostCtx::default();
    let inv = create_invoice(
        &mut w.conn, &w.company, &w.customer, "invoice", "2026-07-04", "2026-08-03",
        &[line("Consulting", 1_000, 1_000_000_00, true)], &ctx,
    ).unwrap();
    post_invoice(&mut w.conn, inv.as_str(), &ctx).unwrap();
    // total = 1,075,000

    // Partial: ₦500,000 cash.
    let r1 = post_payment_in(
        &mut w.conn, &w.company, &w.customer, &w.bank, "2026-07-10",
        500_000_00, 0,
        &[Allocation { target_id: inv.clone(), amount_kobo: 500_000_00 }],
        &ctx,
    ).unwrap();
    assert_eq!(r1.receipt_number.as_deref(), Some("RCT-000001"));
    assert_eq!(invoice_status(&w.conn, &inv), "partially_paid");

    // Settle: customer withholds 5% WHT on the ex-VAT million = ₦50,000;
    // pays 525,000 cash + 50,000 WHT credit, and 25,000 extra as a deposit.
    let r2 = post_payment_in(
        &mut w.conn, &w.company, &w.customer, &w.bank, "2026-07-20",
        550_000_00, 50_000_00,
        &[Allocation { target_id: inv.clone(), amount_kobo: 575_000_00 }],
        &ctx,
    ).unwrap();
    assert_eq!(r2.deposit_kobo, 25_000_00);
    assert_eq!(invoice_status(&w.conn, &inv), "paid");
    // Spec 01 §6.2 template, exactly (GTBank is the first-added bank → code 1010):
    assert_eq!(
        entry_lines(&w.conn, &r2.entry_id),
        vec![
            ("1010".into(), 550_000_00, false),  // Dr Bank — cash received
            ("1100".into(), -575_000_00, true),  // Cr AR — Σ allocations, contact (P8)
            ("1320".into(), 50_000_00, false),   // Dr WHT Receivable — credit in kind
            ("2300".into(), -25_000_00, true),   // Cr Unearned Revenue — deposit remainder
        ]
    );
    assert_eq!(r2.receipt_number.as_deref(), Some("RCT-000002"));
    assert_eq!(trial_balance(&w.conn), 0);
}

#[test]
fn payment_in_over_allocation_rejected() {
    let mut w = world();
    let ctx = PostCtx::default();
    let inv = create_invoice(
        &mut w.conn, &w.company, &w.customer, "invoice", "2026-07-04", "2026-08-03",
        &[line("Goods", 1_000, 100_000_00, false)], &ctx,
    ).unwrap();
    post_invoice(&mut w.conn, inv.as_str(), &ctx).unwrap();
    let err = post_payment_in(
        &mut w.conn, &w.company, &w.customer, &w.bank, "2026-07-10",
        200_000_00, 0,
        &[Allocation { target_id: inv, amount_kobo: 150_000_00 }],
        &ctx,
    );
    assert!(err.is_err());
}

// ===== postPayment out: WHT split + exemption ladder (Spec 01 §6.2, Spec 04 §3) =====

fn wht_bill(w: &mut World, subtotal: i64, rate_bp: i64, ctx: &PostCtx) -> String {
    let fees = acct_by_code(&w.conn, &w.company, "6600");
    let bill = create_bill(
        &mut w.conn, &w.company, &w.supplier, "2026-07-04", "2026-08-03", true, Some(rate_bp),
        &[BillLineInput {
            product_id: None, description: "Services".into(),
            quantity_milli: 1_000, unit_cost_kobo: subtotal,
            vat_charged: true, vat_claimable: true, expense_account_id: fees,
        }],
        ctx,
    ).unwrap();
    post_bill(&mut w.conn, bill.as_str(), ctx).unwrap();
    bill
}

#[test]
fn payment_out_splits_wht_at_payment() {
    let mut w = world(); // cit_exempt = false → no exemption
    let ctx = PostCtx::default();
    let bill = wht_bill(&mut w, 1_000_000_00, 500, &ctx); // total 1,075,000

    let r = post_payment_out(
        &mut w.conn, &w.company, &w.supplier, &w.bank, "2026-07-15",
        &[Allocation { target_id: bill.clone(), amount_kobo: 1_075_000_00 }],
        WhtMode::Auto, &ctx,
    ).unwrap();
    // WHT = 5% of ex-VAT 1,000,000 = 50,000; cash = 1,025,000.
    assert_eq!(r.wht_kobo, 50_000_00);
    let lines = entry_lines(&w.conn, &r.entry_id);
    assert!(lines.contains(&("2100".into(), 1_075_000_00, true)));  // Dr AP gross
    assert!(lines.contains(&("2220".into(), -50_000_00, false)));   // Cr WHT Payable
    assert!(lines.iter().any(|(_, amt, _)| *amt == -1_025_000_00)); // Cr Bank net
    let status: String = w.conn.query_row(
        "SELECT status FROM bills WHERE id = ?1", params![bill], |r| r.get(0),
    ).unwrap();
    assert_eq!(status, "paid");
}

#[test]
fn payment_out_small_company_exemption_applies() {
    // cit_exempt company, supplier has TIN, month aggregate ≤ ₦2M → no deduction.
    let mut w = world_with(CompanyConfig { cit_exempt: true, ..Default::default() });
    let ctx = PostCtx::default();
    let bill = wht_bill(&mut w, 1_000_000_00, 500, &ctx); // gross 1,075,000 ≤ 2M

    let r = post_payment_out(
        &mut w.conn, &w.company, &w.supplier, &w.bank, "2026-07-15",
        &[Allocation { target_id: bill, amount_kobo: 1_075_000_00 }],
        WhtMode::Auto, &ctx,
    ).unwrap();
    assert_eq!(r.wht_kobo, 0, "₦2M/TIN exemption (WHT Regs 2024)");
}

#[test]
fn payment_out_exemption_lost_above_2m_month_aggregate() {
    let mut w = world_with(CompanyConfig { cit_exempt: true, ..Default::default() });
    let ctx = PostCtx::default();
    let b1 = wht_bill(&mut w, 1_500_000_00, 500, &ctx); // gross 1,612,500
    let b2 = wht_bill(&mut w, 1_000_000_00, 500, &ctx); // gross 1,075,000

    // First payment inside the exemption.
    let r1 = post_payment_out(
        &mut w.conn, &w.company, &w.supplier, &w.bank, "2026-07-10",
        &[Allocation { target_id: b1, amount_kobo: 1_612_500_00 }],
        WhtMode::Auto, &ctx,
    ).unwrap();
    assert_eq!(r1.wht_kobo, 0);

    // Second same-month payment pushes the aggregate past ₦2M → WHT applies.
    let r2 = post_payment_out(
        &mut w.conn, &w.company, &w.supplier, &w.bank, "2026-07-20",
        &[Allocation { target_id: b2, amount_kobo: 1_075_000_00 }],
        WhtMode::Auto, &ctx,
    ).unwrap();
    assert_eq!(r2.wht_kobo, 50_000_00, "calendar-month aggregate breached");
}

#[test]
fn payment_out_no_tin_requires_explicit_decision() {
    let mut w = world();
    let ctx = PostCtx::default();
    let no_tin = contact(&w.conn, &w.company, "Cash Vendor", "supplier", None);
    let fees = acct_by_code(&w.conn, &w.company, "6600");
    let bill = create_bill(
        &mut w.conn, &w.company, &no_tin, "2026-07-04", "2026-08-03", true, Some(500),
        &[BillLineInput {
            product_id: None, description: "Services".into(),
            quantity_milli: 1_000, unit_cost_kobo: 100_000_00,
            vat_charged: false, vat_claimable: false, expense_account_id: fees,
        }],
        &ctx,
    ).unwrap();
    post_bill(&mut w.conn, bill.as_str(), &ctx).unwrap();

    match post_payment_out(
        &mut w.conn, &w.company, &no_tin, &w.bank, "2026-07-15",
        &[Allocation { target_id: bill, amount_kobo: 100_000_00 }],
        WhtMode::Auto, &ctx,
    ) {
        Err(EngineError::WhtDecisionRequired { suggested_kobo }) => {
            assert_eq!(suggested_kobo, 10_000_00, "2× the 5% rate, offered never silent");
        }
        other => panic!("expected WhtDecisionRequired, got {other:?}"),
    }
}

// ===== transfer, drawing, void =====

#[test]
fn transfer_with_fee_never_income_never_expense_except_fee() {
    let mut w = world();
    let ctx = PostCtx::default();
    // Fund the bank first (owner puts money in).
    post_drawing(&mut w.conn, &w.company, &w.bank, "2026-07-01", 500_000_00, false, &ctx).unwrap();

    let entry = post_transfer(
        &mut w.conn, &w.company, &w.bank, &w.cash, "2026-07-04", 100_000_00, 50_00, &ctx,
    ).unwrap();
    let lines = entry_lines(&w.conn, &entry);
    assert_eq!(lines.len(), 3);
    assert!(lines.contains(&("6900".into(), 50_00, false))); // fee → Bank & POS Charges
    assert_eq!(trial_balance(&w.conn), 0);
}

#[test]
fn cash_box_cannot_go_negative_but_bank_can() {
    let mut w = world();
    let ctx = PostCtx::default();
    // Petty cash holds nothing: moving money OUT of it must fail (B1)…
    let err = post_transfer(
        &mut w.conn, &w.company, &w.cash, &w.bank, "2026-07-04", 10_000_00, 0, &ctx,
    );
    assert!(matches!(err, Err(EngineError::Validation(_))));
    // …while the bank account may overdraw (warn upstream, allow here).
    post_transfer(
        &mut w.conn, &w.company, &w.bank, &w.cash, "2026-07-04", 10_000_00, 0, &ctx,
    ).unwrap();
}

#[test]
fn drawing_out_hits_drawings_never_expenses() {
    let mut w = world();
    let ctx = PostCtx::default();
    post_drawing(&mut w.conn, &w.company, &w.bank, "2026-07-01", 300_000_00, false, &ctx).unwrap();
    let entry = post_drawing(&mut w.conn, &w.company, &w.bank, "2026-07-04", 120_000_00, true, &ctx).unwrap();
    let lines = entry_lines(&w.conn, &entry);
    assert!(lines.contains(&("3200".into(), 120_000_00, false)), "Dr Owner's Drawings");
    assert!(!lines.iter().any(|(code, _, _)| code.starts_with('6')), "never an expense");
}

#[test]
fn void_creates_cross_linked_negated_reversal() {
    let mut w = world();
    let ctx = PostCtx::default();
    let inv = create_invoice(
        &mut w.conn, &w.company, &w.customer, "invoice", "2026-07-04", "2026-08-03",
        &[line("Goods", 1_000, 250_000_00, true)], &ctx,
    ).unwrap();
    let entry = post_invoice(&mut w.conn, inv.as_str(), &ctx).unwrap().unwrap();

    let rev = void_entry(&mut w.conn, &entry, "2026-07-05", &ctx).unwrap();
    let orig: Vec<_> = entry_lines(&w.conn, &entry);
    let reversed: Vec<_> = entry_lines(&w.conn, &rev);
    for (code, amt, _) in &orig {
        assert!(reversed.iter().any(|(c, a, _)| c == code && *a == -amt), "negated {code}");
    }
    // Cross-links + double-void refusal.
    let (fwd, back): (Option<String>, Option<String>) = w.conn.query_row(
        "SELECT (SELECT reversed_by_entry_id FROM journal_entries WHERE id = ?1),
                (SELECT reverses_entry_id FROM journal_entries WHERE id = ?2)",
        params![entry, rev], |r| Ok((r.get(0)?, r.get(1)?)),
    ).unwrap();
    assert_eq!(fwd.as_deref(), Some(rev.as_str()));
    assert_eq!(back.as_deref(), Some(entry.as_str()));
    assert!(void_entry(&mut w.conn, &entry, "2026-07-06", &ctx).is_err());
    assert_eq!(trial_balance(&w.conn), 0);
}

// ===== soft close (P4) =====

#[test]
fn soft_close_requires_confirmation_then_proceeds() {
    let mut w = world();
    let ctx = PostCtx::default();
    w.conn.execute(
        "UPDATE companies SET soft_close_through = '2026-06-30' WHERE id = ?1",
        params![w.company],
    ).unwrap();
    let err = post_drawing(&mut w.conn, &w.company, &w.bank, "2026-06-15", 10_000_00, false, &ctx);
    assert!(matches!(err, Err(EngineError::SoftCloseConfirmationRequired)));

    let confirmed = PostCtx { confirm_soft_close: true, ..Default::default() };
    post_drawing(&mut w.conn, &w.company, &w.bank, "2026-06-15", 10_000_00, false, &confirmed).unwrap();
}
