//! Company creation + seed data (Spec 02 §3–§4): the full COA, document
//! sequences, and WHT rate presets, committed atomically (W1) — a
//! half-configured company is unrepresentable.

use crate::ids::{new_id, now_iso};
use crate::posting::PostError;
use rusqlite::{params, Connection, TransactionBehavior};

/// (code, name, class, system_key) — Spec 02 §3, verbatim.
/// Bank/cash accounts (1010–1099) are NOT here: wizard/`add_bank_account` creates them.
const COA_SEED: &[(&str, &str, &str, Option<&str>)] = &[
    // 1000s Assets
    ("1100", "Accounts Receivable", "asset", Some("AR")),
    ("1200", "Inventory", "asset", Some("INVENTORY")),
    ("1310", "VAT Receivable (Input)", "asset", Some("VAT_INPUT")),
    ("1320", "WHT Receivable", "asset", Some("WHT_RECEIVABLE")),
    ("1400", "Prepayments & Deposits Paid", "asset", None),
    ("1450", "Staff Loans & Advances", "asset", None),
    ("1510", "Motor Vehicles", "asset", None),
    ("1520", "Furniture & Fittings", "asset", None),
    ("1530", "Computers & Office Equipment", "asset", None),
    ("1540", "Plant & Machinery", "asset", None),
    ("1550", "Generators & Power Equipment", "asset", None),
    ("1590", "Accumulated Depreciation", "asset", None), // contra; signed balances make it work
    // 2000s Liabilities
    ("2100", "Accounts Payable", "liability", Some("AP")),
    ("2210", "VAT Payable (Output)", "liability", Some("VAT_OUTPUT")),
    ("2220", "WHT Payable", "liability", Some("WHT_PAYABLE")),
    ("2230", "PAYE Payable", "liability", None),
    ("2240", "Pension Payable", "liability", None),
    ("2250", "Company Income Tax Payable", "liability", None),
    ("2300", "Customer Deposits (Unearned Revenue)", "liability", Some("UNEARNED_REVENUE")),
    ("2410", "Bank Loans & Overdrafts", "liability", None),
    ("2420", "Director's / Owner's Loan", "liability", None),
    ("2500", "Accrued Expenses", "liability", None),
    // 3000s Equity
    ("3000", "Opening Balance Equity", "equity", Some("OPENING_BALANCE_EQUITY")),
    ("3100", "Owner's Capital", "equity", Some("OWNER_CAPITAL")),
    ("3200", "Owner's Drawings", "equity", Some("OWNER_DRAWINGS")),
    ("3900", "Retained Earnings", "equity", Some("RETAINED_EARNINGS")),
    // 4000s Revenue
    ("4000", "Sales Revenue", "income", Some("SALES_DEFAULT")),
    ("4100", "Service Income", "income", None),
    ("4200", "Other Income", "income", None),
    ("4300", "Interest Income", "income", None),
    ("4900", "FX Gain/Loss", "income", Some("FX_GAIN_LOSS")),
    // 5000s COGS
    ("5000", "Cost of Goods Sold", "cogs", Some("COGS_DEFAULT")),
    ("5100", "Purchases (Goods for Resale)", "cogs", None),
    ("5200", "Carriage Inwards & Clearing", "cogs", None),
    ("5300", "Direct Labour", "cogs", None),
    // 6000s OpEx
    ("6000", "Salaries & Wages", "expense", None),
    ("6100", "Rent", "expense", None),
    ("6200", "Utilities & Power", "expense", None), // planning-doc-mandated distinct line
    ("6300", "Transport & Logistics", "expense", None),
    ("6400", "Marketing & Advertising", "expense", None),
    ("6500", "Communication & Internet", "expense", None),
    ("6650", "Licenses, Levies & Permits", "expense", None),
    ("6600", "Professional & Legal Fees", "expense", None),
    ("6700", "Repairs & Maintenance", "expense", None),
    ("6750", "Insurance", "expense", None),
    ("6800", "Office Supplies & Consumables", "expense", None),
    ("6850", "Staff Welfare & Entertainment", "expense", None),
    ("6870", "Security Services", "expense", None),
    ("6900", "Bank & POS Charges", "expense", Some("BANK_CHARGES")),
    ("6930", "Bad Debts Written Off", "expense", None),
    ("6950", "Depreciation Expense", "expense", None),
    ("6980", "Miscellaneous Expenses", "expense", None),
    ("6990", "Rounding Differences", "expense", Some("ROUNDING")),
];

pub const COA_SEED_COUNT: usize = 53; // 17 system + 36 user-space

/// ⚠ Provisional WHT Regulations 2024 figures — seed DATA, editable in settings;
/// Spec 02 decision #9: confirm against the current schedule before ship.
const WHT_PRESETS: &[(&str, i64)] = &[
    ("Supply of goods", 200),
    ("Services / contracts (general)", 200),
    ("Professional & consultancy fees", 500),
    ("Rent & hire of equipment", 1000),
    ("Interest & dividends", 1000),
    ("Construction", 200),
];

#[derive(Debug, Clone)]
pub struct CompanyConfig {
    pub name: String,
    pub vat_registered: bool,
    pub vat_exempt: bool,
    pub cit_exempt: bool,
    pub inventory_enabled: bool,
    pub fiscal_year_start_month: u8,
    pub business_type: String,
    pub tin: Option<String>,
    /// First invoice number for migrating businesses (Spec 02 §5.7); default 1.
    pub invoice_start: i64,
    /// Spec 10 §3: stored identifier, offline, unvalidated in v1.
    pub license_key: Option<String>,
}

impl Default for CompanyConfig {
    fn default() -> Self {
        Self {
            name: "Test Company".into(),
            vat_registered: true,
            vat_exempt: false,
            cit_exempt: false,
            inventory_enabled: false,
            fiscal_year_start_month: 1,
            business_type: "trading".into(),
            tin: None,
            invoice_start: 1,
            license_key: None,
        }
    }
}

/// Create a company with full seed, atomically (Spec 02 W1). Returns company id.
pub fn create_company(conn: &mut Connection, cfg: &CompanyConfig) -> Result<String, PostError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let company_id = new_id();
    let now = now_iso();

    tx.execute(
        "INSERT INTO companies (id, name, tin, vat_registered, vat_exempt, cit_exempt,
                                inventory_enabled, fiscal_year_start_month, business_type,
                                license_key, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            company_id, cfg.name, cfg.tin,
            cfg.vat_registered as i64, cfg.vat_exempt as i64, cfg.cit_exempt as i64,
            cfg.inventory_enabled as i64, cfg.fiscal_year_start_month as i64,
            cfg.business_type, cfg.license_key, now
        ],
    )?;

    let mut misc_id = None;
    let mut other_income_id = None;
    for (code, name, class, system_key) in COA_SEED {
        let id = new_id();
        tx.execute(
            "INSERT INTO accounts (id, company_id, code, name, class, system_key, is_bank, is_system)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, ?7)",
            params![id, company_id, code, name, class, system_key, system_key.is_some() as i64],
        )?;
        match *code {
            "6980" => misc_id = Some(id),
            "4200" => other_income_id = Some(id),
            _ => {}
        }
    }

    // Write-off routing settings, seeded per approved Spec 04 #7 (advisor-editable later).
    tx.execute(
        "UPDATE companies SET writeoff_debit_account_id = ?2, writeoff_credit_account_id = ?3 WHERE id = ?1",
        params![company_id, misc_id, other_income_id],
    )?;

    for (doc_type, prefix, start) in [
        ("invoice", "INV-", cfg.invoice_start),
        ("quote", "QUO-", 1),
        ("receipt", "RCT-", 1),
    ] {
        tx.execute(
            "INSERT INTO document_sequences (company_id, doc_type, prefix, next_number)
             VALUES (?1, ?2, ?3, ?4)",
            params![company_id, doc_type, prefix, start],
        )?;
    }

    for (label, rate_bp) in WHT_PRESETS {
        tx.execute(
            "INSERT INTO wht_rate_presets (id, company_id, label, rate_bp) VALUES (?1, ?2, ?3, ?4)",
            params![new_id(), company_id, label, rate_bp],
        )?;
    }

    tx.commit()?;
    Ok(company_id)
}

/// Create a bank/cash account: COA row (next free code in 1010–1099, `is_bank`)
/// + `bank_accounts` metadata row (Spec 02 §5.4). Opening balances post separately
/// via the opening journal — never stored as columns.
pub fn add_bank_account(
    conn: &mut Connection,
    company_id: &str,
    label: &str,
    kind: &str,
    currency: &str,
) -> Result<String, PostError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let max_code: Option<String> = tx
        .query_row(
            "SELECT MAX(code) FROM accounts WHERE company_id = ?1 AND code >= '1010' AND code < '1100'",
            params![company_id],
            |r| r.get(0),
        )?;
    let next_code = match max_code {
        Some(c) => {
            let n: i64 = c.parse().map_err(|_| {
                PostError::Validation(format!("non-numeric bank account code: {c}"))
            })?;
            format!("{}", n + 10)
        }
        None => "1010".to_string(),
    };
    if next_code.as_str() >= "1100" {
        return Err(PostError::Validation(
            "bank account code band (1010-1099) exhausted".into(),
        ));
    }

    let account_id = new_id();
    tx.execute(
        "INSERT INTO accounts (id, company_id, code, name, class, is_bank, is_system)
         VALUES (?1, ?2, ?3, ?4, 'asset', 1, 0)",
        params![account_id, company_id, next_code, label],
    )?;
    let bank_id = new_id();
    tx.execute(
        "INSERT INTO bank_accounts (id, company_id, account_id, label, kind, currency)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![bank_id, company_id, account_id, label, kind, currency],
    )?;
    tx.commit()?;
    Ok(bank_id)
}
