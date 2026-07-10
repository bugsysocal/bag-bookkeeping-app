//! Session, attribution, and Advisor Mode elevation (Spec 07 §5).
//!
//! Business rule, not UI plumbing — this lives in ledger-core per the project's
//! one architectural law: the Tauri shell only wraps these functions in thin
//! IPC commands. Nothing here talks to Tauri; everything is plain and testable.
//!
//! Three ideas, matching Spec 07 §5 exactly:
//! 1. **A session identifies who is acting** — every posting call must carry a
//!    real `user_id` so the audit log is attributable (Spec 01 invariant #2).
//! 2. **Advisor Mode is a PIN elevation over the current session, not a
//!    separate login.** Eligible roles: `owner` and `advisor`; `staff` never.
//! 3. **Idle auto-exit and failed-PIN lockout** are session-local — this is a
//!    single-user desktop process; state lives in memory, not the database.

use crate::engine::EngineError;
use crate::ids::{new_id, now_iso};
use rusqlite::{params, Connection, OptionalExtension};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const LOCKOUT_ATTEMPTS: u32 = 5;
const LOCKOUT_MINUTES: u64 = 15;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub user_id: String,
    pub company_id: String,
    pub role: String, // 'owner' | 'staff' | 'advisor'
    pub name: String,
}

struct Inner {
    session: Session,
    last_activity: Instant,
    advisor_elevated_at: Option<Instant>,
    failed_pin_attempts: u32,
    locked_until: Option<Instant>,
}

/// Owned by the app (Tauri `manage`d state, or a test's local variable).
/// One store = one signed-in user for the lifetime of the process, matching
/// the desktop reality: re-selecting a user is how a shift change is modelled.
pub struct SessionStore(Mutex<Option<Inner>>);

impl Default for SessionStore {
    fn default() -> Self {
        Self(Mutex::new(None))
    }
}

impl SessionStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Establish a session for `user_id` (Spec 07: pick who's using it — no
    /// password gates ordinary attribution; the PIN is reserved for Advisor
    /// Mode elevation, per §5 "not a separate login").
    pub fn login(&self, conn: &Connection, user_id: &str) -> Result<Session, EngineError> {
        let (company_id, role, name): (String, String, String) = conn
            .query_row(
                "SELECT company_id, role, name FROM users WHERE id = ?1",
                params![user_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?
            .ok_or_else(|| EngineError::Validation("unknown user".into()))?;
        let session = Session { user_id: user_id.to_string(), company_id, role, name };
        *self.0.lock().unwrap() = Some(Inner {
            session: session.clone(),
            last_activity: Instant::now(),
            advisor_elevated_at: None,
            failed_pin_attempts: 0,
            locked_until: None,
        });
        Ok(session)
    }

    pub fn logout(&self) {
        *self.0.lock().unwrap() = None;
    }

    pub fn current(&self) -> Option<Session> {
        self.0.lock().unwrap().as_ref().map(|i| i.session.clone())
    }

    /// Every authenticated command calls this first: proves a user is
    /// identified (so `PostCtx.user_id` is never silently `None` again) and
    /// refreshes the idle clock the Advisor Mode timeout is measured against.
    pub fn require_session(&self) -> Result<Session, EngineError> {
        let mut guard = self.0.lock().unwrap();
        let inner = guard.as_mut().ok_or(EngineError::NoActiveSession)?;
        inner.last_activity = Instant::now();
        Ok(inner.session.clone())
    }

    /// Spec 02 role matrix: staff cannot void documents or reach settings.
    /// This blocks at the point of decision, not by hiding a button.
    pub fn require_not_staff(&self) -> Result<Session, EngineError> {
        let session = self.require_session()?;
        if session.role == "staff" {
            return Err(EngineError::StaffForbidden);
        }
        Ok(session)
    }

    /// Whether Advisor Mode is currently elevated for this session, applying
    /// the idle-timeout auto-exit (Spec 07 §5) as a side effect when it has
    /// lapsed — `conn` is used only to read the company's configured timeout
    /// and to write the `mode.timeout` audit row.
    pub fn advisor_active(&self, conn: &Connection) -> Result<bool, EngineError> {
        let mut guard = self.0.lock().unwrap();
        let Some(inner) = guard.as_mut() else { return Ok(false) };
        let Some(elevated_at) = inner.advisor_elevated_at else { return Ok(false) };
        let timeout_minutes: i64 = conn.query_row(
            "SELECT advisor_timeout_minutes FROM companies WHERE id = ?1",
            params![inner.session.company_id],
            |r| r.get(0),
        )?;
        let idle_for = inner.last_activity.max(elevated_at).elapsed();
        if idle_for > Duration::from_secs((timeout_minutes.max(1) as u64) * 60) {
            let company_id = inner.session.company_id.clone();
            let user_id = inner.session.user_id.clone();
            inner.advisor_elevated_at = None;
            audit(conn, &company_id, &user_id, "mode.timeout")?;
            return Ok(false);
        }
        Ok(true)
    }

    /// Guard for Advisor-only capabilities (Spec 07 §5 table): requires both
    /// the right role AND a currently-elevated session — role alone is not
    /// enough, matching "write-off above the limit is Advisor Mode only."
    pub fn require_advisor_elevated(&self, conn: &Connection) -> Result<Session, EngineError> {
        let session = self.require_not_staff()?;
        if !self.advisor_active(conn)? {
            return Err(EngineError::AdvisorPinRequired);
        }
        Ok(session)
    }

    /// PIN elevation (Spec 07 §5): argon2-verified against the company's
    /// Advisor Mode PIN (stored on the owner's row, Spec 02 §5.8 — one secret
    /// per company in v1). 5 failed attempts → 15-minute lockout; every
    /// transition (enter/failed/lockout) is audit-logged.
    pub fn advisor_enter(&self, conn: &Connection, pin: &str) -> Result<(), EngineError> {
        let session = self.require_not_staff()?;

        {
            let guard = self.0.lock().unwrap();
            if let Some(inner) = guard.as_ref() {
                if let Some(until) = inner.locked_until {
                    if Instant::now() < until {
                        let minutes_left = (until - Instant::now()).as_secs().div_ceil(60);
                        return Err(EngineError::AdvisorLockedOut { minutes_left });
                    }
                }
            }
        }

        let pin_hash: Option<String> = conn.query_row(
            "SELECT pin_hash FROM users WHERE company_id = ?1 AND role = 'owner'",
            params![session.company_id],
            |r| r.get(0),
        )?;
        let ok = pin_hash
            .as_deref()
            .map(|h| verify_pin(h, pin))
            .unwrap_or(false);

        let mut guard = self.0.lock().unwrap();
        let inner = guard.as_mut().ok_or(EngineError::NoActiveSession)?;
        if ok {
            inner.advisor_elevated_at = Some(Instant::now());
            inner.failed_pin_attempts = 0;
            inner.locked_until = None;
            drop(guard);
            audit(conn, &session.company_id, &session.user_id, "mode.entered")?;
            Ok(())
        } else {
            inner.failed_pin_attempts += 1;
            let attempts = inner.failed_pin_attempts;
            let locked = attempts >= LOCKOUT_ATTEMPTS;
            if locked {
                inner.locked_until = Some(Instant::now() + Duration::from_secs(LOCKOUT_MINUTES * 60));
                inner.failed_pin_attempts = 0;
            }
            drop(guard);
            audit(conn, &session.company_id, &session.user_id, "mode.pin_failed")?;
            if locked {
                audit(conn, &session.company_id, &session.user_id, "mode.lockout")?;
                Err(EngineError::AdvisorLockedOut { minutes_left: LOCKOUT_MINUTES })
            } else {
                Err(EngineError::AdvisorPinIncorrect { attempts_remaining: LOCKOUT_ATTEMPTS - attempts })
            }
        }
    }

    /// Manual exit — always allowed, always audited.
    pub fn advisor_exit(&self, conn: &Connection) -> Result<(), EngineError> {
        let session = self.require_session()?;
        self.0.lock().unwrap().as_mut().unwrap().advisor_elevated_at = None;
        audit(conn, &session.company_id, &session.user_id, "mode.exited")
    }
}

fn verify_pin(stored_hash: &str, pin: &str) -> bool {
    use argon2::password_hash::PasswordHash;
    use argon2::{Argon2, PasswordVerifier};
    let Ok(parsed) = PasswordHash::new(stored_hash) else { return false };
    Argon2::default().verify_password(pin.as_bytes(), &parsed).is_ok()
}

/// Hash a 6-digit Advisor Mode PIN for storage on the owner's `users` row
/// (Spec 02 §5.8). Shared by the wizard (`seed::create_company_full`) and
/// anywhere else a PIN needs setting, so there is exactly one hashing path.
pub fn hash_pin(pin: &str) -> Result<String, EngineError> {
    use argon2::password_hash::{rand_core::OsRng, SaltString};
    use argon2::{Argon2, PasswordHasher};
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(pin.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| EngineError::Validation(format!("PIN hashing failed: {e}")))
}

fn audit(conn: &Connection, company_id: &str, user_id: &str, action: &str) -> Result<(), EngineError> {
    conn.execute(
        "INSERT INTO audit_log (id, company_id, user_id, action, entity_type, entity_id, created_at)
         VALUES (?1, ?2, ?3, ?4, 'advisor_mode', ?2, ?5)",
        params![new_id(), company_id, user_id, action, now_iso()],
    )?;
    Ok(())
}
