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
    let id = create_company_in(&tx, cfg)?;
    tx.commit()?;
    Ok(id)
}

/// Company + seed inside the caller's open transaction (wizard composition).
pub fn create_company_in(tx: &Connection, cfg: &CompanyConfig) -> Result<String, PostError> {
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
    let (bank_id, _) = add_bank_account_in(&tx, company_id, label, kind, currency)?;
    tx.commit()?;
    Ok(bank_id)
}

/// Bank/cash account inside the caller's transaction. Returns (bank_account_id, coa_account_id).
pub fn add_bank_account_in(
    tx: &Connection,
    company_id: &str,
    label: &str,
    kind: &str,
    currency: &str,
) -> Result<(String, String), PostError> {
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
    Ok((bank_id, account_id))
}

// ===== Full wizard commit (Spec 02 W1 + Spec 10 §5b EULA record) =====

#[derive(Debug, Clone)]
pub struct SetupBank {
    pub label: String,
    pub kind: String, // 'bank' | 'cash' | 'pos_wallet' | 'domiciliary'
    pub currency: String,
    pub opening_balance_kobo: i64, // may be negative (overdraft)
}

#[derive(Debug, Clone)]
pub struct SetupContact {
    pub name: String,
    pub phone: Option<String>,
    pub amount_kobo: i64, // owed to us (customers) / owed by us (suppliers)
}

#[derive(Debug, Clone)]
pub struct FullSetup {
    pub company: CompanyConfig,
    pub banks: Vec<SetupBank>,
    pub customers_owing: Vec<SetupContact>,
    pub suppliers_owed: Vec<SetupContact>,
    pub opening_date: String,
    pub owner_name: String,
    pub staff_name: Option<String>,
    /// Advisor Mode PIN, 6 digits (Spec 02 §5.8) — argon2-hashed, stored on the owner row.
    pub advisor_pin: String,
    /// Accepted EULA version (Spec 10 §5b) — acceptance is the gate; recorded append-only.
    pub eula_version: String,
}

/// The whole wizard in ONE transaction: company, COA, banks, contacts, opening
/// journal (OBE plug), users, EULA acceptance. Cancel = nothing exists (W1).
pub fn create_company_full(conn: &mut Connection, s: &FullSetup) -> Result<String, PostError> {
    if s.banks.is_empty() {
        return Err(PostError::Validation("at least one bank or cash account is needed".into()));
    }
    if s.owner_name.trim().is_empty() {
        return Err(PostError::Validation("the owner's name is needed".into()));
    }
    if s.advisor_pin.len() != 6 || !s.advisor_pin.chars().all(|c| c.is_ascii_digit()) {
        return Err(PostError::Validation("the advisor PIN must be exactly 6 digits".into()));
    }
    if s.eula_version.trim().is_empty() {
        return Err(PostError::Validation("the agreement must be accepted first".into()));
    }
    for c in s.customers_owing.iter().chain(&s.suppliers_owed) {
        if c.name.trim().is_empty() || c.amount_kobo <= 0 {
            return Err(PostError::Validation("each opening balance needs a name and a positive amount".into()));
        }
    }

    let pin_hash = {
        use argon2::password_hash::{rand_core::OsRng, SaltString};
        use argon2::{Argon2, PasswordHasher};
        let salt = SaltString::generate(&mut OsRng);
        Argon2::default()
            .hash_password(s.advisor_pin.as_bytes(), &salt)
            .map_err(|e| PostError::Validation(format!("PIN hashing failed: {e}")))?
            .to_string()
    };

    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let company_id = create_company_in(&tx, &s.company)?;
    let now = now_iso();

    let sys_acct = |key: &str| -> Result<String, PostError> {
        Ok(tx.query_row(
            "SELECT id FROM accounts WHERE company_id = ?1 AND system_key = ?2",
            params![company_id, key],
            |r| r.get(0),
        )?)
    };

    // Banks + opening lines.
    let mut je_lines: Vec<crate::posting::LineSpec> = Vec::new();
    for b in &s.banks {
        let (_bank_id, coa_id) = add_bank_account_in(&tx, &company_id, &b.label, &b.kind, &b.currency)?;
        if b.opening_balance_kobo != 0 {
            je_lines.push(crate::posting::LineSpec::new(&coa_id, b.opening_balance_kobo));
        }
    }

    // Contacts + per-contact AR/AP opening lines (P8: subledger dimension from day one).
    let ar = sys_acct("AR")?;
    let ap = sys_acct("AP")?;
    let mut mk_contact = |name: &str, phone: &Option<String>, kind: &str| -> Result<String, PostError> {
        let id = new_id();
        tx.execute(
            "INSERT INTO contacts (id, company_id, kind, name, phone, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![id, company_id, kind, name.trim(), phone, now],
        )?;
        Ok(id)
    };
    for c in &s.customers_owing {
        let cid = mk_contact(&c.name, &c.phone, "customer")?;
        je_lines.push(crate::posting::LineSpec::with_contact(&ar, c.amount_kobo, &cid));
    }
    for c in &s.suppliers_owed {
        let cid = mk_contact(&c.name, &c.phone, "supplier")?;
        je_lines.push(crate::posting::LineSpec::with_contact(&ap, -c.amount_kobo, &cid));
    }

    // OBE plug — balanced by construction (Spec 02 §5.6).
    let sum: i64 = je_lines.iter().map(|l| l.amount_kobo).sum();
    if sum != 0 {
        je_lines.push(crate::posting::LineSpec::new(&sys_acct("OPENING_BALANCE_EQUITY")?, -sum));
    }
    if je_lines.len() >= 2 {
        crate::posting::post_entry_in(
            &tx, &company_id, &s.opening_date, "Opening balances at setup",
            "opening_balance", None, None, &je_lines,
        )?;
    }

    // Users: owner (carries the argon2 advisor-PIN hash), optional accounts officer.
    let owner_id = new_id();
    tx.execute(
        "INSERT INTO users (id, company_id, name, role, pin_hash, created_at)
         VALUES (?1, ?2, ?3, 'owner', ?4, ?5)",
        params![owner_id, company_id, s.owner_name.trim(), pin_hash, now],
    )?;
    if let Some(staff) = &s.staff_name {
        if !staff.trim().is_empty() {
            tx.execute(
                "INSERT INTO users (id, company_id, name, role, created_at)
                 VALUES (?1, ?2, ?3, 'staff', ?4)",
                params![new_id(), company_id, staff.trim(), now],
            )?;
        }
    }

    // EULA acceptance: append-only evidence (Spec 10 §5b).
    tx.execute(
        "INSERT INTO audit_log (id, company_id, user_id, action, entity_type, entity_id, detail_json, created_at)
         VALUES (?1, ?2, ?3, 'eula.accepted', 'company', ?2, ?4, ?5)",
        params![new_id(), company_id, owner_id,
                format!("{{\"version\":\"{}\"}}", s.eula_version), now],
    )?;

    tx.commit()?;
    Ok(company_id)
}
