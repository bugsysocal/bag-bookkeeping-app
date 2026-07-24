//! Spec 08 — Backups & Data Safety. Pure, testable mechanics: snapshot +
//! verify, manifest read/write, and retention-ladder pruning. Scheduling
//! (timers, app-data-dir resolution, the restore command surface) is shell
//! concerns and lives in `src-tauri`; nothing here talks to Tauri.
//!
//! No schema changes (Spec 08 §7, deliberate) — backup state is
//! machine-level, not book-level, and lives entirely in files this module
//! manages (the manifest + a settings file the shell owns).

use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    #[error("storage error: {0}")]
    Io(#[from] std::io::Error),
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("the backup failed verification: {0}")]
    VerificationFailed(String),
    #[error("{0}")]
    Other(String),
}

type R<T> = Result<T, BackupError>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestEntry {
    pub id: String,
    pub timestamp: String, // ISO, when generated — also what retention buckets on
    pub filename: String,  // relative to the backup folder
    pub checksum: String,  // sha256 hex, of the file as written
    pub size_bytes: u64,
    pub verified: bool,
    pub app_version: String,
    pub schema_version: i64,
    /// "daily" | "weekly" | "monthly" | "fiscal_year_end" | "pre_restore" —
    /// informational only; `prune_retention` re-derives what to keep from
    /// the ladder rules each time, it doesn't trust this label.
    pub tier: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Manifest {
    pub entries: Vec<ManifestEntry>,
}

impl Manifest {
    pub fn load(path: &Path) -> Manifest {
        fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> R<()> {
        let json = serde_json::to_string_pretty(self).map_err(|e| BackupError::Other(e.to_string()))?;
        fs::write(path, json)?;
        Ok(())
    }

    pub fn last_success(&self) -> Option<&ManifestEntry> {
        self.entries.iter().filter(|e| e.verified).max_by(|a, b| a.timestamp.cmp(&b.timestamp))
    }
}

/// Spec 08 §2 Decision #1: open read-only, `PRAGMA integrity_check` must
/// return exactly "ok", and a trial query against `journal_lines` must
/// succeed. Used right after every snapshot, and again before a restore.
pub fn verify_snapshot(path: &Path) -> R<()> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let integrity: String = conn.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
    if integrity != "ok" {
        return Err(BackupError::VerificationFailed(format!("integrity check returned \"{integrity}\"")));
    }
    conn.query_row("SELECT COUNT(*) FROM journal_lines", [], |r| r.get::<_, i64>(0))
        .map_err(|e| BackupError::VerificationFailed(format!("trial query failed: {e}")))?;
    Ok(())
}

pub fn checksum_file(path: &Path) -> R<String> {
    let bytes = fs::read(path)?;
    Ok(format!("{:x}", Sha256::digest(&bytes)))
}

/// Spec 08 §2: `VACUUM INTO` (WAL-safe, transactionally consistent, compacted
/// — never a raw file copy of a hot db) + immediate verification. Any
/// failure deletes the snapshot and returns an error; nothing half-verified
/// is ever left on disk or in the manifest.
pub fn create_snapshot(
    conn: &Connection, dest_dir: &Path, filename: &str, app_version: &str, schema_version: i64, tier: &str,
) -> R<ManifestEntry> {
    fs::create_dir_all(dest_dir)?;
    let dest_path = dest_dir.join(filename);
    let escaped = dest_path
        .to_str()
        .ok_or_else(|| BackupError::Other("backup destination path is not valid UTF-8".into()))?
        .replace('\'', "''");
    conn.execute(&format!("VACUUM INTO '{escaped}'"), [])?;

    if let Err(e) = verify_snapshot(&dest_path) {
        let _ = fs::remove_file(&dest_path);
        return Err(e);
    }

    let checksum = checksum_file(&dest_path)?;
    let size_bytes = fs::metadata(&dest_path)?.len();

    Ok(ManifestEntry {
        id: crate::ids::new_id(),
        timestamp: crate::ids::now_iso(),
        filename: filename.to_string(),
        checksum,
        size_bytes,
        verified: true,
        app_version: app_version.to_string(),
        schema_version,
        tier: tier.to_string(),
    })
}

/// Spec 08 §3 retention ladder: daily (7), weekly (4), monthly (12), and
/// fiscal year-end (forever — the last verified snapshot strictly before
/// each `fiscal_year_start_month` rollover the manifest's history straddles,
/// per company). A generation kept by *any* tier survives. Week buckets are
/// a plain 7-day grouping from the epoch, not calendar ISO weeks — a
/// deliberate simplification for a rotation policy, not a financial figure.
/// Returns the filenames deleted, for the caller to log/report.
pub fn prune_retention(manifest: &mut Manifest, backup_dir: &Path, fiscal_start_months: &[u32]) -> Vec<String> {
    let mut newest_first = manifest.entries.clone();
    newest_first.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));

    let mut keep: HashSet<String> = HashSet::new();
    keep_latest_per_bucket(&newest_first, &mut keep, 7, |e| e.timestamp[0..10].to_string());
    keep_latest_per_bucket(&newest_first, &mut keep, 4, |e| (crate::reports::to_days(&e.timestamp[0..10]) / 7).to_string());
    keep_latest_per_bucket(&newest_first, &mut keep, 12, |e| e.timestamp[0..7].to_string());

    for &fsm in fiscal_start_months {
        for boundary in fiscal_boundaries_crossed(&newest_first, fsm) {
            if let Some(last_before) =
                newest_first.iter().filter(|e| e.timestamp.as_str() < boundary.as_str()).max_by(|a, b| a.timestamp.cmp(&b.timestamp))
            {
                keep.insert(last_before.id.clone());
            }
        }
    }
    // Pre-restore safety copies are never auto-pruned — they're the one
    // generation a human explicitly asked to be kept aside (Spec 08 §4).
    for e in &newest_first {
        if e.tier == "pre_restore" {
            keep.insert(e.id.clone());
        }
    }

    let (kept, dropped): (Vec<_>, Vec<_>) = manifest.entries.drain(..).partition(|e| keep.contains(&e.id));
    manifest.entries = kept;
    let mut deleted = Vec::new();
    for e in dropped {
        let _ = fs::remove_file(backup_dir.join(&e.filename));
        deleted.push(e.filename);
    }
    deleted
}

fn keep_latest_per_bucket(
    entries_newest_first: &[ManifestEntry], keep: &mut HashSet<String>, max_buckets: usize,
    bucket_of: impl Fn(&ManifestEntry) -> String,
) {
    let mut seen: HashSet<String> = HashSet::new();
    for e in entries_newest_first {
        let b = bucket_of(e);
        if seen.contains(&b) {
            continue;
        }
        if seen.len() >= max_buckets {
            break;
        }
        seen.insert(b);
        keep.insert(e.id.clone());
    }
}

fn fiscal_boundaries_crossed(entries: &[ManifestEntry], fiscal_start_month: u32) -> Vec<String> {
    if entries.is_empty() {
        return vec![];
    }
    let earliest = entries.iter().map(|e| e.timestamp[0..10].to_string()).min().unwrap();
    let latest = entries.iter().map(|e| e.timestamp[0..10].to_string()).max().unwrap();
    let y0: i64 = earliest[0..4].parse().unwrap();
    let y1: i64 = latest[0..4].parse().unwrap();
    let mut out = Vec::new();
    for y in y0..=y1 {
        let boundary = format!("{y:04}-{fiscal_start_month:02}-01");
        if boundary.as_str() > earliest.as_str() && boundary.as_str() <= latest.as_str() {
            out.push(boundary);
        }
    }
    out
}

/// Spec 08 §4 step 4: the live db is moved aside, never deleted, before a
/// restore copies a snapshot in. The safety copy gets its own manifest entry
/// (tier `pre_restore`) so it shows up like any other generation.
pub fn move_aside_for_restore(live_db_path: &Path, backup_dir: &Path) -> R<PathBuf> {
    fs::create_dir_all(backup_dir)?;
    let filename = format!("pre-restore-{}.db", crate::ids::now_iso().replace([':', '.'], "-"));
    let dest = backup_dir.join(&filename);
    fs::copy(live_db_path, &dest)?;
    Ok(dest)
}

pub struct RestorePreview {
    pub companies: Vec<String>,
    pub last_entry_date: Option<String>,
    pub journal_entry_count: i64,
}

/// Spec 08 §4 step 2: a read-only preview of a restore candidate before
/// anything happens to the live database.
pub fn preview_restore_candidate(path: &Path) -> R<RestorePreview> {
    verify_snapshot(path)?;
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut stmt = conn.prepare("SELECT name FROM companies ORDER BY created_at")?;
    let companies = stmt.query_map([], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
    drop(stmt);
    let last_entry_date: Option<String> =
        conn.query_row("SELECT MAX(entry_date) FROM journal_entries WHERE is_posted = 1", [], |r| r.get(0))?;
    let journal_entry_count: i64 = conn.query_row("SELECT COUNT(*) FROM journal_entries WHERE is_posted = 1", [], |r| r.get(0))?;
    Ok(RestorePreview { companies, last_entry_date, journal_entry_count })
}
