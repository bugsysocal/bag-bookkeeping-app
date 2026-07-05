//! The posting engine (Spec 01 §6): the single service layer through which ALL
//! financial events flow. Each function is one transaction (P1); each generates
//! balanced lines from its spec template; the §4 triggers are the backstop.

use crate::ids::{new_id, now_iso};
use crate::money::{line_net, round_ratio, vat_of, wht_of};
use crate::posting::{post_entry_in, LineSpec, PostError};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use std::collections::BTreeMap;

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("validation: {0}")]
    Validation(String),
    /// P4: entry dated within the soft-closed period; retry with `confirm_soft_close`.
    #[error("soft-closed period — confirmation required")]
    SoftCloseConfirmationRequired,
    /// Spec 01 §6.2: WHT-applicable payment to a supplier with no TIN — the
    /// Regulations prescribe 2× the rate; never applied silently.
    #[error("supplier has no TIN — explicit WHT decision required (2× suggested: {suggested_kobo} kobo)")]
    WhtDecisionRequired { suggested_kobo: i64 },
    /// P7, in owner language upstream.
    #[error("insufficient stock: short {short_milli} milliunits")]
    InsufficientStock { short_milli: i64 },
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}

impl From<PostError> for EngineError {
    fn from(e: PostError) -> Self {
        match e {
            PostError::Validation(s) => EngineError::Validation(s),
            PostError::Db(e) => EngineError::Db(e),
        }
    }
}

type R<T> = Result<T, EngineError>;

/// Caller context: who is acting, and whether a soft-close warning was confirmed.
#[derive(Debug, Clone, Default)]
pub struct PostCtx {
    pub user_id: Option<String>,
    pub confirm_soft_close: bool,
}

// ===== lookups =====

struct Cfg {
    vat_registered: bool,
    vat_rate_bp: i64,
    inventory_enabled: bool,
    cit_exempt: bool,
    soft_close_through: Option<String>,
}

fn cfg(conn: &Connection, company_id: &str) -> R<Cfg> {
    conn.query_row(
        "SELECT vat_registered, vat_rate_bp, inventory_enabled, cit_exempt, soft_close_through
         FROM companies WHERE id = ?1",
        params![company_id],
        |r| {
            Ok(Cfg {
                vat_registered: r.get::<_, i64>(0)? != 0,
                vat_rate_bp: r.get(1)?,
                inventory_enabled: r.get::<_, i64>(2)? != 0,
                cit_exempt: r.get::<_, i64>(3)? != 0,
                soft_close_through: r.get(4)?,
            })
        },
    )
    .optional()?
    .ok_or_else(|| EngineError::Validation("unknown company".into()))
}

/// Resolve a system account by key (engine never resolves by code — Spec 01 §5).
fn sys(conn: &Connection, company_id: &str, key: &str) -> R<String> {
    conn.query_row(
        "SELECT id FROM accounts WHERE company_id = ?1 AND system_key = ?2 AND is_active = 1",
        params![company_id, key],
        |r| r.get(0),
    )
    .optional()?
    .ok_or_else(|| EngineError::Validation(format!("system account {key} missing")))
}

struct Bank {
    account_id: String,
    kind: String,
    last_reconciled: Option<String>,
}

fn bank(conn: &Connection, bank_account_id: &str) -> R<Bank> {
    conn.query_row(
        "SELECT account_id, kind, last_reconciled_date FROM bank_accounts WHERE id = ?1 AND is_active = 1",
        params![bank_account_id],
        |r| Ok(Bank { account_id: r.get(0)?, kind: r.get(1)?, last_reconciled: r.get(2)? }),
    )
    .optional()?
    .ok_or_else(|| EngineError::Validation("unknown or inactive bank account".into()))
}

fn bank_balance(conn: &Connection, account_id: &str) -> R<i64> {
    Ok(conn.query_row(
        "SELECT COALESCE(SUM(l.amount_kobo), 0) FROM journal_lines l
         JOIN journal_entries e ON e.id = l.entry_id
         WHERE l.account_id = ?1 AND e.is_posted = 1",
        params![account_id],
        |r| r.get(0),
    )?)
}

/// P4 soft close (T5 hard close stays with the trigger).
fn check_soft_close(c: &Cfg, entry_date: &str, ctx: &PostCtx) -> R<()> {
    if let Some(lock) = &c.soft_close_through {
        if entry_date <= lock.as_str() && !ctx.confirm_soft_close {
            return Err(EngineError::SoftCloseConfirmationRequired);
        }
    }
    Ok(())
}

/// P6 reconciliation lock.
fn check_recon_lock(b: &Bank, entry_date: &str) -> R<()> {
    if let Some(lock) = &b.last_reconciled {
        if entry_date <= lock.as_str() {
            return Err(EngineError::Validation(
                "this bank account is reconciled through that date (P6)".into(),
            ));
        }
    }
    Ok(())
}

/// Spec 04 B1: cash boxes hold what they hold; banks may overdraw (warn upstream).
fn check_cash_floor(conn: &Connection, b: &Bank, outflow_kobo: i64) -> R<()> {
    if (b.kind == "cash" || b.kind == "pos_wallet")
        && bank_balance(conn, &b.account_id)? - outflow_kobo < 0
    {
        return Err(EngineError::Validation(
            "cash account cannot go below zero (B1)".into(),
        ));
    }
    Ok(())
}

fn next_doc_number(conn: &Connection, company_id: &str, doc_type: &str) -> R<String> {
    let (prefix, n): (String, i64) = conn.query_row(
        "SELECT prefix, next_number FROM document_sequences WHERE company_id = ?1 AND doc_type = ?2",
        params![company_id, doc_type],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    conn.execute(
        "UPDATE document_sequences SET next_number = next_number + 1
         WHERE company_id = ?1 AND doc_type = ?2",
        params![company_id, doc_type],
    )?;
    Ok(format!("{prefix}{n:06}"))
}

fn stock_state(conn: &Connection, product_id: &str) -> R<(i64, i64)> {
    Ok(conn.query_row(
        "SELECT COALESCE(SUM(quantity_milli),0), COALESCE(SUM(total_cost_kobo),0)
         FROM inventory_movements WHERE product_id = ?1",
        params![product_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?)
}

// ===== postJournal (Spec 01 §6.7) =====

/// Advisor-mode manual journal. P8: AR/AP lines must carry a contact.
pub fn post_journal(
    conn: &mut Connection,
    company_id: &str,
    entry_date: &str,
    memo: &str,
    source_type: &str, // 'manual' | 'opening_balance' | ...
    ctx: &PostCtx,
    lines: &[LineSpec],
) -> R<String> {
    let c = cfg(conn, company_id)?;
    check_soft_close(&c, entry_date, ctx)?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    for l in lines {
        let key: Option<String> = tx
            .query_row(
                "SELECT system_key FROM accounts WHERE id = ?1 AND company_id = ?2",
                params![l.account_id, company_id],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(|| EngineError::Validation("line references unknown account".into()))?;
        if matches!(key.as_deref(), Some("AR") | Some("AP")) && l.contact_id.is_none() {
            return Err(EngineError::Validation(
                "AR/AP lines must carry a contact (P8)".into(),
            ));
        }
    }
    let id = post_entry_in(
        &tx, company_id, entry_date, memo, source_type, None,
        ctx.user_id.as_deref(), lines,
    )?;
    tx.commit()?;
    Ok(id)
}

// ===== Invoices (Spec 01 §6.1, Spec 03) =====

#[derive(Debug, Clone)]
pub struct InvoiceLineInput {
    pub product_id: Option<String>,
    pub description: String,
    pub quantity_milli: i64,
    pub unit_price_kobo: i64,
    pub discount_bp: i64,
    pub vat_applied: bool,
    /// Defaults to the product's income account, else 4000 SALES_DEFAULT.
    pub income_account_id: Option<String>,
}

/// Create a draft invoice/quote. Consumes a sequence number (Spec 01 decision #5).
/// Drafts post nothing.
pub fn create_invoice(
    conn: &mut Connection,
    company_id: &str,
    contact_id: &str,
    kind: &str, // 'invoice' | 'quote'
    issue_date: &str,
    due_date: &str,
    lines: &[InvoiceLineInput],
    ctx: &PostCtx,
) -> R<String> {
    if lines.is_empty() {
        return Err(EngineError::Validation("an invoice needs at least one line (V1)".into()));
    }
    if due_date < issue_date {
        return Err(EngineError::Validation("due date before issue date (V2)".into()));
    }
    for l in lines {
        if l.quantity_milli <= 0 || l.unit_price_kobo < 0 || !(0..=10_000).contains(&l.discount_bp) {
            return Err(EngineError::Validation("invalid line quantity/price/discount (V1)".into()));
        }
    }
    let c = cfg(conn, company_id)?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let number = next_doc_number(&tx, company_id, if kind == "quote" { "quote" } else { "invoice" })?;
    let sales_default = sys(&tx, company_id, "SALES_DEFAULT")?;
    let invoice_id = new_id();
    let now = now_iso();

    let mut subtotal = 0i64;
    let mut vat_total = 0i64;
    let mut rows = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        let net = line_net(l.quantity_milli, l.unit_price_kobo, l.discount_bp);
        let vat = if l.vat_applied && c.vat_registered { vat_of(net, c.vat_rate_bp) } else { 0 };
        subtotal += net;
        vat_total += vat;
        let income = match &l.income_account_id {
            Some(id) => id.clone(),
            None => match &l.product_id {
                Some(p) => tx
                    .query_row(
                        "SELECT income_account_id FROM products WHERE id = ?1",
                        params![p],
                        |r| r.get::<_, Option<String>>(0),
                    )?
                    .unwrap_or_else(|| sales_default.clone()),
                None => sales_default.clone(),
            },
        };
        rows.push((i as i64 + 1, l, net, vat, income));
    }

    tx.execute(
        "INSERT INTO invoices (id, company_id, contact_id, number, kind, status, issue_date, due_date,
                               subtotal_kobo, vat_kobo, total_kobo, created_by, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, 'draft', ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            invoice_id, company_id, contact_id, number, kind, issue_date, due_date,
            subtotal, vat_total, subtotal + vat_total, ctx.user_id, now
        ],
    )?;
    for (line_no, l, net, vat, income) in &rows {
        tx.execute(
            "INSERT INTO invoice_lines (id, invoice_id, line_no, product_id, description, quantity_milli,
                                        unit_price_kobo, discount_bp, vat_applied, net_kobo, vat_kobo,
                                        income_account_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                new_id(), invoice_id, line_no, l.product_id, l.description, l.quantity_milli,
                l.unit_price_kobo, l.discount_bp, l.vat_applied as i64, net, vat, income
            ],
        )?;
    }
    tx.commit()?;
    Ok(invoice_id)
}

/// Draft → Sent: freeze amounts, post the Spec 01 §6.1 template (+ COGS when
/// inventory is on), handle the zero-total free-sample case (Spec 03 V7).
pub fn post_invoice(conn: &mut Connection, invoice_id: &str, ctx: &PostCtx) -> R<Option<String>> {
    let (company_id, contact_id, number, kind, status, issue_date, total, subtotal, vat_total): (
        String, String, String, String, String, String, i64, i64, i64,
    ) = conn
        .query_row(
            "SELECT company_id, contact_id, number, kind, status, issue_date,
                    total_kobo, subtotal_kobo, vat_kobo
             FROM invoices WHERE id = ?1",
            params![invoice_id],
            |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?,
                    r.get(6)?, r.get(7)?, r.get(8)?))
            },
        )
        .optional()?
        .ok_or_else(|| EngineError::Validation("unknown invoice".into()))?;

    if kind != "invoice" {
        return Err(EngineError::Validation("quotes never post (Spec 03 §3)".into()));
    }
    if status != "draft" {
        return Err(EngineError::Validation(format!(
            "only drafts can be sent (status: {status}); posted invoices are void-and-reissue"
        )));
    }
    let c = cfg(conn, &company_id)?;
    check_soft_close(&c, &issue_date, ctx)?;

    let contact_name: String = conn.query_row(
        "SELECT name FROM contacts WHERE id = ?1", params![contact_id], |r| r.get(0),
    )?;

    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

    // Income lines grouped per account (Spec 01 §6.1).
    let mut income: BTreeMap<String, i64> = BTreeMap::new();
    {
        let mut q = tx.prepare(
            "SELECT income_account_id, net_kobo FROM invoice_lines WHERE invoice_id = ?1",
        )?;
        let iter = q.query_map(params![invoice_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?;
        for row in iter {
            let (acct, net) = row?;
            *income.entry(acct).or_insert(0) += net;
        }
    }

    let mut je_lines = Vec::new();
    if total > 0 {
        je_lines.push(LineSpec::with_contact(&sys(&tx, &company_id, "AR")?, total, &contact_id));
        for (acct, net) in &income {
            if *net != 0 {
                je_lines.push(LineSpec::new(acct, -net));
            }
        }
        if vat_total != 0 {
            je_lines.push(LineSpec::new(&sys(&tx, &company_id, "VAT_OUTPUT")?, -vat_total));
        }
        debug_assert_eq!(subtotal + vat_total, total);
    }

    // COGS at WAC for inventory-tracked lines — same entry (Spec 01 §6.1).
    let mut movements: Vec<(String, i64, i64, i64)> = Vec::new();
    if c.inventory_enabled {
        let mut cogs: BTreeMap<String, i64> = BTreeMap::new();
        let mut total_cost = 0i64;
        {
            let mut q = tx.prepare(
                "SELECT il.product_id, il.quantity_milli, il.description,
                        COALESCE(p.cogs_account_id, ''), p.track_inventory
                 FROM invoice_lines il JOIN products p ON p.id = il.product_id
                 WHERE il.invoice_id = ?1 AND il.product_id IS NOT NULL",
            )?;
            let iter = q.query_map(params![invoice_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?, r.get::<_, i64>(4)?))
            })?;
            for row in iter {
                let (product_id, qty, _desc, cogs_acct, tracked) = row?;
                if tracked == 0 {
                    continue;
                }
                let (on_hand, value) = stock_state(&tx, &product_id)?;
                if qty > on_hand {
                    return Err(EngineError::InsufficientStock { short_milli: qty - on_hand });
                }
                let cost = round_ratio(qty as i128 * value as i128, on_hand.max(1) as i128);
                let acct = if cogs_acct.is_empty() {
                    sys(&tx, &company_id, "COGS_DEFAULT")?
                } else {
                    cogs_acct
                };
                *cogs.entry(acct).or_insert(0) += cost;
                total_cost += cost;
                movements.push((product_id, -qty, round_ratio(cost as i128 * 1000, qty as i128), -cost));
            }
        }
        if total_cost > 0 {
            for (acct, cost) in &cogs {
                je_lines.push(LineSpec::new(acct, *cost));
            }
            je_lines.push(LineSpec::new(&sys(&tx, &company_id, "INVENTORY")?, -total_cost));
        }
    }

    // Zero-total with no stock lines: no JE at all — the document is the paper trail (Spec 03 V7).
    let entry_id = if je_lines.is_empty() {
        None
    } else {
        Some(post_entry_in(
            &tx, &company_id, &issue_date,
            &format!("Invoice {number} — {contact_name}"),
            "invoice", Some(invoice_id), ctx.user_id.as_deref(), &je_lines,
        )?)
    };

    let now = now_iso();
    for (product_id, qty, unit_cost, tot) in &movements {
        tx.execute(
            "INSERT INTO inventory_movements (id, company_id, product_id, movement_date, kind,
                                              quantity_milli, unit_cost_kobo, total_cost_kobo,
                                              journal_entry_id, created_at)
             VALUES (?1, ?2, ?3, ?4, 'sale', ?5, ?6, ?7, ?8, ?9)",
            params![new_id(), company_id, product_id, issue_date, qty, unit_cost, tot, entry_id, now],
        )?;
    }

    let new_status = if total == 0 { "paid" } else { "sent" };
    tx.execute(
        "UPDATE invoices SET status = ?2, sent_at = ?3, journal_entry_id = ?4 WHERE id = ?1",
        params![invoice_id, new_status, now, entry_id],
    )?;
    tx.commit()?;
    Ok(entry_id)
}

// ===== Bills (Spec 01 §6.4, Spec 04 §2) =====

#[derive(Debug, Clone)]
pub struct BillLineInput {
    pub product_id: Option<String>,
    pub description: String,
    pub quantity_milli: i64,
    pub unit_cost_kobo: i64,
    /// Vendor charged VAT on this line.
    pub vat_charged: bool,
    /// NTA 2025 §155(4): defaults claimable when VAT-registered; advisor override per line.
    pub vat_claimable: bool,
    pub expense_account_id: String,
}

pub fn create_bill(
    conn: &mut Connection,
    company_id: &str,
    contact_id: &str,
    bill_date: &str,
    due_date: &str,
    wht_applicable: bool,
    wht_rate_bp: Option<i64>,
    lines: &[BillLineInput],
    ctx: &PostCtx,
) -> R<String> {
    if lines.is_empty() {
        return Err(EngineError::Validation("a bill needs at least one line".into()));
    }
    let c = cfg(conn, company_id)?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let bill_id = new_id();
    let now = now_iso();

    let mut subtotal = 0i64;
    let mut vat_claimed = 0i64;
    let mut total = 0i64;
    let mut rows = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        let net = line_net(l.quantity_milli, l.unit_cost_kobo, 0);
        let vat = if l.vat_charged { vat_of(net, c.vat_rate_bp) } else { 0 };
        let claimable = l.vat_claimable && c.vat_registered && vat != 0;
        subtotal += net;
        total += net + vat;
        if claimable {
            vat_claimed += vat;
        }
        rows.push((i as i64 + 1, l, net, vat, claimable));
    }

    tx.execute(
        "INSERT INTO bills (id, company_id, contact_id, status, bill_date, due_date,
                            subtotal_kobo, vat_kobo, total_kobo, wht_applicable, wht_rate_bp,
                            created_by, created_at)
         VALUES (?1, ?2, ?3, 'draft', ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            bill_id, company_id, contact_id, bill_date, due_date,
            subtotal, vat_claimed, total, wht_applicable as i64, wht_rate_bp, ctx.user_id, now
        ],
    )?;
    for (line_no, l, net, vat, claimable) in &rows {
        tx.execute(
            "INSERT INTO bill_lines (id, bill_id, line_no, product_id, description, quantity_milli,
                                     unit_cost_kobo, vat_claimable, net_kobo, vat_kobo, expense_account_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                new_id(), bill_id, line_no, l.product_id, l.description, l.quantity_milli,
                l.unit_cost_kobo, *claimable as i64, net, vat, l.expense_account_id
            ],
        )?;
    }
    tx.commit()?;
    Ok(bill_id)
}

/// Draft → Open: Dr expense per line (net + non-claimable VAT) / Dr VAT_INPUT / Cr AP.
/// WHT posts NOTHING here — the split happens at payment (Spec 01 §6.4).
pub fn post_bill(conn: &mut Connection, bill_id: &str, ctx: &PostCtx) -> R<String> {
    let (company_id, contact_id, status, bill_date, total): (String, String, String, String, i64) =
        conn.query_row(
            "SELECT company_id, contact_id, status, bill_date, total_kobo FROM bills WHERE id = ?1",
            params![bill_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?
        .ok_or_else(|| EngineError::Validation("unknown bill".into()))?;
    if status != "draft" {
        return Err(EngineError::Validation("only draft bills post".into()));
    }
    let c = cfg(conn, &company_id)?;
    check_soft_close(&c, &bill_date, ctx)?;
    let contact_name: String = conn.query_row(
        "SELECT name FROM contacts WHERE id = ?1", params![contact_id], |r| r.get(0),
    )?;

    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let inventory_acct = sys(&tx, &company_id, "INVENTORY")?;

    let mut debits: BTreeMap<String, i64> = BTreeMap::new();
    let mut vat_input_total = 0i64;
    let mut movements = Vec::new();
    {
        let mut q = tx.prepare(
            "SELECT expense_account_id, net_kobo, vat_kobo, vat_claimable, product_id, quantity_milli
             FROM bill_lines WHERE bill_id = ?1",
        )?;
        let iter = q.query_map(params![bill_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?, r.get::<_, Option<String>>(4)?, r.get::<_, i64>(5)?))
        })?;
        for row in iter {
            let (acct, net, vat, claimable, product_id, qty) = row?;
            let dr = net + if claimable == 0 { vat } else { 0 };
            *debits.entry(acct.clone()).or_insert(0) += dr;
            if claimable != 0 {
                vat_input_total += vat;
            }
            if c.inventory_enabled && acct == inventory_acct {
                if let Some(pid) = product_id {
                    if qty > 0 {
                        movements.push((pid, qty, round_ratio(dr as i128 * 1000, qty as i128), dr));
                    }
                }
            }
        }
    }

    let mut je_lines = Vec::new();
    for (acct, amt) in &debits {
        if *amt != 0 {
            je_lines.push(LineSpec::new(acct, *amt));
        }
    }
    if vat_input_total != 0 {
        je_lines.push(LineSpec::new(&sys(&tx, &company_id, "VAT_INPUT")?, vat_input_total));
    }
    je_lines.push(LineSpec::with_contact(&sys(&tx, &company_id, "AP")?, -total, &contact_id));

    let entry_id = post_entry_in(
        &tx, &company_id, &bill_date,
        &format!("Bill — {contact_name}"),
        "bill", Some(bill_id), ctx.user_id.as_deref(), &je_lines,
    )?;

    let now = now_iso();
    for (pid, qty, unit_cost, tot) in &movements {
        tx.execute(
            "INSERT INTO inventory_movements (id, company_id, product_id, movement_date, kind,
                                              quantity_milli, unit_cost_kobo, total_cost_kobo,
                                              journal_entry_id, created_at)
             VALUES (?1, ?2, ?3, ?4, 'purchase', ?5, ?6, ?7, ?8, ?9)",
            params![new_id(), company_id, pid, bill_date, qty, unit_cost, tot, entry_id, now],
        )?;
    }
    tx.execute(
        "UPDATE bills SET status = 'open', journal_entry_id = ?2 WHERE id = ?1",
        params![bill_id, entry_id],
    )?;
    tx.commit()?;
    Ok(entry_id)
}

// ===== Payments (Spec 01 §6.2, Spec 03 §5, Spec 04 §3) =====

#[derive(Debug, Clone)]
pub struct Allocation {
    pub target_id: String, // invoice or bill id, per direction
    pub amount_kobo: i64,  // gross amount of the document being settled
}

/// WHT handling for supplier payments.
#[derive(Debug, Clone)]
pub enum WhtMode {
    /// Compute per flagged bill; apply the small-company exemption (cit_exempt +
    /// supplier TIN + ≤ ₦2M calendar-month aggregate). No-TIN → WhtDecisionRequired.
    Auto,
    /// Advisor override: no deduction.
    Off,
    /// Advisor override / no-TIN resolution: exact amount.
    Manual(i64),
}

#[derive(Debug)]
pub struct PaymentResult {
    pub payment_id: String,
    pub entry_id: String,
    pub receipt_number: Option<String>,
    pub wht_kobo: i64,
    pub deposit_kobo: i64,
}

fn recompute_invoice_status(conn: &Connection, invoice_id: &str) -> R<()> {
    let allocated: i64 = conn.query_row(
        "SELECT COALESCE(SUM(pa.amount_kobo), 0) FROM payment_allocations pa
         JOIN payments p ON p.id = pa.payment_id
         WHERE pa.target_type = 'invoice' AND pa.target_id = ?1 AND p.voided = 0",
        params![invoice_id],
        |r| r.get(0),
    )?;
    // Deposit applications settle AR without a payment row (Spec 03 §5.4): read them
    // from the ledger itself — Cr AR lines of unreversed deposit_application entries.
    let deposit_applied: i64 = conn.query_row(
        "SELECT COALESCE(-SUM(l.amount_kobo), 0)
         FROM journal_lines l
         JOIN journal_entries e ON e.id = l.entry_id
         JOIN accounts a ON a.id = l.account_id
         WHERE e.source_type = 'deposit_application' AND e.source_id = ?1
           AND e.is_posted = 1 AND e.reversed_by_entry_id IS NULL
           AND a.system_key = 'AR'",
        params![invoice_id],
        |r| r.get(0),
    )?;
    let paid = allocated + deposit_applied;
    conn.execute(
        "UPDATE invoices SET amount_paid_kobo = ?2,
           status = CASE
             WHEN status IN ('void','draft','converted') THEN status
             WHEN total_kobo - ?2 <= 0 THEN 'paid'
             WHEN ?2 > 0 THEN 'partially_paid'
             ELSE 'sent' END
         WHERE id = ?1",
        params![invoice_id, paid],
    )?;
    Ok(())
}

fn recompute_bill_status(conn: &Connection, bill_id: &str) -> R<()> {
    let paid: i64 = conn.query_row(
        "SELECT COALESCE(SUM(pa.amount_kobo), 0) FROM payment_allocations pa
         JOIN payments p ON p.id = pa.payment_id
         WHERE pa.target_type = 'bill' AND pa.target_id = ?1 AND p.voided = 0",
        params![bill_id],
        |r| r.get(0),
    )?;
    conn.execute(
        "UPDATE bills SET amount_paid_kobo = ?2,
           status = CASE
             WHEN status IN ('void','draft') THEN status
             WHEN total_kobo - ?2 <= 0 THEN 'paid'
             WHEN ?2 > 0 THEN 'partially_paid'
             ELSE 'open' END
         WHERE id = ?1",
        params![bill_id, paid],
    )?;
    Ok(())
}

/// FX receipt metadata (Spec 03 §5.3): NGN invoice settled into a domiciliary
/// account. `amount_kobo` is the NGN equivalent; the difference vs the settled
/// AR posts to FX_GAIN_LOSS as realized FX.
#[derive(Debug, Clone)]
pub struct FxReceipt {
    pub currency: String,
    pub fx_amount_kobo: i64, // minor units of the foreign currency
}

/// Customer receipt: Dr Bank + Dr WHT Receivable / Cr AR (allocations) / Cr
/// Unearned Revenue (remainder = customer deposit). Generates an RCT number.
#[allow(clippy::too_many_arguments)]
pub fn post_payment_in(
    conn: &mut Connection,
    company_id: &str,
    contact_id: &str,
    bank_account_id: &str,
    payment_date: &str,
    amount_kobo: i64,
    wht_withheld_kobo: i64,
    allocations: &[Allocation],
    fx: Option<FxReceipt>,
    ctx: &PostCtx,
) -> R<PaymentResult> {
    if amount_kobo <= 0 || wht_withheld_kobo < 0 {
        return Err(EngineError::Validation("amounts must be positive".into()));
    }
    let c = cfg(conn, company_id)?;
    check_soft_close(&c, payment_date, ctx)?;
    let b = bank(conn, bank_account_id)?;
    check_recon_lock(&b, payment_date)?;

    let gross = amount_kobo + wht_withheld_kobo;
    let mut alloc_total = 0i64;
    for a in allocations {
        let (total, paid, status): (i64, i64, String) = conn
            .query_row(
                "SELECT total_kobo, amount_paid_kobo, status FROM invoices
                 WHERE id = ?1 AND company_id = ?2 AND kind = 'invoice'",
                params![a.target_id, company_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?
            .ok_or_else(|| EngineError::Validation("allocation to unknown invoice".into()))?;
        if status == "void" || status == "draft" {
            return Err(EngineError::Validation("cannot allocate to void/draft invoices".into()));
        }
        if a.amount_kobo <= 0 || a.amount_kobo > total - paid {
            return Err(EngineError::Validation(
                "allocation exceeds invoice balance".into(),
            ));
        }
        alloc_total += a.amount_kobo;
    }
    // FX receipts must allocate fully — a remainder is FX difference, never a
    // deposit (deposits in foreign currency are out of v1 scope, Spec 03 §5.3).
    let (deposit, fx_diff) = if fx.is_some() {
        if alloc_total == 0 {
            return Err(EngineError::Validation(
                "FX receipts must be allocated to invoices".into(),
            ));
        }
        (0i64, amount_kobo - (alloc_total - wht_withheld_kobo))
    } else {
        if alloc_total > gross {
            return Err(EngineError::Validation(
                "allocations exceed cash received + WHT".into(),
            ));
        }
        (gross - alloc_total, 0i64)
    };

    let contact_name: String = conn.query_row(
        "SELECT name FROM contacts WHERE id = ?1", params![contact_id], |r| r.get(0),
    )?;

    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut bank_line = LineSpec::new(&b.account_id, amount_kobo);
    if let Some(f) = &fx {
        bank_line.fx_currency = Some(f.currency.clone());
        bank_line.fx_amount_kobo = Some(f.fx_amount_kobo);
    }
    let mut je = vec![bank_line];
    if wht_withheld_kobo > 0 {
        je.push(LineSpec::new(&sys(&tx, company_id, "WHT_RECEIVABLE")?, wht_withheld_kobo));
    }
    if alloc_total > 0 {
        je.push(LineSpec::with_contact(&sys(&tx, company_id, "AR")?, -alloc_total, contact_id));
    }
    if deposit > 0 {
        je.push(LineSpec::with_contact(
            &sys(&tx, company_id, "UNEARNED_REVENUE")?, -deposit, contact_id,
        ));
    }
    if fx_diff != 0 {
        // Realized FX: gain when the naira received exceeds the AR settled (Cr),
        // loss when it falls short (Dr). Line = -(diff) keeps the entry balanced.
        je.push(LineSpec::new(&sys(&tx, company_id, "FX_GAIN_LOSS")?, -fx_diff));
    }

    let receipt = next_doc_number(&tx, company_id, "receipt")?;
    let entry_id = post_entry_in(
        &tx, company_id, payment_date,
        &format!("Payment received — {contact_name} ({receipt})"),
        "payment", None, ctx.user_id.as_deref(), &je,
    )?;

    let payment_id = new_id();
    tx.execute(
        "INSERT INTO payments (id, company_id, direction, contact_id, bank_account_id, payment_date,
                               amount_kobo, wht_kobo, receipt_number, journal_entry_id, created_by, created_at)
         VALUES (?1, ?2, 'in', ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            payment_id, company_id, contact_id, bank_account_id, payment_date,
            amount_kobo, wht_withheld_kobo, receipt, entry_id, ctx.user_id, now_iso()
        ],
    )?;
    for a in allocations {
        tx.execute(
            "INSERT INTO payment_allocations (id, payment_id, target_type, target_id, amount_kobo)
             VALUES (?1, ?2, 'invoice', ?3, ?4)",
            params![new_id(), payment_id, a.target_id, a.amount_kobo],
        )?;
        recompute_invoice_status(&tx, &a.target_id)?;
    }
    tx.commit()?;
    Ok(PaymentResult {
        payment_id, entry_id, receipt_number: Some(receipt),
        wht_kobo: wht_withheld_kobo, deposit_kobo: deposit,
    })
}

/// Supplier payment: Dr AP (gross) / Cr Bank (net) / Cr WHT Payable — the WHT
/// split at payment time, with the WHT Regs 2024 small-company exemption.
#[allow(clippy::too_many_arguments)]
pub fn post_payment_out(
    conn: &mut Connection,
    company_id: &str,
    contact_id: &str,
    bank_account_id: &str,
    payment_date: &str,
    allocations: &[Allocation],
    wht_mode: WhtMode,
    ctx: &PostCtx,
) -> R<PaymentResult> {
    if allocations.is_empty() {
        return Err(EngineError::Validation("supplier payment needs allocations".into()));
    }
    let c = cfg(conn, company_id)?;
    check_soft_close(&c, payment_date, ctx)?;
    let b = bank(conn, bank_account_id)?;
    check_recon_lock(&b, payment_date)?;

    let (tin, contact_name): (Option<String>, String) = conn.query_row(
        "SELECT tin, name FROM contacts WHERE id = ?1", params![contact_id], |r| Ok((r.get(0)?, r.get(1)?)),
    )?;

    let mut gross = 0i64;
    let mut computed_wht = 0i64;
    for a in allocations {
        let (total, paid, status, subtotal, wht_applicable, rate): (i64, i64, String, i64, i64, Option<i64>) =
            conn.query_row(
                "SELECT total_kobo, amount_paid_kobo, status, subtotal_kobo, wht_applicable, wht_rate_bp
                 FROM bills WHERE id = ?1 AND company_id = ?2",
                params![a.target_id, company_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
            )
            .optional()?
            .ok_or_else(|| EngineError::Validation("allocation to unknown bill".into()))?;
        if status != "open" && status != "partially_paid" {
            return Err(EngineError::Validation("bill is not payable in its current status".into()));
        }
        if a.amount_kobo <= 0 || a.amount_kobo > total - paid {
            return Err(EngineError::Validation("allocation exceeds bill balance".into()));
        }
        gross += a.amount_kobo;
        if wht_applicable != 0 {
            // WHT on the ex-VAT portion of the allocated amount (Spec 01 §6.2).
            let ex_vat = round_ratio(a.amount_kobo as i128 * subtotal as i128, total.max(1) as i128);
            computed_wht += wht_of(ex_vat, rate.unwrap_or(0));
        }
    }

    let wht = match wht_mode {
        WhtMode::Off => 0,
        WhtMode::Manual(x) => {
            if x < 0 || x >= gross {
                return Err(EngineError::Validation("manual WHT out of range".into()));
            }
            x
        }
        WhtMode::Auto => {
            if computed_wht == 0 {
                0
            } else if tin.is_none() {
                // Never silent: Regulations prescribe 2× when the supplier has no TIN.
                return Err(EngineError::WhtDecisionRequired { suggested_kobo: computed_wht * 2 });
            } else {
                // Small-company exemption: cit_exempt + TIN + ≤ ₦2M calendar-month
                // aggregate to this supplier, THIS payment included (Spec 04 §3).
                let month = &payment_date[0..7];
                let prior: i64 = conn.query_row(
                    "SELECT COALESCE(SUM(amount_kobo + wht_kobo), 0) FROM payments
                     WHERE company_id = ?1 AND contact_id = ?2 AND direction = 'out'
                       AND voided = 0 AND substr(payment_date, 1, 7) = ?3",
                    params![company_id, contact_id, month],
                    |r| r.get(0),
                )?;
                let exempt = c.cit_exempt && prior + gross <= 2_000_000_00;
                if exempt { 0 } else { computed_wht }
            }
        }
    };

    let cash = gross - wht;
    check_cash_floor(conn, &b, cash)?;

    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut je = vec![
        LineSpec::with_contact(&sys(&tx, company_id, "AP")?, gross, contact_id),
        LineSpec::new(&b.account_id, -cash),
    ];
    if wht > 0 {
        je.push(LineSpec::new(&sys(&tx, company_id, "WHT_PAYABLE")?, -wht));
    }
    let entry_id = post_entry_in(
        &tx, company_id, payment_date,
        &format!("Payment to {contact_name}"),
        "payment", None, ctx.user_id.as_deref(), &je,
    )?;

    let payment_id = new_id();
    tx.execute(
        "INSERT INTO payments (id, company_id, direction, contact_id, bank_account_id, payment_date,
                               amount_kobo, wht_kobo, journal_entry_id, created_by, created_at)
         VALUES (?1, ?2, 'out', ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            payment_id, company_id, contact_id, bank_account_id, payment_date,
            cash, wht, entry_id, ctx.user_id, now_iso()
        ],
    )?;
    for a in allocations {
        tx.execute(
            "INSERT INTO payment_allocations (id, payment_id, target_type, target_id, amount_kobo)
             VALUES (?1, ?2, 'bill', ?3, ?4)",
            params![new_id(), payment_id, a.target_id, a.amount_kobo],
        )?;
        recompute_bill_status(&tx, &a.target_id)?;
    }
    tx.commit()?;
    Ok(PaymentResult {
        payment_id, entry_id, receipt_number: None, wht_kobo: wht, deposit_kobo: 0,
    })
}

// ===== Transfers & drawings (Spec 01 §6.5–6.6) =====

/// Never income, never expense. Optional fee rides the same entry → BANK_CHARGES.
pub fn post_transfer(
    conn: &mut Connection,
    company_id: &str,
    from_bank_id: &str,
    to_bank_id: &str,
    transfer_date: &str,
    amount_kobo: i64,
    fee_kobo: i64,
    ctx: &PostCtx,
) -> R<String> {
    if from_bank_id == to_bank_id {
        return Err(EngineError::Validation("source and destination must differ".into()));
    }
    if amount_kobo <= 0 || fee_kobo < 0 {
        return Err(EngineError::Validation("invalid transfer amounts".into()));
    }
    let c = cfg(conn, company_id)?;
    check_soft_close(&c, transfer_date, ctx)?;
    let from = bank(conn, from_bank_id)?;
    let to = bank(conn, to_bank_id)?;
    check_recon_lock(&from, transfer_date)?;
    check_recon_lock(&to, transfer_date)?;
    check_cash_floor(conn, &from, amount_kobo + fee_kobo)?;

    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut je = vec![
        LineSpec::new(&to.account_id, amount_kobo),
        LineSpec::new(&from.account_id, -(amount_kobo + fee_kobo)),
    ];
    if fee_kobo > 0 {
        je.push(LineSpec::new(&sys(&tx, company_id, "BANK_CHARGES")?, fee_kobo));
    }
    let entry_id = post_entry_in(
        &tx, company_id, transfer_date, "Money moved between accounts",
        "transfer", None, ctx.user_id.as_deref(), &je,
    )?;
    tx.commit()?;
    Ok(entry_id)
}

/// The guilt-free button. out: Dr Drawings / Cr bank. in: Dr bank / Cr Capital.
/// No contact, no VAT, no WHT — structurally impossible.
pub fn post_drawing(
    conn: &mut Connection,
    company_id: &str,
    bank_account_id: &str,
    drawing_date: &str,
    amount_kobo: i64,
    direction_out: bool,
    ctx: &PostCtx,
) -> R<String> {
    if amount_kobo <= 0 {
        return Err(EngineError::Validation("amount must be positive".into()));
    }
    let c = cfg(conn, company_id)?;
    check_soft_close(&c, drawing_date, ctx)?;
    let b = bank(conn, bank_account_id)?;
    check_recon_lock(&b, drawing_date)?;

    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let (je, source_type, memo) = if direction_out {
        check_cash_floor(&tx, &b, amount_kobo)?;
        (
            vec![
                LineSpec::new(&sys(&tx, company_id, "OWNER_DRAWINGS")?, amount_kobo),
                LineSpec::new(&b.account_id, -amount_kobo),
            ],
            "drawing",
            "Owner took money out",
        )
    } else {
        (
            vec![
                LineSpec::new(&b.account_id, amount_kobo),
                LineSpec::new(&sys(&tx, company_id, "OWNER_CAPITAL")?, -amount_kobo),
            ],
            "capital",
            "Owner put money in",
        )
    };
    let entry_id = post_entry_in(
        &tx, company_id, drawing_date, memo, source_type, None, ctx.user_id.as_deref(), &je,
    )?;
    tx.commit()?;
    Ok(entry_id)
}

// ===== Quick expense (Spec 01 §6.3, Spec 04 §2) =====

/// Cash expense paid now — no AP. Dr category (gross − backed-out VAT) /
/// Dr VAT Input / Cr Bank (gross − WHT) / Cr WHT Payable.
/// `payee` is display text; a contact is optional (Spec 04 §2: free text does
/// not create a contact). The bills-pipeline composite is UI-layer sugar; the
/// ledger effect is identical and this is the Spec 01 §5.3 `postExpense` surface.
#[allow(clippy::too_many_arguments)]
pub fn post_expense(
    conn: &mut Connection,
    company_id: &str,
    bank_account_id: &str,
    payee: &str,
    expense_account_id: &str,
    expense_date: &str,
    gross_kobo: i64,
    vat_inclusive: bool,
    wht_withheld_kobo: i64,
    ctx: &PostCtx,
) -> R<String> {
    if gross_kobo <= 0 || wht_withheld_kobo < 0 || wht_withheld_kobo >= gross_kobo {
        return Err(EngineError::Validation("invalid expense amounts".into()));
    }
    let c = cfg(conn, company_id)?;
    check_soft_close(&c, expense_date, ctx)?;
    let b = bank(conn, bank_account_id)?;
    check_recon_lock(&b, expense_date)?;
    let cash = gross_kobo - wht_withheld_kobo;
    check_cash_floor(conn, &b, cash)?;

    // Back out VAT from the entered amount (Spec 03 §6.3 / Spec 01 §6.3):
    // vat = paid × rate / (10000 + rate).
    let vat = if vat_inclusive && c.vat_registered {
        round_ratio(gross_kobo as i128 * c.vat_rate_bp as i128, (10_000 + c.vat_rate_bp) as i128)
    } else {
        0
    };

    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut je = vec![LineSpec::new(expense_account_id, gross_kobo - vat)];
    if vat > 0 {
        je.push(LineSpec::new(&sys(&tx, company_id, "VAT_INPUT")?, vat));
    }
    je.push(LineSpec::new(&b.account_id, -cash));
    if wht_withheld_kobo > 0 {
        je.push(LineSpec::new(&sys(&tx, company_id, "WHT_PAYABLE")?, -wht_withheld_kobo));
    }
    let entry_id = post_entry_in(
        &tx, company_id, expense_date,
        &format!("Money out — {payee}"),
        "expense", None, ctx.user_id.as_deref(), &je,
    )?;
    tx.commit()?;
    Ok(entry_id)
}

// ===== Customer deposits (Spec 03 §5.4) =====

/// Available deposit for a contact: the credit balance on Unearned Revenue lines
/// carrying that contact — a live query, never a stored balance.
pub fn deposit_balance(conn: &Connection, company_id: &str, contact_id: &str) -> R<i64> {
    Ok(conn.query_row(
        "SELECT COALESCE(-SUM(l.amount_kobo), 0)
         FROM journal_lines l
         JOIN journal_entries e ON e.id = l.entry_id
         JOIN accounts a ON a.id = l.account_id
         WHERE e.company_id = ?1 AND e.is_posted = 1
           AND a.system_key = 'UNEARNED_REVENUE' AND l.contact_id = ?2",
        params![company_id, contact_id],
        |r| r.get(0),
    )?)
}

/// Apply a held deposit to an open invoice: Dr Unearned Revenue / Cr AR.
/// Never automatic — the caller (UI) has shown the confirm prompt (Spec 03 §5.4).
pub fn apply_deposit(
    conn: &mut Connection,
    company_id: &str,
    contact_id: &str,
    invoice_id: &str,
    amount_kobo: i64,
    application_date: &str,
    ctx: &PostCtx,
) -> R<String> {
    if amount_kobo <= 0 {
        return Err(EngineError::Validation("amount must be positive".into()));
    }
    let c = cfg(conn, company_id)?;
    check_soft_close(&c, application_date, ctx)?;
    let (total, paid, status): (i64, i64, String) = conn
        .query_row(
            "SELECT total_kobo, amount_paid_kobo, status FROM invoices
             WHERE id = ?1 AND company_id = ?2 AND contact_id = ?3 AND kind = 'invoice'",
            params![invoice_id, company_id, contact_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?
        .ok_or_else(|| EngineError::Validation("unknown invoice for this customer".into()))?;
    if status != "sent" && status != "partially_paid" {
        return Err(EngineError::Validation("invoice is not open".into()));
    }
    if amount_kobo > total - paid {
        return Err(EngineError::Validation("application exceeds invoice balance".into()));
    }
    if amount_kobo > deposit_balance(conn, company_id, contact_id)? {
        return Err(EngineError::Validation("not enough deposit held for this customer".into()));
    }

    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let je = vec![
        LineSpec::with_contact(&sys(&tx, company_id, "UNEARNED_REVENUE")?, amount_kobo, contact_id),
        LineSpec::with_contact(&sys(&tx, company_id, "AR")?, -amount_kobo, contact_id),
    ];
    let entry_id = post_entry_in(
        &tx, company_id, application_date,
        "Customer's advance payment used for this invoice",
        "deposit_application", Some(invoice_id), ctx.user_id.as_deref(), &je,
    )?;
    recompute_invoice_status(&tx, invoice_id)?;
    tx.commit()?;
    Ok(entry_id)
}

/// Refund a held deposit: Dr Unearned Revenue / Cr Bank, as an outbound payment
/// row with no allocations (Spec 03 §5.4 / §8.3 refundDeposit).
pub fn refund_deposit(
    conn: &mut Connection,
    company_id: &str,
    contact_id: &str,
    bank_account_id: &str,
    refund_date: &str,
    amount_kobo: i64,
    ctx: &PostCtx,
) -> R<PaymentResult> {
    if amount_kobo <= 0 {
        return Err(EngineError::Validation("amount must be positive".into()));
    }
    let c = cfg(conn, company_id)?;
    check_soft_close(&c, refund_date, ctx)?;
    let b = bank(conn, bank_account_id)?;
    check_recon_lock(&b, refund_date)?;
    check_cash_floor(conn, &b, amount_kobo)?;
    if amount_kobo > deposit_balance(conn, company_id, contact_id)? {
        return Err(EngineError::Validation("not enough deposit held for this customer".into()));
    }
    let contact_name: String = conn.query_row(
        "SELECT name FROM contacts WHERE id = ?1", params![contact_id], |r| r.get(0),
    )?;

    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let je = vec![
        LineSpec::with_contact(&sys(&tx, company_id, "UNEARNED_REVENUE")?, amount_kobo, contact_id),
        LineSpec::new(&b.account_id, -amount_kobo),
    ];
    let entry_id = post_entry_in(
        &tx, company_id, refund_date,
        &format!("Advance payment returned — {contact_name}"),
        "payment", None, ctx.user_id.as_deref(), &je,
    )?;
    let payment_id = new_id();
    tx.execute(
        "INSERT INTO payments (id, company_id, direction, contact_id, bank_account_id, payment_date,
                               amount_kobo, wht_kobo, journal_entry_id, created_by, created_at)
         VALUES (?1, ?2, 'out', ?3, ?4, ?5, ?6, 0, ?7, ?8, ?9)",
        params![payment_id, company_id, contact_id, bank_account_id, refund_date,
                amount_kobo, entry_id, ctx.user_id, now_iso()],
    )?;
    tx.commit()?;
    Ok(PaymentResult { payment_id, entry_id, receipt_number: None, wht_kobo: 0, deposit_kobo: -amount_kobo })
}

// ===== Reversal (Spec 01 §6.8) =====

/// Core reversal inside the caller's transaction: negated twin, cross-linked.
fn void_entry_in(conn: &Connection, entry_id: &str, reversal_date: &str, ctx: &PostCtx) -> R<String> {
    let (company_id, memo, posted, reversed_by): (String, String, i64, Option<String>) = conn
        .query_row(
            "SELECT company_id, memo, is_posted, reversed_by_entry_id FROM journal_entries WHERE id = ?1",
            params![entry_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?
        .ok_or_else(|| EngineError::Validation("unknown entry".into()))?;
    if posted == 0 {
        return Err(EngineError::Validation("unposted entries have nothing to reverse".into()));
    }
    if reversed_by.is_some() {
        return Err(EngineError::Validation("entry is already reversed".into()));
    }
    let mut lines = Vec::new();
    {
        let mut q = conn.prepare(
            "SELECT account_id, amount_kobo, contact_id FROM journal_lines
             WHERE entry_id = ?1 ORDER BY line_no",
        )?;
        let iter = q.query_map(params![entry_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, Option<String>>(2)?))
        })?;
        for row in iter {
            let (acct, amt, contact) = row?;
            let mut l = LineSpec::new(&acct, -amt);
            l.contact_id = contact;
            lines.push(l);
        }
    }
    let rev_id = post_entry_in(
        conn, &company_id, reversal_date,
        &format!("REVERSAL of: {memo}"),
        "reversal", Some(entry_id), ctx.user_id.as_deref(), &lines,
    )?;
    conn.execute(
        "UPDATE journal_entries SET reverses_entry_id = ?2 WHERE id = ?1",
        params![rev_id, entry_id],
    )?;
    conn.execute(
        "UPDATE journal_entries SET reversed_by_entry_id = ?2 WHERE id = ?1",
        params![entry_id, rev_id],
    )?;
    Ok(rev_id)
}

/// Nothing is ever deleted: create the negated twin, cross-link both.
pub fn void_entry(
    conn: &mut Connection,
    entry_id: &str,
    reversal_date: &str,
    ctx: &PostCtx,
) -> R<String> {
    let company_id: String = conn
        .query_row(
            "SELECT company_id FROM journal_entries WHERE id = ?1",
            params![entry_id],
            |r| r.get(0),
        )
        .optional()?
        .ok_or_else(|| EngineError::Validation("unknown entry".into()))?;
    let c = cfg(conn, &company_id)?;
    check_soft_close(&c, reversal_date, ctx)?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let rev_id = void_entry_in(&tx, entry_id, reversal_date, ctx)?;
    tx.commit()?;
    Ok(rev_id)
}

/// Quote → invoice, one click (Spec 03 §3): fresh draft with a new INV number,
/// lines copied verbatim (quoted prices honored), quote marked converted
/// (terminal) and linked via converted_from_id. A quote converts at most once.
pub fn convert_quote(conn: &mut Connection, quote_id: &str, ctx: &PostCtx) -> R<String> {
    let (company_id, contact_id, status, kind): (String, String, String, String) = conn
        .query_row(
            "SELECT company_id, contact_id, status, kind FROM invoices WHERE id = ?1",
            params![quote_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?
        .ok_or_else(|| EngineError::Validation("unknown quote".into()))?;
    if kind != "quote" {
        return Err(EngineError::Validation("only quotes can be converted".into()));
    }
    if status == "converted" || status == "void" {
        return Err(EngineError::Validation("this quote is no longer open".into()));
    }
    let mut lines = Vec::new();
    {
        let mut q = conn.prepare(
            "SELECT product_id, description, quantity_milli, unit_price_kobo, discount_bp,
                    vat_applied, income_account_id
             FROM invoice_lines WHERE invoice_id = ?1 ORDER BY line_no",
        )?;
        let it = q.query_map(params![quote_id], |r| {
            Ok(InvoiceLineInput {
                product_id: r.get(0)?,
                description: r.get(1)?,
                quantity_milli: r.get(2)?,
                unit_price_kobo: r.get(3)?,
                discount_bp: r.get(4)?,
                vat_applied: r.get::<_, i64>(5)? != 0,
                income_account_id: r.get(6)?,
            })
        })?;
        for l in it {
            lines.push(l?);
        }
    }
    let today = now_iso()[..10].to_string();
    // create_invoice commits its own transaction; the linking updates follow. A crash
    // between them leaves only a spare unposted draft (no ledger effect, deletable) —
    // the quote stays open, so nothing financial can double-count.
    let inv = create_invoice(conn, &company_id, &contact_id, "invoice", &today, &today, &lines, ctx)?;
    conn.execute(
        "UPDATE invoices SET converted_from_id = ?2 WHERE id = ?1",
        params![inv, quote_id],
    )?;
    conn.execute(
        "UPDATE invoices SET status = 'converted' WHERE id = ?1",
        params![quote_id],
    )?;
    Ok(inv)
}

/// Void a payment (Spec 03 §6): reverse its entry, flag the row, recompute every
/// document it touched. Allocations remain as inactive history (excluded from
/// balances by the reversal + voided flag); a receipt number is never reused.
pub fn void_payment(
    conn: &mut Connection,
    payment_id: &str,
    reversal_date: &str,
    ctx: &PostCtx,
) -> R<String> {
    let (company_id, entry_id, voided): (String, String, i64) = conn
        .query_row(
            "SELECT company_id, journal_entry_id, voided FROM payments WHERE id = ?1",
            params![payment_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?
        .ok_or_else(|| EngineError::Validation("unknown payment".into()))?;
    if voided != 0 {
        return Err(EngineError::Validation("this payment is already cancelled".into()));
    }
    let c = cfg(conn, &company_id)?;
    check_soft_close(&c, reversal_date, ctx)?;

    let targets: Vec<(String, String)> = {
        let mut q = conn.prepare(
            "SELECT target_type, target_id FROM payment_allocations WHERE payment_id = ?1",
        )?;
        let it = q.query_map(params![payment_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
        it.collect::<Result<_, _>>()?
    };

    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let rev = void_entry_in(&tx, &entry_id, reversal_date, ctx)?;
    tx.execute("UPDATE payments SET voided = 1 WHERE id = ?1", params![payment_id])?;
    for (tt, tid) in &targets {
        if tt == "invoice" {
            recompute_invoice_status(&tx, tid)?;
        } else {
            recompute_bill_status(&tx, tid)?;
        }
    }
    tx.commit()?;
    Ok(rev)
}

/// Document-level void (Spec 03 §6): blocked while money is attached; reverses
/// the entry AND restores inventory at the ORIGINAL movement cost, not current WAC.
pub fn void_invoice(
    conn: &mut Connection,
    invoice_id: &str,
    reversal_date: &str,
    ctx: &PostCtx,
) -> R<Option<String>> {
    let (company_id, status, paid, entry_id): (String, String, i64, Option<String>) = conn
        .query_row(
            "SELECT company_id, status, amount_paid_kobo, journal_entry_id FROM invoices WHERE id = ?1",
            params![invoice_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?
        .ok_or_else(|| EngineError::Validation("unknown invoice".into()))?;
    match status.as_str() {
        "draft" => {
            // Drafts posted nothing; the row is kept, the number stays visible.
            conn.execute("UPDATE invoices SET status = 'void' WHERE id = ?1", params![invoice_id])?;
            return Ok(None);
        }
        "sent" | "partially_paid" | "paid" => {}
        other => return Err(EngineError::Validation(format!("cannot void a {other} invoice"))),
    }
    if paid != 0 {
        return Err(EngineError::Validation(
            "money has been received against this invoice — first void the payment or move it to the customer's deposit (Spec 03 §6)".into(),
        ));
    }
    let c = cfg(conn, &company_id)?;
    check_soft_close(&c, reversal_date, ctx)?;

    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let rev_id = match &entry_id {
        Some(eid) => {
            let rev = void_entry_in(&tx, eid, reversal_date, ctx)?;
            // Restore stock at original movement cost (Spec 01 §6.8).
            let now = now_iso();
            let mut q = tx.prepare(
                "SELECT product_id, quantity_milli, unit_cost_kobo, total_cost_kobo
                 FROM inventory_movements WHERE journal_entry_id = ?1 AND kind = 'sale'",
            )?;
            let rows: Vec<(String, i64, i64, i64)> = q
                .query_map(params![eid], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                })?
                .collect::<Result<_, _>>()?;
            drop(q);
            for (pid, qty, unit_cost, tot) in rows {
                tx.execute(
                    "INSERT INTO inventory_movements (id, company_id, product_id, movement_date, kind,
                                                      quantity_milli, unit_cost_kobo, total_cost_kobo,
                                                      journal_entry_id, created_at)
                     VALUES (?1, ?2, ?3, ?4, 'reversal', ?5, ?6, ?7, ?8, ?9)",
                    params![new_id(), company_id, pid, reversal_date, -qty, unit_cost, -tot, rev, now],
                )?;
            }
            Some(rev)
        }
        None => None, // zero-total free sample without stock lines: document-only void
    };
    tx.execute(
        "UPDATE invoices SET status = 'void', voided_by_entry = ?2 WHERE id = ?1",
        params![invoice_id, rev_id],
    )?;
    tx.commit()?;
    Ok(rev_id)
}
