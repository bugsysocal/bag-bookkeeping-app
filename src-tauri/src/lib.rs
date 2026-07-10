//! LedgerOne desktop shell. Commands are thin wrappers over ledger-core;
//! every error crossing this boundary is CLIENT-FACING: plain-language,
//! lexicon-compliant (Spec 07 §4 — no debit/credit/journal/ledger/posting/
//! accrual/liability/equity in owner strings), recoverable, with the raw
//! technical detail carried separately for Advisor Mode display.

use ledger_core::auth::SessionStore;
use ledger_core::engine::{self, PostCtx};
use ledger_core::rusqlite::{self, Connection, OptionalExtension};
use ledger_core::seed::{self, CompanyConfig};
use ledger_core::EngineError;
use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use tauri::Manager;

struct Db(Mutex<Connection>);

/// Who's using LedgerOne right now (Spec 07 §5). One process = one signed-in
/// user; see `ledger_core::auth` for the actual rules — this is IPC plumbing
/// only, per the project's one architectural law (business logic lives in
/// ledger-core, the shell just wraps it).
struct Sess(SessionStore);

/// Every mutating command starts here instead of `PostCtx { user_id: None, .. }`.
/// Turns the engine's session error into the same owner-language envelope as
/// everything else, and is the one place "no session ⇒ no post" is enforced.
fn ctx(sess: &tauri::State<Sess>, confirm_soft_close: bool) -> Result<PostCtx, CmdError> {
    let session = sess.0.require_session()?;
    Ok(PostCtx { user_id: Some(session.user_id), confirm_soft_close })
}

/// Same, but for the Spec 02 role matrix's owner/advisor-only actions (voids,
/// import-error resolution): staff is rejected HERE, before the engine call.
fn ctx_not_staff(sess: &tauri::State<Sess>, confirm_soft_close: bool) -> Result<PostCtx, CmdError> {
    let session = sess.0.require_not_staff()?;
    Ok(PostCtx { user_id: Some(session.user_id), confirm_soft_close })
}

/// Client-facing error envelope. `message` is what the owner reads —
/// plain business language, always with a next step. `detail` is the raw
/// technical error, shown only in Advisor Mode / support bundles.
#[derive(Serialize)]
pub struct CmdError {
    pub code: &'static str,
    pub message: String,
    pub detail: Option<String>,
}

impl From<EngineError> for CmdError {
    fn from(e: EngineError) -> Self {
        match &e {
            EngineError::SoftCloseConfirmationRequired => CmdError {
                code: "soft_close",
                message: "This date falls in a period your advisor has closed off. \
                          You can still record it — confirm to continue, and your advisor will see it was added later."
                    .into(),
                detail: None,
            },
            EngineError::WhtDecisionRequired { suggested_kobo } => CmdError {
                code: "wht_decision",
                message: format!(
                    "This supplier has no TIN. Tax rules say you should hold back double the normal tax \
                     (₦{}). Add their TIN, or choose how much tax to hold back before paying.",
                    naira(*suggested_kobo)
                ),
                detail: None,
            },
            EngineError::InsufficientStock { short_milli } => CmdError {
                code: "insufficient_stock",
                message: format!(
                    "You're selling more than you have in stock — short by {}. \
                     Record the purchase that brought the stock in first, or reduce the quantity.",
                    qty(*short_milli)
                ),
                detail: None,
            },
            EngineError::Validation(s) => CmdError {
                code: "invalid",
                message: "This couldn't be recorded — please check the details and try again."
                    .into(),
                detail: Some(s.clone()),
            },
            EngineError::Db(err) => CmdError {
                code: "storage",
                message: "Something went wrong while saving. Nothing was changed — your books are safe: \
                          every change saves completely or not at all."
                    .into(),
                detail: Some(err.to_string()),
            },
            EngineError::NoActiveSession => CmdError {
                code: "no_session",
                message: "Please choose who's using LedgerOne, then try again.".into(),
                detail: None,
            },
            EngineError::StaffForbidden => CmdError {
                code: "staff_forbidden",
                message: "Only the owner or the advisor can do that. Ask them, or switch to their login."
                    .into(),
                detail: None,
            },
            EngineError::AdvisorPinRequired => CmdError {
                code: "advisor_pin_required",
                message: "This needs the accountant's area unlocked first — enter the Advisor PIN.".into(),
                detail: None,
            },
            EngineError::AdvisorPinIncorrect { attempts_remaining } => CmdError {
                code: "advisor_pin_incorrect",
                message: format!(
                    "That PIN isn't right. {attempts_remaining} attempt(s) left before it locks for a while."
                ),
                detail: None,
            },
            EngineError::AdvisorLockedOut { minutes_left } => CmdError {
                code: "advisor_locked_out",
                message: format!(
                    "Too many wrong tries — the accountant's area is locked for about {minutes_left} more minute(s)."
                ),
                detail: None,
            },
        }
    }
}

fn db_err(e: rusqlite::Error) -> CmdError {
    CmdError::from(EngineError::Db(e))
}

fn naira(kobo: i64) -> String {
    let n = kobo / 100;
    let k = (kobo % 100).abs();
    let mut s = n.abs().to_string();
    let mut out = String::new();
    while s.len() > 3 {
        let rest = s.split_off(s.len() - 3);
        out = format!(",{rest}{out}");
    }
    format!("{}{}{}.{k:02}", if kobo < 0 { "-" } else { "" }, s, out)
}

fn qty(milli: i64) -> String {
    if milli % 1000 == 0 {
        format!("{}", milli / 1000)
    } else {
        format!("{:.3}", milli as f64 / 1000.0)
    }
}

// ===== DTOs =====

#[derive(Deserialize)]
pub struct NewCompany {
    pub name: String,
    pub vat_registered: bool,
    pub vat_exempt: bool,
    pub cit_exempt: bool,
    pub inventory_enabled: bool,
    pub fiscal_year_start_month: u8,
    pub business_type: String,
    pub tin: Option<String>,
    pub invoice_start: Option<i64>,
    pub license_key: Option<String>, // Spec 10 §3: stored, offline, unvalidated in v1
}

#[derive(Serialize)]
pub struct CompanyDto {
    pub id: String,
    pub name: String,
}

#[derive(Serialize)]
pub struct CashAccountDto {
    pub bank_account_id: String,
    pub label: String,
    pub kind: String,
    pub currency: String,
    pub balance_kobo: i64,
}

#[derive(Serialize)]
pub struct DashboardDto {
    pub cash_accounts: Vec<CashAccountDto>,
    pub cash_total_kobo: i64,
    pub who_owes_me_kobo: i64,
    pub what_i_owe_kobo: i64,
    /// Spec 07 §2.1 tile #3 named line: tax collected/held, not yet remitted.
    pub unremitted_tax_kobo: i64,
    pub profit_this_month_kobo: i64,
}

// ===== Commands =====

#[tauri::command]
fn create_company(state: tauri::State<Db>, input: NewCompany) -> Result<CompanyDto, CmdError> {
    let mut conn = state.0.lock().unwrap();
    let cfg = CompanyConfig {
        name: input.name.clone(),
        vat_registered: input.vat_registered,
        vat_exempt: input.vat_exempt,
        cit_exempt: input.cit_exempt,
        inventory_enabled: input.inventory_enabled,
        fiscal_year_start_month: input.fiscal_year_start_month,
        business_type: input.business_type,
        tin: input.tin,
        invoice_start: input.invoice_start.unwrap_or(1),
        license_key: input.license_key,
    };
    let id = seed::create_company(&mut conn, &cfg).map_err(EngineError::from)?;
    Ok(CompanyDto { id, name: input.name })
}

#[derive(Deserialize)]
pub struct SetupBankDto {
    pub label: String,
    pub kind: String,
    pub currency: String,
    pub opening_balance_kobo: i64,
}

#[derive(Deserialize)]
pub struct SetupContactDto {
    pub name: String,
    pub phone: Option<String>,
    pub amount_kobo: i64,
}

#[derive(Deserialize)]
pub struct FullSetupDto {
    pub company: NewCompany,
    pub banks: Vec<SetupBankDto>,
    pub customers_owing: Vec<SetupContactDto>,
    pub suppliers_owed: Vec<SetupContactDto>,
    pub opening_date: String,
    pub owner_name: String,
    pub staff_name: Option<String>,
    pub advisor_pin: String,
    pub eula_version: String,
}

/// The whole Spec 02 wizard, atomically (W1): cancel-anywhere leaves zero rows.
#[tauri::command]
fn create_company_full(
    state: tauri::State<Db>,
    sess: tauri::State<Sess>,
    input: FullSetupDto,
) -> Result<CompanyDto, CmdError> {
    let mut conn = state.0.lock().unwrap();
    let name = input.company.name.clone();
    let setup = seed::FullSetup {
        company: CompanyConfig {
            name: name.clone(),
            vat_registered: input.company.vat_registered,
            vat_exempt: input.company.vat_exempt,
            cit_exempt: input.company.cit_exempt,
            inventory_enabled: input.company.inventory_enabled,
            fiscal_year_start_month: input.company.fiscal_year_start_month,
            business_type: input.company.business_type,
            tin: input.company.tin,
            invoice_start: input.company.invoice_start.unwrap_or(1),
            license_key: input.company.license_key,
        },
        banks: input.banks.into_iter().map(|b| seed::SetupBank {
            label: b.label, kind: b.kind, currency: b.currency,
            opening_balance_kobo: b.opening_balance_kobo,
        }).collect(),
        customers_owing: input.customers_owing.into_iter().map(|c| seed::SetupContact {
            name: c.name, phone: c.phone, amount_kobo: c.amount_kobo,
        }).collect(),
        suppliers_owed: input.suppliers_owed.into_iter().map(|c| seed::SetupContact {
            name: c.name, phone: c.phone, amount_kobo: c.amount_kobo,
        }).collect(),
        opening_date: input.opening_date,
        owner_name: input.owner_name,
        staff_name: input.staff_name,
        advisor_pin: input.advisor_pin,
        eula_version: input.eula_version,
    };
    let id = seed::create_company_full(&mut conn, &setup).map_err(EngineError::from)?;
    // The wizard just created the owner row (Spec 02 W1) — sign them in immediately
    // so "establish a session" happens without a second manual step post-setup.
    let owner_id: String = conn.query_row(
        "SELECT id FROM users WHERE company_id = ?1 AND role = 'owner'",
        rusqlite::params![id], |r| r.get(0),
    ).map_err(db_err)?;
    sess.0.login(&conn, &owner_id)?;
    Ok(CompanyDto { id, name })
}

// ===== Session & Advisor Mode (Spec 07 §5) =====

#[derive(Serialize)]
struct UserDto { id: String, name: String, role: String }

#[tauri::command]
fn list_users(state: tauri::State<Db>, company_id: String) -> Result<Vec<UserDto>, CmdError> {
    let conn = state.0.lock().unwrap();
    let mut q = conn.prepare(
        "SELECT id, name, role FROM users WHERE company_id = ?1 ORDER BY
           CASE role WHEN 'owner' THEN 0 WHEN 'advisor' THEN 1 ELSE 2 END, name",
    ).map_err(db_err)?;
    let rows = q.query_map(rusqlite::params![company_id], |r| {
        Ok(UserDto { id: r.get(0)?, name: r.get(1)?, role: r.get(2)? })
    }).map_err(db_err)?.collect::<Result<Vec<_>, _>>().map_err(db_err)?;
    Ok(rows)
}

#[derive(Serialize)]
struct SessionDto { user_id: String, company_id: String, role: String, name: String, advisor_active: bool }

fn session_dto(conn: &Connection, sess: &Sess, session: ledger_core::auth::Session) -> Result<SessionDto, CmdError> {
    let advisor_active = sess.0.advisor_active(conn)?;
    Ok(SessionDto {
        user_id: session.user_id, company_id: session.company_id,
        role: session.role, name: session.name, advisor_active,
    })
}

/// Pick who's using LedgerOne (Spec 07 §5: attribution, never a password gate —
/// the PIN is reserved for Advisor Mode elevation below).
#[tauri::command]
fn login(state: tauri::State<Db>, sess: tauri::State<Sess>, user_id: String) -> Result<SessionDto, CmdError> {
    let conn = state.0.lock().unwrap();
    let session = sess.0.login(&conn, &user_id)?;
    session_dto(&conn, &sess, session)
}

#[tauri::command]
fn logout(sess: tauri::State<Sess>) {
    sess.0.logout();
}

/// Called on app start / window focus so the UI can restore or redirect to
/// the user picker without guessing at in-memory state.
#[tauri::command]
fn current_session(state: tauri::State<Db>, sess: tauri::State<Sess>) -> Result<Option<SessionDto>, CmdError> {
    let conn = state.0.lock().unwrap();
    match sess.0.current() {
        Some(session) => Ok(Some(session_dto(&conn, &sess, session)?)),
        None => Ok(None),
    }
}

/// PIN elevation over the CURRENT session — not a separate login (Spec 07 §5).
#[tauri::command]
fn advisor_enter(state: tauri::State<Db>, sess: tauri::State<Sess>, pin: String) -> Result<SessionDto, CmdError> {
    let conn = state.0.lock().unwrap();
    sess.0.advisor_enter(&conn, &pin)?;
    let session = sess.0.current().ok_or(EngineError::NoActiveSession)?;
    session_dto(&conn, &sess, session)
}

#[tauri::command]
fn advisor_exit(state: tauri::State<Db>, sess: tauri::State<Sess>) -> Result<(), CmdError> {
    let conn = state.0.lock().unwrap();
    sess.0.advisor_exit(&conn)?;
    Ok(())
}

#[tauri::command]
fn list_companies(state: tauri::State<Db>) -> Result<Vec<CompanyDto>, CmdError> {
    let conn = state.0.lock().unwrap();
    let mut q = conn
        .prepare("SELECT id, name FROM companies ORDER BY created_at")
        .map_err(db_err)?;
    let rows = q
        .query_map([], |r| Ok(CompanyDto { id: r.get(0)?, name: r.get(1)? }))
        .map_err(db_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_err)?;
    Ok(rows)
}

#[tauri::command]
fn add_bank_account(
    state: tauri::State<Db>,
    sess: tauri::State<Sess>,
    company_id: String,
    label: String,
    kind: String,
    currency: String,
) -> Result<String, CmdError> {
    sess.0.require_session()?;
    let mut conn = state.0.lock().unwrap();
    seed::add_bank_account(&mut conn, &company_id, &label, &kind, &currency)
        .map_err(|e| EngineError::from(e).into())
}

/// The five-tile dashboard (Spec 07 §2.1) — every number a live query.
#[tauri::command]
fn dashboard(state: tauri::State<Db>, company_id: String) -> Result<DashboardDto, CmdError> {
    let conn = state.0.lock().unwrap();

    let mut cash_accounts = Vec::new();
    let mut cash_total = 0i64;
    {
        let mut q = conn
            .prepare(
                "SELECT b.id, b.label, b.kind, b.currency,
                        COALESCE((SELECT SUM(l.amount_kobo) FROM journal_lines l
                                  JOIN journal_entries e ON e.id = l.entry_id
                                  WHERE l.account_id = b.account_id AND e.is_posted = 1), 0)
                 FROM bank_accounts b
                 WHERE b.company_id = ?1 AND b.is_active = 1
                 ORDER BY b.label",
            )
            .map_err(db_err)?;
        let rows = q
            .query_map([&company_id], |r| {
                Ok(CashAccountDto {
                    bank_account_id: r.get(0)?,
                    label: r.get(1)?,
                    kind: r.get(2)?,
                    currency: r.get(3)?,
                    balance_kobo: r.get(4)?,
                })
            })
            .map_err(db_err)?;
        for row in rows {
            let a = row.map_err(db_err)?;
            cash_total += a.balance_kobo;
            cash_accounts.push(a);
        }
    }

    let sys_balance = |key: &str| -> Result<i64, CmdError> {
        conn.query_row(
            "SELECT COALESCE(SUM(l.amount_kobo), 0)
             FROM journal_lines l
             JOIN journal_entries e ON e.id = l.entry_id
             JOIN accounts a ON a.id = l.account_id
             WHERE e.company_id = ?1 AND e.is_posted = 1 AND a.system_key = ?2",
            rusqlite::params![company_id, key],
            |r| r.get(0),
        )
        .map_err(db_err)
    };

    let who_owes_me = sys_balance("AR")?;
    let what_i_owe = -sys_balance("AP")?;
    // Tax collected/held, not yet remitted: VAT output + WHT payable (credit balances).
    let unremitted_tax = -(sys_balance("VAT_OUTPUT")? + sys_balance("WHT_PAYABLE")?);

    // Simplified accrual profit, current calendar month (fiscal quarters in Spec 05 impl).
    let month = &ledger_core::ids::now_iso()[0..7];
    let profit: i64 = conn
        .query_row(
            "SELECT COALESCE(-SUM(l.amount_kobo), 0)
             FROM journal_lines l
             JOIN journal_entries e ON e.id = l.entry_id
             JOIN accounts a ON a.id = l.account_id
             WHERE e.company_id = ?1 AND e.is_posted = 1
               AND a.class IN ('income','cogs','expense')
               AND substr(e.entry_date, 1, 7) = ?2",
            rusqlite::params![company_id, month],
            |r| r.get(0),
        )
        .map_err(db_err)?;

    Ok(DashboardDto {
        cash_accounts,
        cash_total_kobo: cash_total,
        who_owes_me_kobo: who_owes_me,
        what_i_owe_kobo: what_i_owe,
        unremitted_tax_kobo: unremitted_tax,
        profit_this_month_kobo: profit,
    })
}

/// "Owner took money out / put money in" — the guilt-free button, end to end.
#[tauri::command]
fn record_drawing(
    state: tauri::State<Db>,
    sess: tauri::State<Sess>,
    company_id: String,
    bank_account_id: String,
    date: String,
    amount_kobo: i64,
    out: bool,
    confirm_soft_close: bool,
) -> Result<String, CmdError> {
    let post_ctx = ctx(&sess, confirm_soft_close)?;
    let mut conn = state.0.lock().unwrap();
    engine::post_drawing(&mut conn, &company_id, &bank_account_id, &date, amount_kobo, out, &post_ctx)
        .map_err(Into::into)
}

// ===== Sales: contacts, invoices, payments (Spec 03) =====

#[derive(Serialize)]
struct ContactDto { id: String, name: String, phone: Option<String> }

#[tauri::command]
fn list_contacts(state: tauri::State<Db>, company_id: String, kind: Option<String>) -> Result<Vec<ContactDto>, CmdError> {
    let conn = state.0.lock().unwrap();
    let mut q = conn.prepare(
        "SELECT id, name, phone FROM contacts
         WHERE company_id = ?1 AND is_active = 1 AND (?2 IS NULL OR kind = ?2 OR kind = 'both')
         ORDER BY name",
    ).map_err(db_err)?;
    let rows = q.query_map(rusqlite::params![company_id, kind], |r| {
        Ok(ContactDto { id: r.get(0)?, name: r.get(1)?, phone: r.get(2)? })
    }).map_err(db_err)?.collect::<Result<Vec<_>, _>>().map_err(db_err)?;
    Ok(rows)
}

/// Inline create from pickers (Spec 07 §6): a real, deduped record — never a free-text string.
#[tauri::command]
fn create_contact(state: tauri::State<Db>, sess: tauri::State<Sess>, company_id: String, name: String, phone: Option<String>, kind: String) -> Result<ContactDto, CmdError> {
    sess.0.require_session()?;
    let conn = state.0.lock().unwrap();
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err(CmdError { code: "name_required", message: "Please enter a name.".into(), detail: None });
    }
    // Dedupe nudge (Spec 06 §3 rule): exact case/space-insensitive match blocks with guidance.
    let existing: Option<String> = conn.query_row(
        "SELECT name FROM contacts WHERE company_id = ?1 AND lower(trim(name)) = lower(?2) AND is_active = 1",
        rusqlite::params![company_id, name],
        |r| r.get(0),
    ).optional().map_err(db_err)?;
    if let Some(e) = existing {
        return Err(CmdError {
            code: "duplicate_contact",
            message: format!("'{e}' is already in your list — pick them instead of adding twice."),
            detail: None,
        });
    }
    let id = ledger_core::ids::new_id();
    conn.execute(
        "INSERT INTO contacts (id, company_id, kind, name, phone, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![id, company_id, kind, name, phone, ledger_core::ids::now_iso()],
    ).map_err(db_err)?;
    Ok(ContactDto { id, name, phone })
}

#[derive(Deserialize)]
pub struct InvoiceLineDto {
    pub description: String,
    pub quantity_milli: i64,
    pub unit_price_kobo: i64,
    pub discount_bp: i64,
    pub vat_applied: bool,
}

#[derive(Deserialize)]
pub struct NewInvoiceDto {
    pub company_id: String,
    pub contact_id: String,
    pub issue_date: String,
    pub due_date: String,
    pub lines: Vec<InvoiceLineDto>,
    /// 'invoice' (default) or 'quote' — quotes never post (Spec 03 §3).
    pub kind: Option<String>,
}

#[derive(Serialize)]
struct DraftDto { id: String, number: String }

#[tauri::command]
fn create_invoice_draft(state: tauri::State<Db>, sess: tauri::State<Sess>, input: NewInvoiceDto) -> Result<DraftDto, CmdError> {
    let ctx = ctx(&sess, false)?;
    let mut conn = state.0.lock().unwrap();
    let lines: Vec<engine::InvoiceLineInput> = input.lines.into_iter().map(|l| engine::InvoiceLineInput {
        product_id: None,
        description: l.description,
        quantity_milli: l.quantity_milli,
        unit_price_kobo: l.unit_price_kobo,
        discount_bp: l.discount_bp,
        vat_applied: l.vat_applied,
        income_account_id: None, // free-description lines default to 4000 SALES
    }).collect();
    let kind = input.kind.as_deref().unwrap_or("invoice");
    let id = engine::create_invoice(
        &mut conn, &input.company_id, &input.contact_id, kind,
        &input.issue_date, &input.due_date, &lines, &ctx,
    )?;
    let number: String = conn.query_row(
        "SELECT number FROM invoices WHERE id = ?1", rusqlite::params![id], |r| r.get(0),
    ).map_err(db_err)?;
    Ok(DraftDto { id, number })
}

#[tauri::command]
fn send_invoice(state: tauri::State<Db>, sess: tauri::State<Sess>, invoice_id: String, confirm_soft_close: bool) -> Result<(), CmdError> {
    let ctx = ctx(&sess, confirm_soft_close)?;
    let mut conn = state.0.lock().unwrap();
    engine::post_invoice(&mut conn, &invoice_id, &ctx)?;
    Ok(())
}

#[derive(Serialize)]
struct InvoiceRowDto {
    id: String, number: String, kind: String, customer: String,
    issue_date: String, due_date: String,
    total_kobo: i64, paid_kobo: i64, status: String,
    overdue: bool, // DERIVED, never stored (Spec 03 §2)
}

#[tauri::command]
fn list_invoices(state: tauri::State<Db>, company_id: String) -> Result<Vec<InvoiceRowDto>, CmdError> {
    let conn = state.0.lock().unwrap();
    let today = ledger_core::ids::now_iso()[..10].to_string();
    let mut q = conn.prepare(
        "SELECT i.id, i.number, i.kind, c.name, i.issue_date, i.due_date, i.total_kobo, i.amount_paid_kobo, i.status
         FROM invoices i JOIN contacts c ON c.id = i.contact_id
         WHERE i.company_id = ?1
         ORDER BY i.created_at DESC LIMIT 200",
    ).map_err(db_err)?;
    let rows = q.query_map(rusqlite::params![company_id], |r| {
        let status: String = r.get(8)?;
        let due: String = r.get(5)?;
        Ok(InvoiceRowDto {
            id: r.get(0)?, number: r.get(1)?, kind: r.get(2)?, customer: r.get(3)?,
            issue_date: r.get(4)?, due_date: due.clone(),
            total_kobo: r.get(6)?, paid_kobo: r.get(7)?,
            overdue: (status == "sent" || status == "partially_paid") && due < today,
            status,
        })
    }).map_err(db_err)?.collect::<Result<Vec<_>, _>>().map_err(db_err)?;
    Ok(rows)
}

#[derive(Serialize)]
struct OpenInvoiceDto { id: String, number: String, due_date: String, balance_kobo: i64 }

#[tauri::command]
fn open_invoices(state: tauri::State<Db>, company_id: String, contact_id: String) -> Result<Vec<OpenInvoiceDto>, CmdError> {
    let conn = state.0.lock().unwrap();
    let mut q = conn.prepare(
        "SELECT id, number, due_date, total_kobo - amount_paid_kobo
         FROM invoices
         WHERE company_id = ?1 AND contact_id = ?2 AND kind = 'invoice'
           AND status IN ('sent','partially_paid')
         ORDER BY due_date, number", // FIFO by due date (Spec 03 decision #3)
    ).map_err(db_err)?;
    let rows = q.query_map(rusqlite::params![company_id, contact_id], |r| {
        Ok(OpenInvoiceDto { id: r.get(0)?, number: r.get(1)?, due_date: r.get(2)?, balance_kobo: r.get(3)? })
    }).map_err(db_err)?.collect::<Result<Vec<_>, _>>().map_err(db_err)?;
    Ok(rows)
}

#[derive(Deserialize)]
pub struct AllocationDto { pub invoice_id: String, pub amount_kobo: i64 }

#[derive(Deserialize)]
pub struct PaymentInDto {
    pub company_id: String,
    pub contact_id: String,
    pub bank_account_id: String,
    pub payment_date: String,
    pub amount_kobo: i64,
    pub wht_kobo: i64,
    pub allocations: Vec<AllocationDto>,
    pub confirm_soft_close: bool,
}

#[derive(Serialize)]
struct PaymentDoneDto { receipt_number: Option<String>, deposit_kobo: i64, wht_kobo: i64 }

#[tauri::command]
fn record_payment_in(state: tauri::State<Db>, sess: tauri::State<Sess>, input: PaymentInDto) -> Result<PaymentDoneDto, CmdError> {
    let ctx = ctx(&sess, input.confirm_soft_close)?;
    let mut conn = state.0.lock().unwrap();
    let allocs: Vec<engine::Allocation> = input.allocations.into_iter()
        .map(|a| engine::Allocation { target_id: a.invoice_id, amount_kobo: a.amount_kobo })
        .collect();
    let res = engine::post_payment_in(
        &mut conn, &input.company_id, &input.contact_id, &input.bank_account_id,
        &input.payment_date, input.amount_kobo, input.wht_kobo, &allocs, None, &ctx,
    )?;
    Ok(PaymentDoneDto {
        receipt_number: res.receipt_number,
        deposit_kobo: res.deposit_kobo,
        wht_kobo: res.wht_kobo,
    })
}

#[derive(Serialize)]
struct BankDto { id: String, label: String, kind: String, currency: String }

#[tauri::command]
fn list_bank_accounts(state: tauri::State<Db>, company_id: String) -> Result<Vec<BankDto>, CmdError> {
    let conn = state.0.lock().unwrap();
    let mut q = conn.prepare(
        "SELECT id, label, kind, currency FROM bank_accounts
         WHERE company_id = ?1 AND is_active = 1 ORDER BY label",
    ).map_err(db_err)?;
    let rows = q.query_map(rusqlite::params![company_id], |r| {
        Ok(BankDto { id: r.get(0)?, label: r.get(1)?, kind: r.get(2)?, currency: r.get(3)? })
    }).map_err(db_err)?.collect::<Result<Vec<_>, _>>().map_err(db_err)?;
    Ok(rows)
}

// ===== Purchases: expenses, bills, supplier payments, transfers (Spec 04) =====

#[derive(Serialize)]
struct CategoryDto { id: String, code: String, name: String }

#[tauri::command]
fn expense_categories(state: tauri::State<Db>, company_id: String) -> Result<Vec<CategoryDto>, CmdError> {
    let conn = state.0.lock().unwrap();
    let mut q = conn.prepare(
        "SELECT id, code, name FROM accounts
         WHERE company_id = ?1 AND is_active = 1 AND class IN ('expense','cogs')
           AND (system_key IS NULL OR system_key NOT IN ('ROUNDING'))
         ORDER BY code",
    ).map_err(db_err)?;
    let rows = q.query_map(rusqlite::params![company_id], |r| {
        Ok(CategoryDto { id: r.get(0)?, code: r.get(1)?, name: r.get(2)? })
    }).map_err(db_err)?.collect::<Result<Vec<_>, _>>().map_err(db_err)?;
    Ok(rows)
}

#[derive(Serialize)]
struct WhtPresetDto { label: String, rate_bp: i64 }

#[tauri::command]
fn wht_presets(state: tauri::State<Db>, company_id: String) -> Result<Vec<WhtPresetDto>, CmdError> {
    let conn = state.0.lock().unwrap();
    let mut q = conn.prepare(
        "SELECT label, rate_bp FROM wht_rate_presets WHERE company_id = ?1 ORDER BY label",
    ).map_err(db_err)?;
    let rows = q.query_map(rusqlite::params![company_id], |r| {
        Ok(WhtPresetDto { label: r.get(0)?, rate_bp: r.get(1)? })
    }).map_err(db_err)?.collect::<Result<Vec<_>, _>>().map_err(db_err)?;
    Ok(rows)
}

#[derive(Deserialize)]
pub struct ExpenseDto {
    pub company_id: String,
    pub bank_account_id: String,
    pub payee: String,
    pub expense_account_id: String,
    pub date: String,
    pub gross_kobo: i64,
    pub vat_inclusive: bool,
    pub wht_kobo: i64,
    pub confirm_soft_close: bool,
}

#[tauri::command]
fn record_expense(state: tauri::State<Db>, sess: tauri::State<Sess>, input: ExpenseDto) -> Result<String, CmdError> {
    let ctx = ctx(&sess, input.confirm_soft_close)?;
    let mut conn = state.0.lock().unwrap();
    engine::post_expense(
        &mut conn, &input.company_id, &input.bank_account_id, &input.payee,
        &input.expense_account_id, &input.date, input.gross_kobo,
        input.vat_inclusive, input.wht_kobo, &ctx,
    ).map_err(Into::into)
}

#[derive(Deserialize)]
pub struct BillLineDto {
    pub description: String,
    pub quantity_milli: i64,
    pub unit_cost_kobo: i64,
    pub vat_charged: bool,
    pub vat_claimable: bool,
    pub expense_account_id: String,
}

#[derive(Deserialize)]
pub struct NewBillDto {
    pub company_id: String,
    pub contact_id: String,
    pub bill_date: String,
    pub due_date: String,
    pub wht_applicable: bool,
    pub wht_rate_bp: Option<i64>,
    pub lines: Vec<BillLineDto>,
    pub confirm_soft_close: bool,
}

/// Save-as-open: creates the bill AND posts it (Dr expense / Dr VAT input / Cr AP).
#[tauri::command]
fn create_bill(state: tauri::State<Db>, sess: tauri::State<Sess>, input: NewBillDto) -> Result<String, CmdError> {
    let ctx = ctx(&sess, input.confirm_soft_close)?;
    let mut conn = state.0.lock().unwrap();
    let lines: Vec<engine::BillLineInput> = input.lines.into_iter().map(|l| engine::BillLineInput {
        product_id: None,
        description: l.description,
        quantity_milli: l.quantity_milli,
        unit_cost_kobo: l.unit_cost_kobo,
        vat_charged: l.vat_charged,
        vat_claimable: l.vat_claimable,
        expense_account_id: l.expense_account_id,
    }).collect();
    let id = engine::create_bill(
        &mut conn, &input.company_id, &input.contact_id, &input.bill_date, &input.due_date,
        input.wht_applicable, input.wht_rate_bp, &lines, &ctx,
    )?;
    engine::post_bill(&mut conn, &id, &ctx)?;
    Ok(id)
}

#[derive(Serialize)]
struct BillRowDto {
    id: String, supplier: String, reference: Option<String>,
    bill_date: String, due_date: String,
    total_kobo: i64, paid_kobo: i64, status: String, overdue: bool,
}

#[tauri::command]
fn list_bills(state: tauri::State<Db>, company_id: String) -> Result<Vec<BillRowDto>, CmdError> {
    let conn = state.0.lock().unwrap();
    let today = ledger_core::ids::now_iso()[..10].to_string();
    let mut q = conn.prepare(
        "SELECT b.id, c.name, b.reference, b.bill_date, b.due_date, b.total_kobo, b.amount_paid_kobo, b.status
         FROM bills b JOIN contacts c ON c.id = b.contact_id
         WHERE b.company_id = ?1 ORDER BY b.created_at DESC LIMIT 200",
    ).map_err(db_err)?;
    let rows = q.query_map(rusqlite::params![company_id], |r| {
        let status: String = r.get(7)?;
        let due: String = r.get(4)?;
        Ok(BillRowDto {
            id: r.get(0)?, supplier: r.get(1)?, reference: r.get(2)?,
            bill_date: r.get(3)?, due_date: due.clone(),
            total_kobo: r.get(5)?, paid_kobo: r.get(6)?,
            overdue: (status == "open" || status == "partially_paid") && due < today,
            status,
        })
    }).map_err(db_err)?.collect::<Result<Vec<_>, _>>().map_err(db_err)?;
    Ok(rows)
}

#[derive(Serialize)]
struct OpenBillDto { id: String, reference: Option<String>, due_date: String, balance_kobo: i64, wht_applicable: bool }

#[tauri::command]
fn open_bills(state: tauri::State<Db>, company_id: String, contact_id: String) -> Result<Vec<OpenBillDto>, CmdError> {
    let conn = state.0.lock().unwrap();
    let mut q = conn.prepare(
        "SELECT id, reference, due_date, total_kobo - amount_paid_kobo, wht_applicable
         FROM bills
         WHERE company_id = ?1 AND contact_id = ?2 AND status IN ('open','partially_paid')
         ORDER BY due_date", // FIFO by due date (Spec 03 decision #3, mirrored)
    ).map_err(db_err)?;
    let rows = q.query_map(rusqlite::params![company_id, contact_id], |r| {
        Ok(OpenBillDto {
            id: r.get(0)?, reference: r.get(1)?, due_date: r.get(2)?,
            balance_kobo: r.get(3)?, wht_applicable: r.get::<_, i64>(4)? != 0,
        })
    }).map_err(db_err)?.collect::<Result<Vec<_>, _>>().map_err(db_err)?;
    Ok(rows)
}

#[derive(Deserialize)]
pub struct PaymentOutDto {
    pub company_id: String,
    pub contact_id: String,
    pub bank_account_id: String,
    pub payment_date: String,
    pub allocations: Vec<AllocationDto>,
    /// "auto" (exemption logic + no-TIN decision), "off", or "manual".
    pub wht_mode: String,
    pub wht_manual_kobo: Option<i64>,
    pub confirm_soft_close: bool,
}

#[tauri::command]
fn record_payment_out(state: tauri::State<Db>, sess: tauri::State<Sess>, input: PaymentOutDto) -> Result<PaymentDoneDto, CmdError> {
    let ctx = ctx(&sess, input.confirm_soft_close)?;
    let mut conn = state.0.lock().unwrap();
    let allocs: Vec<engine::Allocation> = input.allocations.into_iter()
        .map(|a| engine::Allocation { target_id: a.invoice_id, amount_kobo: a.amount_kobo })
        .collect();
    let mode = match input.wht_mode.as_str() {
        "off" => engine::WhtMode::Off,
        "manual" => engine::WhtMode::Manual(input.wht_manual_kobo.unwrap_or(0)),
        _ => engine::WhtMode::Auto,
    };
    let res = engine::post_payment_out(
        &mut conn, &input.company_id, &input.contact_id, &input.bank_account_id,
        &input.payment_date, &allocs, mode, &ctx,
    )?;
    Ok(PaymentDoneDto { receipt_number: None, deposit_kobo: 0, wht_kobo: res.wht_kobo })
}

#[derive(Deserialize)]
pub struct TransferDto {
    pub company_id: String,
    pub from_bank_id: String,
    pub to_bank_id: String,
    pub date: String,
    pub amount_kobo: i64,
    pub fee_kobo: i64,
    pub confirm_soft_close: bool,
}

#[tauri::command]
fn record_transfer(state: tauri::State<Db>, sess: tauri::State<Sess>, input: TransferDto) -> Result<String, CmdError> {
    let ctx = ctx(&sess, input.confirm_soft_close)?;
    let mut conn = state.0.lock().unwrap();
    engine::post_transfer(
        &mut conn, &input.company_id, &input.from_bank_id, &input.to_bank_id,
        &input.date, input.amount_kobo, input.fee_kobo, &ctx,
    ).map_err(Into::into)
}

// ===== Voids, quotes, documents (Spec 03 completion) =====

#[tauri::command]
fn void_invoice_cmd(state: tauri::State<Db>, sess: tauri::State<Sess>, invoice_id: String, confirm_soft_close: bool) -> Result<(), CmdError> {
    // Spec 02 role matrix: voiding documents is owner/advisor only — staff is
    // blocked HERE, at the command layer, not by hiding the Cancel link.
    let ctx = ctx_not_staff(&sess, confirm_soft_close)?;
    let mut conn = state.0.lock().unwrap();
    let today = ledger_core::ids::now_iso()[..10].to_string();
    engine::void_invoice(&mut conn, &invoice_id, &today, &ctx)?;
    Ok(())
}

#[derive(Serialize)]
struct PaymentRowDto {
    id: String, direction: String, contact: Option<String>, bank: String,
    payment_date: String, amount_kobo: i64, wht_kobo: i64,
    receipt_number: Option<String>, voided: bool,
}

#[tauri::command]
fn list_payments(state: tauri::State<Db>, company_id: String) -> Result<Vec<PaymentRowDto>, CmdError> {
    let conn = state.0.lock().unwrap();
    let mut q = conn.prepare(
        "SELECT p.id, p.direction, c.name, b.label, p.payment_date, p.amount_kobo, p.wht_kobo,
                p.receipt_number, p.voided
         FROM payments p
         LEFT JOIN contacts c ON c.id = p.contact_id
         JOIN bank_accounts b ON b.id = p.bank_account_id
         WHERE p.company_id = ?1 ORDER BY p.created_at DESC LIMIT 200",
    ).map_err(db_err)?;
    let rows = q.query_map(rusqlite::params![company_id], |r| {
        Ok(PaymentRowDto {
            id: r.get(0)?, direction: r.get(1)?, contact: r.get(2)?, bank: r.get(3)?,
            payment_date: r.get(4)?, amount_kobo: r.get(5)?, wht_kobo: r.get(6)?,
            receipt_number: r.get(7)?, voided: r.get::<_, i64>(8)? != 0,
        })
    }).map_err(db_err)?.collect::<Result<Vec<_>, _>>().map_err(db_err)?;
    Ok(rows)
}

#[tauri::command]
fn void_payment_cmd(state: tauri::State<Db>, sess: tauri::State<Sess>, payment_id: String, confirm_soft_close: bool) -> Result<(), CmdError> {
    // Same restriction as void_invoice_cmd — owner/advisor only (Spec 02 role matrix).
    let ctx = ctx_not_staff(&sess, confirm_soft_close)?;
    let mut conn = state.0.lock().unwrap();
    let today = ledger_core::ids::now_iso()[..10].to_string();
    engine::void_payment(&mut conn, &payment_id, &today, &ctx)?;
    Ok(())
}

#[tauri::command]
fn convert_quote_cmd(state: tauri::State<Db>, sess: tauri::State<Sess>, quote_id: String) -> Result<DraftDto, CmdError> {
    let ctx = ctx(&sess, false)?;
    let mut conn = state.0.lock().unwrap();
    let id = engine::convert_quote(&mut conn, &quote_id, &ctx)?;
    let number: String = conn.query_row(
        "SELECT number FROM invoices WHERE id = ?1", rusqlite::params![id], |r| r.get(0),
    ).map_err(db_err)?;
    Ok(DraftDto { id, number })
}

#[derive(Serialize)]
struct DocLineDto { description: String, quantity_milli: i64, unit_price_kobo: i64, discount_bp: i64, net_kobo: i64, vat_kobo: i64 }

#[derive(Serialize)]
struct InvoiceDocDto {
    number: String, kind: String, status: String,
    company_name: String, company_tin: Option<String>,
    customer: String, customer_phone: Option<String>,
    issue_date: String, due_date: String,
    subtotal_kobo: i64, vat_kobo: i64, total_kobo: i64, paid_kobo: i64,
    lines: Vec<DocLineDto>,
    bank_details: Vec<String>, // "pay into" block (Spec 03 §7)
}

/// Everything the printable document view needs — one call, print-ready.
#[tauri::command]
fn invoice_doc(state: tauri::State<Db>, invoice_id: String) -> Result<InvoiceDocDto, CmdError> {
    let conn = state.0.lock().unwrap();
    let head = conn.query_row(
        "SELECT i.number, i.kind, i.status, co.name, co.tin, c.name, c.phone,
                i.issue_date, i.due_date, i.subtotal_kobo, i.vat_kobo, i.total_kobo,
                i.amount_paid_kobo, i.company_id
         FROM invoices i
         JOIN companies co ON co.id = i.company_id
         JOIN contacts c ON c.id = i.contact_id
         WHERE i.id = ?1",
        rusqlite::params![invoice_id],
        |r| Ok((
            r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?,
            r.get::<_, String>(3)?, r.get::<_, Option<String>>(4)?,
            r.get::<_, String>(5)?, r.get::<_, Option<String>>(6)?,
            r.get::<_, String>(7)?, r.get::<_, String>(8)?,
            r.get::<_, i64>(9)?, r.get::<_, i64>(10)?, r.get::<_, i64>(11)?,
            r.get::<_, i64>(12)?, r.get::<_, String>(13)?,
        )),
    ).map_err(db_err)?;
    let mut q = conn.prepare(
        "SELECT description, quantity_milli, unit_price_kobo, discount_bp, net_kobo, vat_kobo
         FROM invoice_lines WHERE invoice_id = ?1 ORDER BY line_no",
    ).map_err(db_err)?;
    let lines = q.query_map(rusqlite::params![invoice_id], |r| {
        Ok(DocLineDto {
            description: r.get(0)?, quantity_milli: r.get(1)?, unit_price_kobo: r.get(2)?,
            discount_bp: r.get(3)?, net_kobo: r.get(4)?, vat_kobo: r.get(5)?,
        })
    }).map_err(db_err)?.collect::<Result<Vec<_>, _>>().map_err(db_err)?;
    let mut bq = conn.prepare(
        "SELECT label || CASE WHEN bank_name IS NOT NULL THEN ' — ' || bank_name ELSE '' END
                 || CASE WHEN account_number_last4 IS NOT NULL THEN ' (…' || account_number_last4 || ')' ELSE '' END
         FROM bank_accounts WHERE company_id = ?1 AND is_active = 1 AND kind = 'bank'",
    ).map_err(db_err)?;
    let bank_details = bq.query_map(rusqlite::params![head.13], |r| r.get::<_, String>(0))
        .map_err(db_err)?.collect::<Result<Vec<_>, _>>().map_err(db_err)?;
    Ok(InvoiceDocDto {
        number: head.0, kind: head.1, status: head.2, company_name: head.3, company_tin: head.4,
        customer: head.5, customer_phone: head.6, issue_date: head.7, due_date: head.8,
        subtotal_kobo: head.9, vat_kobo: head.10, total_kobo: head.11, paid_kobo: head.12,
        lines, bank_details,
    })
}

/// Delivery log (Spec 03 §7 / document_deliveries) — the register's "sent via WhatsApp".
#[tauri::command]
fn log_delivery(state: tauri::State<Db>, sess: tauri::State<Sess>, company_id: String, doc_type: String, doc_id: String, channel: String, recipient: Option<String>) -> Result<(), CmdError> {
    sess.0.require_session()?;
    let conn = state.0.lock().unwrap();
    conn.execute(
        "INSERT INTO document_deliveries (id, company_id, doc_type, doc_id, channel, recipient, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![ledger_core::ids::new_id(), company_id, doc_type, doc_id, channel,
                          recipient, ledger_core::ids::now_iso()],
    ).map_err(db_err)?;
    Ok(())
}

// ===== Reconciliation (Spec 04 §6–7) =====

use ledger_core::recon;

#[derive(Serialize)]
struct ReconDto { id: String, statement_date: String, statement_balance_kobo: i64 }

#[tauri::command]
fn recon_open(state: tauri::State<Db>, bank_account_id: String) -> Result<Option<ReconDto>, CmdError> {
    let conn = state.0.lock().unwrap();
    conn.query_row(
        "SELECT id, statement_date, statement_balance_kobo FROM reconciliations
         WHERE bank_account_id = ?1 AND status = 'in_progress'",
        rusqlite::params![bank_account_id],
        |r| Ok(ReconDto { id: r.get(0)?, statement_date: r.get(1)?, statement_balance_kobo: r.get(2)? }),
    ).optional().map_err(db_err)
}

#[tauri::command]
fn recon_start(state: tauri::State<Db>, sess: tauri::State<Sess>, company_id: String, bank_account_id: String,
               statement_date: String, statement_balance_kobo: i64) -> Result<String, CmdError> {
    sess.0.require_session()?;
    let mut conn = state.0.lock().unwrap();
    recon::start_reconciliation(&mut conn, &company_id, &bank_account_id, &statement_date, statement_balance_kobo)
        .map_err(Into::into)
}

#[derive(Deserialize)]
pub struct MappingDto {
    pub header_rows: usize,
    pub date_col: usize,
    pub desc_col: usize,
    pub amount_col: Option<usize>,
    pub debit_col: Option<usize>,
    pub credit_col: Option<usize>,
    pub date_format: String,
    pub flip_sign: bool,
}

#[derive(Serialize)]
struct ImportResultDto { imported: usize, skipped: usize, errors: Vec<String> }

#[tauri::command]
fn recon_import_csv(state: tauri::State<Db>, sess: tauri::State<Sess>, recon_id: String, csv_text: String, mapping: MappingDto)
    -> Result<ImportResultDto, CmdError>
{
    sess.0.require_session()?;
    let mut conn = state.0.lock().unwrap();
    let map = recon::CsvMapping {
        header_rows: mapping.header_rows, date_col: mapping.date_col, desc_col: mapping.desc_col,
        amount_col: mapping.amount_col, debit_col: mapping.debit_col, credit_col: mapping.credit_col,
        date_format: mapping.date_format, flip_sign: mapping.flip_sign,
    };
    let (rows, errors) = recon::parse_csv(&csv_text, &map);
    if rows.is_empty() {
        return Err(CmdError {
            code: "empty_import",
            message: "No usable lines found — check the column choices match your bank's file.".into(),
            detail: Some(errors.join("; ")),
        });
    }
    let (imported, skipped) = recon::import_rows(&mut conn, &recon_id, &rows)?;
    Ok(ImportResultDto { imported, skipped, errors })
}

#[derive(Serialize)]
struct ReconLineDto {
    id: String, stmt_date: String, description: Option<String>, amount_kobo: i64,
    state: String, match_kind: Option<String>, review_note: Option<String>, carried: bool,
}

#[derive(Serialize)]
struct ReconStateDto {
    lines: Vec<ReconLineDto>,
    statement_balance_kobo: i64, matched_kobo: i64, unresolved_kobo: i64,
    ledger_at_date_kobo: i64, outstanding_kobo: i64,
}

#[tauri::command]
fn recon_state(state: tauri::State<Db>, recon_id: String) -> Result<ReconStateDto, CmdError> {
    let conn = state.0.lock().unwrap();
    let mut q = conn.prepare(
        "SELECT id, stmt_date, stmt_description, stmt_amount_kobo, state, match_kind,
                review_note, carried_from_id IS NOT NULL
         FROM reconciliation_lines WHERE reconciliation_id = ?1
         ORDER BY state = 'needs_review' DESC, stmt_date, id",
    ).map_err(db_err)?;
    let lines = q.query_map(rusqlite::params![recon_id], |r| {
        Ok(ReconLineDto {
            id: r.get(0)?, stmt_date: r.get(1)?, description: r.get(2)?, amount_kobo: r.get(3)?,
            state: r.get(4)?, match_kind: r.get(5)?, review_note: r.get(6)?,
            carried: r.get::<_, i64>(7)? != 0,
        })
    }).map_err(db_err)?.collect::<Result<Vec<_>, _>>().map_err(db_err)?;
    let eq = recon::equation(&conn, &recon_id)?;
    Ok(ReconStateDto {
        lines,
        statement_balance_kobo: eq.statement_balance_kobo, matched_kobo: eq.matched_kobo,
        unresolved_kobo: eq.unresolved_kobo, ledger_at_date_kobo: eq.ledger_at_date_kobo,
        outstanding_kobo: eq.outstanding_kobo,
    })
}

#[derive(Serialize)]
struct CandidateDto { journal_line_id: String, entry_date: String, memo: String, amount_kobo: i64 }

/// Unmatched ledger lines on the account near the statement line's date.
#[tauri::command]
fn recon_candidates(state: tauri::State<Db>, recon_id: String, line_id: String) -> Result<Vec<CandidateDto>, CmdError> {
    let conn = state.0.lock().unwrap();
    let mut q = conn.prepare(
        "SELECT l.id, e.entry_date, e.memo, l.amount_kobo
         FROM journal_lines l
         JOIN journal_entries e ON e.id = l.entry_id
         JOIN reconciliations r ON r.id = ?1
         JOIN bank_accounts b ON b.id = r.bank_account_id
         JOIN reconciliation_lines rl ON rl.id = ?2
         WHERE l.account_id = b.account_id AND e.is_posted = 1
           AND NOT EXISTS (SELECT 1 FROM reconciliation_matches m WHERE m.journal_line_id = l.id)
           AND ABS(julianday(e.entry_date) - julianday(rl.stmt_date)) <= 14
         ORDER BY ABS(l.amount_kobo - rl.stmt_amount_kobo), e.entry_date
         LIMIT 20",
    ).map_err(db_err)?;
    let rows = q.query_map(rusqlite::params![recon_id, line_id], |r| {
        Ok(CandidateDto { journal_line_id: r.get(0)?, entry_date: r.get(1)?, memo: r.get(2)?, amount_kobo: r.get(3)? })
    }).map_err(db_err)?.collect::<Result<Vec<_>, _>>().map_err(db_err)?;
    Ok(rows)
}

#[tauri::command]
fn recon_match(state: tauri::State<Db>, sess: tauri::State<Sess>, line_id: String, journal_line_ids: Vec<String>) -> Result<(), CmdError> {
    sess.0.require_session()?;
    let mut conn = state.0.lock().unwrap();
    recon::manual_match(&mut conn, &line_id, &journal_line_ids, "manual").map_err(Into::into)
}

#[tauri::command]
fn recon_unmatch(state: tauri::State<Db>, sess: tauri::State<Sess>, line_id: String) -> Result<(), CmdError> {
    sess.0.require_session()?;
    let mut conn = state.0.lock().unwrap();
    recon::unmatch(&mut conn, &line_id).map_err(Into::into)
}

/// Spec 04 §7.5: flagging "needs review" is deliberately open to ANY role —
/// it's the accounts officer's mechanism for "I'm not sure, Oga will know."
/// Session is still required so the note is attributable.
#[tauri::command]
fn recon_flag(state: tauri::State<Db>, sess: tauri::State<Sess>, line_id: String, note: String) -> Result<(), CmdError> {
    let session = sess.0.require_session()?;
    let conn = state.0.lock().unwrap();
    recon::flag_needs_review(&conn, &line_id, &note, Some(&session.user_id)).map_err(Into::into)
}

/// Spec 04 §7.5: write-off is owner/advisor only, staff never — blocked here,
/// before the engine call. (The engine itself separately refuses anything
/// above the company's write-off limit outright, for any role — an
/// Advisor-Mode-elevated bypass of that limit is a known follow-up, not yet
/// wired; see PROGRESS.md.)
#[tauri::command]
fn recon_writeoff(state: tauri::State<Db>, sess: tauri::State<Sess>, line_id: String, note: String) -> Result<(), CmdError> {
    let ctx = ctx_not_staff(&sess, false)?;
    let mut conn = state.0.lock().unwrap();
    recon::write_off(&mut conn, &line_id, &note, &ctx)?;
    Ok(())
}

/// Spec 04 §7.5 path 4: marking a line as import garbage is advisor/owner only.
#[tauri::command]
fn recon_import_error(state: tauri::State<Db>, sess: tauri::State<Sess>, line_id: String) -> Result<(), CmdError> {
    let ctx = ctx_not_staff(&sess, false)?;
    let conn = state.0.lock().unwrap();
    recon::mark_import_error(&conn, &line_id, &ctx).map_err(Into::into)
}

#[tauri::command]
fn recon_complete(state: tauri::State<Db>, sess: tauri::State<Sess>, recon_id: String) -> Result<String, CmdError> {
    sess.0.require_session()?;
    let mut conn = state.0.lock().unwrap();
    recon::complete(&mut conn, &recon_id).map_err(Into::into)
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&dir)?;
            let db_path = dir.join("ledgerone.db");
            let conn = ledger_core::open(db_path.to_str().expect("utf8 path"))?;
            app.manage(Db(Mutex::new(conn)));
            app.manage(Sess(SessionStore::new()));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            create_company,
            create_company_full,
            list_companies,
            list_users,
            login,
            logout,
            current_session,
            advisor_enter,
            advisor_exit,
            add_bank_account,
            dashboard,
            record_drawing,
            list_contacts,
            create_contact,
            create_invoice_draft,
            send_invoice,
            list_invoices,
            open_invoices,
            record_payment_in,
            list_bank_accounts,
            expense_categories,
            wht_presets,
            record_expense,
            create_bill,
            list_bills,
            open_bills,
            record_payment_out,
            record_transfer,
            void_invoice_cmd,
            list_payments,
            void_payment_cmd,
            convert_quote_cmd,
            invoice_doc,
            log_delivery,
            recon_open,
            recon_start,
            recon_import_csv,
            recon_state,
            recon_candidates,
            recon_match,
            recon_unmatch,
            recon_flag,
            recon_writeoff,
            recon_import_error,
            recon_complete
        ])
        .run(tauri::generate_context!())
        .expect("error while running LedgerOne");
}
