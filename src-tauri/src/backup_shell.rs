//! Spec 08 — the Tauri-side half of backups: scheduling, machine-level
//! settings (destination folder, enabled flag, last-attempt record — kept
//! outside the database per §7), and the command surface for the Settings
//! screen's Backups section and the restore flow. All snapshot/verify/
//! retention mechanics live in `ledger_core::backup`; nothing here touches
//! SQL beyond what that module already does.

use crate::{CmdError, Db};
use ledger_core::backup::{self, Manifest, ManifestEntry};
use ledger_core::rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager};

const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
/// Bump alongside `MIGRATIONS` in `ledger-core/src/db.rs` — informational
/// only (stored per-generation in the manifest), nothing reads it back to
/// gate behavior.
const SCHEMA_VERSION: i64 = 4;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupSettings {
    pub destination: Option<String>,
    pub enabled: bool,
    pub last_attempt_at: Option<String>,
    pub last_attempt_ok: bool,
    pub last_attempt_error: Option<String>,
}

impl Default for BackupSettings {
    fn default() -> Self {
        Self { destination: None, enabled: true, last_attempt_at: None, last_attempt_ok: true, last_attempt_error: None }
    }
}

fn settings_path(app: &AppHandle) -> Option<PathBuf> {
    app.path().app_data_dir().ok().map(|d| d.join("backup_settings.json"))
}

pub fn load_settings(app: &AppHandle) -> BackupSettings {
    settings_path(app)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_settings(app: &AppHandle, settings: &BackupSettings) {
    if let Some(p) = settings_path(app) {
        if let Ok(json) = serde_json::to_string_pretty(settings) {
            let _ = std::fs::write(p, json);
        }
    }
}

/// Spec 08 §3: user-chosen folder, default `Documents\LedgerOne Backups`.
pub fn destination_dir(app: &AppHandle, settings: &BackupSettings) -> PathBuf {
    if let Some(d) = &settings.destination {
        return PathBuf::from(d);
    }
    app.path()
        .document_dir()
        .map(|d| d.join("LedgerOne Backups"))
        .unwrap_or_else(|_| app.path().app_data_dir().unwrap().join("LedgerOne Backups"))
}

fn manifest_path(dir: &Path) -> PathBuf {
    dir.join("manifest.json")
}

fn hours_since(ts: &str) -> f64 {
    let now = ledger_core::ids::parse_iso_to_epoch_secs(&ledger_core::ids::now_iso());
    let then = ledger_core::ids::parse_iso_to_epoch_secs(ts);
    (now - then) as f64 / 3600.0
}

fn fiscal_start_months(conn: &Connection) -> Vec<u32> {
    let mut stmt = match conn.prepare("SELECT DISTINCT fiscal_year_start_month FROM companies") {
        Ok(s) => s,
        Err(_) => return vec![1],
    };
    let months: Vec<u32> = stmt
        .query_map([], |r| r.get::<_, i64>(0))
        .map(|rows| rows.filter_map(|r| r.ok()).map(|m| m as u32).collect())
        .unwrap_or_default();
    if months.is_empty() {
        vec![1]
    } else {
        months
    }
}

/// Spec 08 §2 point 3: an incremental mirror — new files copied, nothing
/// re-copied, deletions never propagated. Currently a no-op in practice: the
/// `attachments` table has no upload feature writing to it yet, so there's
/// nothing to mirror — but the mechanism is correct and ready for when there is.
fn mirror_attachments(conn: &Connection, backup_dir: &Path) {
    let mut stmt = match conn.prepare("SELECT stored_path FROM attachments") {
        Ok(s) => s,
        Err(_) => return,
    };
    let paths: Vec<String> = match stmt.query_map([], |r| r.get::<_, String>(0)) {
        Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
        Err(_) => return,
    };
    if paths.is_empty() {
        return;
    }
    let dest_dir = backup_dir.join("attachments");
    if std::fs::create_dir_all(&dest_dir).is_err() {
        return;
    }
    for p in paths {
        let src = Path::new(&p);
        if let Some(filename) = src.file_name() {
            let dest = dest_dir.join(filename);
            if !dest.exists() && src.exists() {
                let _ = std::fs::copy(src, &dest);
            }
        }
    }
}

fn snapshot_filename() -> String {
    format!("ledgerone-{}.db", ledger_core::ids::now_iso().replace([':', '.'], "-"))
}

/// Spec 08 §3: run one backup attempt if due. `force` bypasses both the
/// enabled flag and the time-since-last-success check (the manual "Back up
/// now" button). Triggers are simplified from the spec's "20h at launch,
/// then every 4h" into a single ">4h since last success" check, run every
/// ~15 minutes by the scheduler below and once at launch — the practical
/// effect is the same (a long-idle app backs up promptly at launch; a
/// running app backs up roughly every 4 hours) without two separate clocks.
pub fn run_backup_if_due(app: &AppHandle, force: bool) {
    let db_state = match app.try_state::<Db>() {
        Some(s) => s,
        None => return,
    };
    let mut settings = load_settings(app);
    if !settings.enabled && !force {
        return;
    }
    let due = force || settings.last_attempt_at.as_deref().map(|ts| hours_since(ts) > 4.0).unwrap_or(true);
    if !due {
        return;
    }

    let dir = destination_dir(app, &settings);
    let conn = db_state.0.lock().unwrap();
    let result = backup::create_snapshot(&conn, &dir, &snapshot_filename(), APP_VERSION, SCHEMA_VERSION, "daily");
    settings.last_attempt_at = Some(ledger_core::ids::now_iso());
    match result {
        Ok(entry) => {
            settings.last_attempt_ok = true;
            settings.last_attempt_error = None;
            let mut manifest = Manifest::load(&manifest_path(&dir));
            manifest.entries.push(entry);
            backup::prune_retention(&mut manifest, &dir, &fiscal_start_months(&conn));
            let _ = manifest.save(&manifest_path(&dir));
            mirror_attachments(&conn, &dir);
        }
        Err(e) => {
            settings.last_attempt_ok = false;
            settings.last_attempt_error = Some(e.to_string());
        }
    }
    drop(conn);
    save_settings(app, &settings);
}

/// Spec 08 §3: background thread, off the UI thread. Checked at launch and
/// every 15 minutes thereafter; `run_backup_if_due` itself decides whether
/// 4 hours have actually passed.
pub fn spawn_scheduler(app: &AppHandle) {
    let handle = app.clone();
    std::thread::spawn(move || loop {
        run_backup_if_due(&handle, false);
        std::thread::sleep(std::time::Duration::from_secs(15 * 60));
    });
}

// ===== CmdError mapping =====

impl From<backup::BackupError> for CmdError {
    fn from(e: backup::BackupError) -> Self {
        CmdError {
            code: "backup_error",
            message: format!(
                "Something went wrong with backups: {e}. Your books themselves are unaffected — only the backup copy is."
            ),
            detail: None,
        }
    }
}

// ===== Commands =====

#[derive(Serialize)]
pub struct BackupSettingsDto {
    destination: String,
    enabled: bool,
    last_attempt_at: Option<String>,
    last_attempt_ok: bool,
    last_attempt_error: Option<String>,
}

#[tauri::command]
pub fn backup_settings_get(app: AppHandle) -> Result<BackupSettingsDto, CmdError> {
    let settings = load_settings(&app);
    let dir = destination_dir(&app, &settings);
    Ok(BackupSettingsDto {
        destination: dir.to_string_lossy().to_string(),
        enabled: settings.enabled,
        last_attempt_at: settings.last_attempt_at,
        last_attempt_ok: settings.last_attempt_ok,
        last_attempt_error: settings.last_attempt_error,
    })
}

#[tauri::command]
pub fn backup_settings_update(
    app: AppHandle, sess: tauri::State<crate::Sess>, destination: Option<String>, enabled: bool,
) -> Result<(), CmdError> {
    sess.0.require_not_staff()?;
    let mut settings = load_settings(&app);
    settings.destination = destination;
    settings.enabled = enabled;
    save_settings(&app, &settings);
    Ok(())
}

#[tauri::command]
pub fn backup_now(app: AppHandle, sess: tauri::State<crate::Sess>) -> Result<(), CmdError> {
    sess.0.require_not_staff()?;
    run_backup_if_due(&app, true);
    let settings = load_settings(&app);
    if !settings.last_attempt_ok {
        return Err(CmdError {
            code: "backup_failed",
            message: format!(
                "The backup didn't complete: {}. Your books are safe — only the backup copy failed.",
                settings.last_attempt_error.unwrap_or_default()
            ),
            detail: None,
        });
    }
    Ok(())
}

#[derive(Serialize)]
pub struct ManifestEntryDto {
    id: String, timestamp: String, filename: String, size_bytes: u64, verified: bool, tier: String,
}

impl From<ManifestEntry> for ManifestEntryDto {
    fn from(e: ManifestEntry) -> Self {
        ManifestEntryDto { id: e.id, timestamp: e.timestamp, filename: e.filename, size_bytes: e.size_bytes, verified: e.verified, tier: e.tier }
    }
}

#[tauri::command]
pub fn backup_list(app: AppHandle) -> Result<Vec<ManifestEntryDto>, CmdError> {
    let settings = load_settings(&app);
    let dir = destination_dir(&app, &settings);
    let mut manifest = Manifest::load(&manifest_path(&dir));
    manifest.entries.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    Ok(manifest.entries.into_iter().map(Into::into).collect())
}

#[derive(Serialize)]
pub struct BackupHealthDto {
    state: &'static str, // "ok" | "warning" | "critical"
    message: Option<String>,
}

/// Spec 08 §5. Read by anyone (matches the dashboard's own read-anywhere
/// stance) — the banner is meant to be seen, not hidden behind elevation.
#[tauri::command]
pub fn backup_health(app: AppHandle) -> Result<BackupHealthDto, CmdError> {
    let settings = load_settings(&app);
    let dir = destination_dir(&app, &settings);
    let manifest = Manifest::load(&manifest_path(&dir));
    let last = manifest.last_success();
    let hours = last.map(|e| hours_since(&e.timestamp));
    let destination_unreachable = !settings.last_attempt_ok && settings.last_attempt_at.is_some();

    let state = match hours {
        None => "critical",
        Some(h) if h > 7.0 * 24.0 || destination_unreachable => "critical",
        Some(h) if h > 48.0 => "warning",
        _ => "ok",
    };
    let message = match state {
        "critical" if last.is_none() => Some("Your books haven't been backed up yet — set a backup destination in Settings.".to_string()),
        "critical" => Some(format!(
            "Backups are failing. If this computer is lost, records after {} go with it.",
            last.map(|e| e.timestamp[0..10].to_string()).unwrap_or_default()
        )),
        "warning" => Some(format!(
            "Your books haven't been backed up since {} — check the backup destination in Settings.",
            last.map(|e| e.timestamp[0..10].to_string()).unwrap_or_default()
        )),
        _ => None,
    };
    Ok(BackupHealthDto { state, message })
}

#[derive(Serialize)]
pub struct RestorePreviewDto {
    companies: Vec<String>, last_entry_date: Option<String>, journal_entry_count: i64,
}

#[tauri::command]
pub fn restore_preview(sess: tauri::State<crate::Sess>, candidate_path: String) -> Result<RestorePreviewDto, CmdError> {
    sess.0.require_not_staff()?;
    let preview = backup::preview_restore_candidate(Path::new(&candidate_path))?;
    Ok(RestorePreviewDto { companies: preview.companies, last_entry_date: preview.last_entry_date, journal_entry_count: preview.journal_entry_count })
}

/// Spec 08 §4: the guided, deliberately heavyweight restore. `typed_company_name`
/// must match one of the candidate's companies (the wizard-pattern typed
/// confirmation for irreversible-feeling actions). The current live db is
/// snapshotted aside first (tier `pre_restore`, kept forever, appears in the
/// manifest like any other generation) — restore never deletes the present.
#[tauri::command]
pub fn restore_backup(
    app: AppHandle, state: tauri::State<Db>, sess: tauri::State<crate::Sess>, candidate_path: String, typed_company_name: String,
) -> Result<(), CmdError> {
    let session = sess.0.require_not_staff()?;
    let preview = backup::preview_restore_candidate(Path::new(&candidate_path))?;
    if !preview.companies.iter().any(|n| n.eq_ignore_ascii_case(typed_company_name.trim())) {
        return Err(CmdError {
            code: "name_mismatch",
            message: "Type the exact name of a company in this backup to confirm — this replaces your current books.".into(),
            detail: None,
        });
    }

    let db_dir = app.path().app_data_dir().map_err(|e| CmdError { code: "backup_error", message: format!("could not find the app data folder: {e}"), detail: None })?;
    let db_path = db_dir.join("ledgerone.db");
    let settings = load_settings(&app);
    let backups_dir = destination_dir(&app, &settings);

    let mut guard = state.0.lock().unwrap();

    // 1. Safety snapshot of the CURRENT live db, before anything changes.
    let safety_entry = backup::create_snapshot(
        &guard, &backups_dir, &format!("pre-restore-{}.db", ledger_core::ids::now_iso().replace([':', '.'], "-")),
        APP_VERSION, SCHEMA_VERSION, "pre_restore",
    )?;
    let mut manifest = Manifest::load(&manifest_path(&backups_dir));
    manifest.entries.push(safety_entry);
    let _ = manifest.save(&manifest_path(&backups_dir));

    // 2. Drop the live connection so its file handle releases, then copy the
    //    verified candidate over the live path.
    *guard = ledger_core::open(":memory:").map_err(map_open_err)?;
    std::fs::copy(&candidate_path, &db_path)
        .map_err(|e| CmdError { code: "backup_error", message: format!("could not copy the backup into place: {e}"), detail: None })?;
    let _ = std::fs::remove_file(db_path.with_extension("db-wal"));
    let _ = std::fs::remove_file(db_path.with_extension("db-shm"));

    // 3. Reopen for real (this also runs any migrations the old backup predates).
    let new_conn = ledger_core::open(db_path.to_str().unwrap()).map_err(map_open_err)?;
    *guard = new_conn;

    // 4. Audit entry, written to the just-restored db as its first new entry.
    record_restore_audit(&guard, &preview, Some(&session.user_id));

    Ok(())
}

/// Small adapter so `rusqlite::Error` from a fresh `ledger_core::open` call
/// maps through the same `CmdError` path as everything else.
fn map_open_err(e: ledger_core::rusqlite::Error) -> CmdError {
    ledger_core::EngineError::Db(e).into()
}

fn record_restore_audit(conn: &Connection, preview: &backup::RestorePreview, user_id: Option<&str>) {
    let company_id: Option<String> =
        conn.query_row("SELECT id FROM companies ORDER BY created_at LIMIT 1", [], |r| r.get(0)).ok();
    if let Some(company_id) = company_id {
        let _ = conn.execute(
            "INSERT INTO audit_log (id, company_id, user_id, action, entity_type, entity_id, detail_json, created_at)
             VALUES (?1, ?2, ?3, 'restore', 'backup', ?2, ?4, ?5)",
            ledger_core::rusqlite::params![
                ledger_core::ids::new_id(), company_id, user_id,
                format!("{{\"journal_entry_count\":{}}}", preview.journal_entry_count),
                ledger_core::ids::now_iso()
            ],
        );
    }
}

