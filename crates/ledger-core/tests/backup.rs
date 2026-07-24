//! Backup mechanics tests (Spec 08): snapshot + verify, checksum, manifest
//! round-trip, retention ladder, and the restore preview.

use ledger_core::backup::{
    checksum_file, create_snapshot, move_aside_for_restore, preview_restore_candidate, prune_retention,
    verify_snapshot, Manifest, ManifestEntry,
};
use ledger_core::engine::PostCtx;
use ledger_core::ids::new_id;
use ledger_core::seed::{create_company, CompanyConfig};
use std::fs;
use std::path::PathBuf;

fn tempdir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ledgerone_backup_test_{}", new_id()));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn world_db_path(dir: &std::path::Path) -> String {
    // A real file-backed db, not :memory:, since VACUUM INTO needs a source
    // connection whose content ends up in the destination file either way,
    // but a real file exercises the exact same path the desktop app uses.
    let path = dir.join("source.db");
    path.to_str().unwrap().to_string()
}

#[test]
fn create_snapshot_produces_a_verified_manifest_entry() {
    let dir = tempdir();
    let db_path = world_db_path(&dir);
    let mut conn = ledger_core::open(&db_path).unwrap();
    create_company(&mut conn, &CompanyConfig::default()).unwrap();

    let backups = dir.join("backups");
    let entry = create_snapshot(&conn, &backups, "test-1.db", "0.1.0", 4, "daily").unwrap();
    assert!(entry.verified);
    assert_eq!(entry.filename, "test-1.db");
    assert!(backups.join("test-1.db").exists());
    assert_eq!(entry.checksum, checksum_file(&backups.join("test-1.db")).unwrap());

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn verify_snapshot_rejects_a_non_database_file() {
    let dir = tempdir();
    let bogus = dir.join("not-a-db.db");
    fs::write(&bogus, b"this is not a sqlite file").unwrap();
    assert!(verify_snapshot(&bogus).is_err());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn checksum_is_deterministic_and_content_sensitive() {
    let dir = tempdir();
    let a = dir.join("a.bin");
    let b = dir.join("b.bin");
    fs::write(&a, b"same content").unwrap();
    fs::write(&b, b"same content").unwrap();
    assert_eq!(checksum_file(&a).unwrap(), checksum_file(&b).unwrap());
    fs::write(&b, b"different content").unwrap();
    assert_ne!(checksum_file(&a).unwrap(), checksum_file(&b).unwrap());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn manifest_round_trips_through_json() {
    let dir = tempdir();
    let path = dir.join("manifest.json");
    let mut m = Manifest::default();
    m.entries.push(ManifestEntry {
        id: "abc".into(), timestamp: "2026-07-19T10:00:00Z".into(), filename: "x.db".into(),
        checksum: "deadbeef".into(), size_bytes: 1234, verified: true,
        app_version: "0.1.0".into(), schema_version: 4, tier: "daily".into(),
    });
    m.save(&path).unwrap();
    let loaded = Manifest::load(&path);
    assert_eq!(loaded.entries.len(), 1);
    assert_eq!(loaded.entries[0].id, "abc");
    assert_eq!(loaded.entries[0].size_bytes, 1234);
    let _ = fs::remove_dir_all(&dir);
}

fn make_entry(id: &str, timestamp: &str, tier: &str) -> ManifestEntry {
    ManifestEntry {
        id: id.into(), timestamp: timestamp.into(), filename: format!("{id}.db"),
        checksum: "x".into(), size_bytes: 1, verified: true,
        app_version: "0.1.0".into(), schema_version: 4, tier: tier.into(),
    }
}

fn touch(dir: &std::path::Path, filename: &str) {
    fs::write(dir.join(filename), b"x").unwrap();
}

#[test]
fn prune_keeps_only_seven_most_recent_daily_buckets_within_one_month() {
    let dir = tempdir();
    let mut m = Manifest::default();
    // 15 consecutive days, all within March 2026 (one calendar month, no
    // fiscal boundary) so only the daily/weekly/monthly rules are in play.
    for day in 1..=15 {
        let id = format!("d{day}");
        let ts = format!("2026-03-{day:02}T10:00:00Z");
        touch(&dir, &format!("{id}.db"));
        m.entries.push(make_entry(&id, &ts, "daily"));
    }
    prune_retention(&mut m, &dir, &[1]);
    // The 7 most recent days (9..15) must all survive via the daily tier,
    // regardless of what else the monthly/weekly tiers also keep.
    for day in 9..=15 {
        assert!(m.entries.iter().any(|e| e.id == format!("d{day}")), "day {day} should survive");
    }
    // Something from well before the daily/weekly window should be pruned —
    // day 1 is outside the last-7-days AND the last-4-weeks AND is not the
    // most recent in its own bucket for the monthly tier (day 15 is).
    assert!(!m.entries.iter().any(|e| e.id == "d1"), "day 1 should have been pruned");
    assert!(!dir.join("d1.db").exists(), "pruned entry's file should be deleted");

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn prune_keeps_the_last_snapshot_before_a_fiscal_year_boundary_forever() {
    let dir = tempdir();
    let mut m = Manifest::default();
    // Two entries before the Jan 1 2026 fiscal boundary (fiscal_start_month=1) ...
    touch(&dir, "old1.db");
    m.entries.push(make_entry("old1", "2025-12-20T10:00:00Z", "daily"));
    touch(&dir, "old2.db");
    m.entries.push(make_entry("old2", "2025-12-31T23:00:00Z", "daily")); // the true fiscal year-end
    // ... then many entries well after, in the new fiscal year, far enough
    // that the daily/weekly/monthly tiers would never reach back to December.
    for day in 1..=20 {
        let id = format!("new{day}");
        touch(&dir, &format!("{id}.db"));
        m.entries.push(make_entry(&id, &format!("2026-02-{day:02}T10:00:00Z"), "daily"));
    }

    prune_retention(&mut m, &dir, &[1]);

    assert!(m.entries.iter().any(|e| e.id == "old2"), "the last snapshot before the fiscal boundary must survive forever");
    assert!(!m.entries.iter().any(|e| e.id == "old1"), "an earlier December snapshot is not the fiscal year-end one and should be pruned");

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn prune_never_deletes_pre_restore_entries_regardless_of_age() {
    let dir = tempdir();
    let mut m = Manifest::default();
    touch(&dir, "safety.db");
    m.entries.push(make_entry("safety", "2020-01-01T00:00:00Z", "pre_restore"));
    for day in 1..=20 {
        let id = format!("new{day}");
        touch(&dir, &format!("{id}.db"));
        m.entries.push(make_entry(&id, &format!("2026-02-{day:02}T10:00:00Z"), "daily"));
    }
    prune_retention(&mut m, &dir, &[1]);
    assert!(m.entries.iter().any(|e| e.id == "safety"), "a pre-restore safety copy is never auto-pruned");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn restore_preview_reads_companies_and_journal_count_read_only() {
    let dir = tempdir();
    let db_path = world_db_path(&dir);
    let mut conn = ledger_core::open(&db_path).unwrap();
    let company = create_company(&mut conn, &CompanyConfig::default()).unwrap();
    let ctx = PostCtx::default();
    let bank = ledger_core::seed::add_bank_account(&mut conn, &company, "Main", "bank", "NGN").unwrap();
    let bank_coa: String = conn
        .query_row("SELECT account_id FROM bank_accounts WHERE id = ?1", ledger_core::rusqlite::params![bank], |r| r.get(0))
        .unwrap();
    let sales: String = conn
        .query_row(
            "SELECT id FROM accounts WHERE company_id = ?1 AND system_key = 'SALES_DEFAULT'",
            ledger_core::rusqlite::params![company], |r| r.get(0),
        )
        .unwrap();
    ledger_core::engine::post_journal(
        &mut conn, &company, "2026-07-01", "test", "manual", &ctx,
        &[ledger_core::LineSpec::new(&bank_coa, 100_00), ledger_core::LineSpec::new(&sales, -100_00)],
    )
    .unwrap();

    let backups = dir.join("backups");
    let entry = create_snapshot(&conn, &backups, "snap.db", "0.1.0", 4, "daily").unwrap();
    let preview = preview_restore_candidate(&backups.join(&entry.filename)).unwrap();
    assert_eq!(preview.journal_entry_count, 1);
    assert_eq!(preview.last_entry_date.as_deref(), Some("2026-07-01"));
    assert!(!preview.companies.is_empty());

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn move_aside_for_restore_copies_without_touching_the_live_file() {
    let dir = tempdir();
    let db_path = world_db_path(&dir);
    let mut conn = ledger_core::open(&db_path).unwrap();
    create_company(&mut conn, &CompanyConfig::default()).unwrap();
    drop(conn); // release the file lock so the plain fs::copy below can read it

    let backups = dir.join("backups");
    let safety_path = move_aside_for_restore(std::path::Path::new(&db_path), &backups).unwrap();
    assert!(safety_path.exists());
    assert!(std::path::Path::new(&db_path).exists(), "the live db must still exist — restore never deletes the present");
    assert!(verify_snapshot(&safety_path).is_ok());

    let _ = fs::remove_dir_all(&dir);
}
