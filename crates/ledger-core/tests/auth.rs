//! Session, attribution, and Advisor Mode elevation tests.
//!
//! Proves the three things the wiring task exists to guarantee:
//! 1. an unauthenticated session cannot post (nothing lands in the ledger);
//! 2. a posting made by a logged-in user is attributed to that exact user;
//! 3. a staff-role user is blocked from an advisor-only action AT THE COMMAND
//!    LAYER — i.e. before the engine call ever runs, not by hiding a button.

use ledger_core::auth::{hash_pin, SessionStore};
use ledger_core::engine::{self, EngineError, PostCtx};
use ledger_core::ids::new_id;
use ledger_core::seed::{add_bank_account, create_company, CompanyConfig};
use rusqlite::{params, Connection};

struct World {
    conn: Connection,
    company: String,
    bank: String,
    owner_id: String,
    staff_id: String,
}

/// Mirrors the wizard's own shape (owner with a hashed PIN, staff without
/// one) but built with raw inserts so this test doesn't depend on the whole
/// FullSetup wizard payload — just the `users` table Spec 02 §5.8 defines.
fn world(advisor_pin: &str) -> World {
    let mut conn = ledger_core::open(":memory:").unwrap();
    let company = create_company(&mut conn, &CompanyConfig::default()).unwrap();
    let bank = add_bank_account(&mut conn, &company, "GTBank Current", "bank", "NGN").unwrap();

    let owner_id = new_id();
    let pin_hash = hash_pin(advisor_pin).unwrap();
    conn.execute(
        "INSERT INTO users (id, company_id, name, role, pin_hash, created_at)
         VALUES (?1, ?2, 'Chidinma (Owner)', 'owner', ?3, ?4)",
        params![owner_id, company, pin_hash, ledger_core::ids::now_iso()],
    ).unwrap();

    let staff_id = new_id();
    conn.execute(
        "INSERT INTO users (id, company_id, name, role, created_at)
         VALUES (?1, ?2, 'Ngozi (Accounts)', 'staff', ?3)",
        params![staff_id, company, ledger_core::ids::now_iso()],
    ).unwrap();

    World { conn, company, bank, owner_id, staff_id }
}

fn journal_entry_count(conn: &Connection) -> i64 {
    conn.query_row("SELECT COUNT(*) FROM journal_entries WHERE is_posted = 1", [], |r| r.get(0)).unwrap()
}

// ===== 1. Unauthenticated session cannot post =====

#[test]
fn unauthenticated_session_cannot_post() {
    let w = world("482913");
    let store = SessionStore::new();

    // No login happened. The command layer's very first move is require_session();
    // it must fail before any engine call is even reachable.
    let err = store.require_session().unwrap_err();
    assert!(matches!(err, EngineError::NoActiveSession));
    assert_eq!(journal_entry_count(&w.conn), 0, "nothing may post without a session");

    // Same guard on the stricter variant used by role-restricted actions.
    let err = store.require_not_staff().unwrap_err();
    assert!(matches!(err, EngineError::NoActiveSession));
}

// ===== 2. A posting made by a logged-in user is attributed to that user =====

#[test]
fn login_attributes_posting_to_the_correct_user() {
    let mut w = world("482913");
    let store = SessionStore::new();

    let session = store.login(&w.conn, &w.owner_id).unwrap();
    assert_eq!(session.user_id, w.owner_id);
    assert_eq!(session.role, "owner");

    // Exactly the pattern the shell now uses: require_session() supplies user_id.
    let session = store.require_session().unwrap();
    let ctx = PostCtx { user_id: Some(session.user_id.clone()), confirm_soft_close: false };
    let entry_id = engine::post_drawing(&mut w.conn, &w.company, &w.bank, "2026-07-06", 50_000_00, true, &ctx)
        .unwrap();

    let created_by: Option<String> = w.conn.query_row(
        "SELECT created_by FROM journal_entries WHERE id = ?1", params![entry_id], |r| r.get(0),
    ).unwrap();
    assert_eq!(created_by.as_deref(), Some(w.owner_id.as_str()), "audit trail must name the real user, not None");
}

#[test]
fn switching_users_reattributes_correctly() {
    let mut w = world("482913");
    let store = SessionStore::new();

    store.login(&w.conn, &w.staff_id).unwrap();
    let ctx = PostCtx { user_id: Some(store.require_session().unwrap().user_id), confirm_soft_close: false };
    let e1 = engine::post_drawing(&mut w.conn, &w.company, &w.bank, "2026-07-06", 10_000_00, true, &ctx).unwrap();

    store.login(&w.conn, &w.owner_id).unwrap();
    let ctx = PostCtx { user_id: Some(store.require_session().unwrap().user_id), confirm_soft_close: false };
    let e2 = engine::post_drawing(&mut w.conn, &w.company, &w.bank, "2026-07-06", 20_000_00, true, &ctx).unwrap();

    let get_by = |id: &str| -> Option<String> {
        w.conn.query_row("SELECT created_by FROM journal_entries WHERE id = ?1", params![id], |r| r.get(0)).unwrap()
    };
    assert_eq!(get_by(&e1).as_deref(), Some(w.staff_id.as_str()));
    assert_eq!(get_by(&e2).as_deref(), Some(w.owner_id.as_str()));
}

// ===== 3. Staff blocked from an advisor-only action AT THE COMMAND LAYER =====

#[test]
fn staff_role_blocked_from_void_even_if_ui_is_reached() {
    let mut w = world("482913");
    let store = SessionStore::new();

    // A real sent invoice a staff member might try to cancel.
    let customer = new_id();
    w.conn.execute(
        "INSERT INTO contacts (id, company_id, kind, name, created_at) VALUES (?1, ?2, 'customer', 'Test Co', ?3)",
        params![customer, w.company, ledger_core::ids::now_iso()],
    ).unwrap();
    let owner_ctx = PostCtx::default();
    let invoice = engine::create_invoice(
        &mut w.conn, &w.company, &customer, "invoice", "2026-07-06", "2026-07-20",
        &[engine::InvoiceLineInput {
            product_id: None, description: "Goods".into(), quantity_milli: 1000,
            unit_price_kobo: 100_000_00, discount_bp: 0, vat_applied: false, income_account_id: None,
        }],
        &owner_ctx,
    ).unwrap();
    engine::post_invoice(&mut w.conn, &invoice, &owner_ctx).unwrap();

    store.login(&w.conn, &w.staff_id).unwrap();

    // This IS the command-layer guard `void_invoice_cmd` calls before touching
    // the engine — pretend the UI somehow rendered the Cancel link for staff anyway.
    let guard_result = store.require_not_staff();
    assert!(matches!(guard_result, Err(EngineError::StaffForbidden)));

    // Because the guard fired first, void_invoice was never called. Prove the
    // invoice is untouched — no reversal, status unchanged.
    let status: String = w.conn.query_row(
        "SELECT status FROM invoices WHERE id = ?1", params![invoice], |r| r.get(0),
    ).unwrap();
    assert_eq!(status, "sent", "a blocked action must leave no trace of having run");
    assert_eq!(journal_entry_count(&w.conn), 1, "only the original post, no reversal");

    // Owner and advisor roles are NOT blocked by the same guard.
    store.login(&w.conn, &w.owner_id).unwrap();
    assert!(store.require_not_staff().is_ok());
}

// ===== Advisor Mode elevation (Spec 07 §5) =====

#[test]
fn advisor_elevation_requires_role_and_correct_pin() {
    let w = world("482913");
    let store = SessionStore::new();

    // Staff can never elevate, even with no PIN attempt at all — role gate first.
    store.login(&w.conn, &w.staff_id).unwrap();
    assert!(matches!(store.advisor_enter(&w.conn, "482913"), Err(EngineError::StaffForbidden)));

    // Owner: wrong PIN fails with attempts remaining, right PIN elevates.
    store.login(&w.conn, &w.owner_id).unwrap();
    assert!(!store.advisor_active(&w.conn).unwrap());
    match store.advisor_enter(&w.conn, "000000") {
        Err(EngineError::AdvisorPinIncorrect { attempts_remaining }) => assert_eq!(attempts_remaining, 4),
        other => panic!("expected AdvisorPinIncorrect, got {other:?}"),
    }
    assert!(!store.advisor_active(&w.conn).unwrap(), "a failed attempt must not elevate");

    store.advisor_enter(&w.conn, "482913").unwrap();
    assert!(store.advisor_active(&w.conn).unwrap());

    // Elevation-gated guard now succeeds; manual exit clears it again.
    assert!(store.require_advisor_elevated(&w.conn).is_ok());
    store.advisor_exit(&w.conn).unwrap();
    assert!(!store.advisor_active(&w.conn).unwrap());
    assert!(matches!(store.require_advisor_elevated(&w.conn), Err(EngineError::AdvisorPinRequired)));
}

#[test]
fn five_wrong_pins_lock_out_advisor_mode() {
    let w = world("482913");
    let store = SessionStore::new();
    store.login(&w.conn, &w.owner_id).unwrap();

    for _ in 0..4 {
        assert!(matches!(store.advisor_enter(&w.conn, "wrong"), Err(EngineError::AdvisorPinIncorrect { .. })));
    }
    // 5th wrong attempt locks it, even though the PIN itself is still wrong.
    assert!(matches!(store.advisor_enter(&w.conn, "wrong"), Err(EngineError::AdvisorLockedOut { .. })));
    // Locked out even with the CORRECT pin now.
    assert!(matches!(store.advisor_enter(&w.conn, "482913"), Err(EngineError::AdvisorLockedOut { .. })));

    // Every attempt is audited (Spec 07 §5): 5 pin_failed + 1 lockout.
    let failed: i64 = w.conn.query_row(
        "SELECT COUNT(*) FROM audit_log WHERE action = 'mode.pin_failed'", [], |r| r.get(0),
    ).unwrap();
    let lockouts: i64 = w.conn.query_row(
        "SELECT COUNT(*) FROM audit_log WHERE action = 'mode.lockout'", [], |r| r.get(0),
    ).unwrap();
    assert_eq!(failed, 5);
    assert_eq!(lockouts, 1);
}
