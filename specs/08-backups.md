# Specification 08 — Backups & Data Safety
**Project:** LedgerOne (placeholder) · **Covers:** Planning doc §5.4, §11 item 9 · **Status:** APPROVED v1.0 (2026-07-03, all six decisions batch-approved; reviewer singled out as right calls: fiscal-year-end snapshots exempt from the retention ladder ("books as filed"), restore's never-destroy-on-the-way-to-fixing posture, and the manifest living outside the database it describes)
**Depends on:** Specs 01–07. Related but distinct: Spec 06 §5.3 export-everything is a *readable copy* for humans; this spec is *restore-capable protection* for the database. The README distinction shipped in Spec 06 exists because owners will conflate the two — the app must not.

---

## 1. Scope & Principles

Automatic local rotating backups, integrity verification, the restore flow, backup-health surfacing (the Spec 07 §2.3 banner slot #3), and the Phase 2 extension point for Google Drive. One principle above all: **a backup that was never test-opened is a hope, not a backup** — every snapshot is verified at creation, and failures are loud.

Layout note: the SQLite file is **one database for all companies** (Spec 01 — multi-company via `company_id`). A backup therefore protects the whole book of business; per-company extraction is Spec 06's export, not this spec.

## 2. What a Backup Is

A backup generation consists of:

1. **Database snapshot** — produced with `VACUUM INTO` on the live connection (WAL-safe, transactionally consistent, compacted). Never a file copy of a hot db.
2. **Verification, immediately, every time** — the snapshot is opened read-only, `PRAGMA integrity_check` must return `ok`, a trial query (`SELECT COUNT(*) FROM journal_lines`) must succeed, and a SHA-256 checksum is recorded. Fail any step → snapshot deleted, backup marked failed, banner fires. *(Decision #1)*
3. **Attachments mirror** — receipt photos/files (Spec 04) are immutable once written, so the backup folder keeps a single `attachments/` mirror synced incrementally: new files copied, nothing re-copied, deletions never propagated (attachment rows are never deleted anyway — void semantics). Cheap daily completeness instead of weekly zip bloat. *(Decision #2)*
4. **Manifest** — `manifest.json` in the backup folder (deliberately **outside** the database it describes): one entry per generation — timestamp, filename, checksum, size, verification result, app version, schema version. The Settings screen and health banner read the manifest, so backup status is knowable even when the main db is the thing that died.

## 3. Schedule & Retention

**Triggers:** app launch (if last successful backup > 20h old), every 4h while running, and on clean exit (if > 4h). Backups run in the background off the UI thread; the owner notices nothing when they succeed.

**Retention ladder** (pruned automatically after each successful generation):

| Tier | Keep | Cadence source |
|---|---|---|
| Daily | 7 | most recent generation per day |
| Weekly | 4 | most recent per ISO week |
| Monthly | 12 | most recent per calendar month |
| **Fiscal year-end** | **forever** | the last verified snapshot dated on/before each `fiscal_year_start_month` rollover, per company-set fiscal year — the "books as filed" snapshot an advisor reaches for years later *(Decision #3)* |

**Destination:** user-chosen folder, default `Documents\LedgerOne Backups`. Setup nudges toward a folder that leaves the machine (OneDrive-synced folder, external drive) and warns — once, dismissibly — when the destination is the same physical disk as the app data (*"If this laptop is stolen, this backup goes with it."*). Destination unreachable (USB unplugged) → backup skipped, health state degrades (§5).

## 4. Restore

Guided, deliberately heavyweight — restore is the one flow where slowing the user down is a feature:

1. Pick a manifest entry (or a bare `.db` file from anywhere — checksum verified against manifest when available, integrity-checked regardless).
2. Preview panel from the candidate, read-only: companies contained, last entry date, journal entry count — *"You are about to return to your books as of 28/06/2026. Everything recorded after that moment will be gone from the app (your current books are kept aside as a safety copy)."*
3. Typed confirmation of the company name (Spec 02 wizard pattern for irreversible-feeling actions).
4. The current live db is moved aside as `pre-restore-{timestamp}.db` — **restore never deletes the present**, it sets it aside, and the safety copy appears in the manifest like any other generation.
5. Snapshot copied in, attachments mirror left in place (superset by construction — no attachment referenced by an older db is ever missing from it).
6. First session after restore: audit_log records the restore event (backup id, checksum, who) — written to the *restored* db as its first new entry, and to the manifest.

Owner and advisor roles may restore; staff may not. *(Decision #4)*

## 5. Backup Health (the Spec 07 banner slot #3)

Backups are silent when healthy, loud when not. Health states, evaluated from the manifest at app launch and after each attempt:

| State | Trigger | Surface |
|---|---|---|
| OK | last verified success < 48h | Settings shows green + last time; no banner |
| Warning | 48h–7 days since success, or destination unreachable on last attempt | Banner: *"Your books haven't been backed up since Tuesday — plug in the backup drive or check Settings."* |
| Critical | > 7 days, or last snapshot failed verification | Banner (priority within slot 3 rises above Warning): *"Backups are failing. If this computer is lost, records after 24/06 go with it."* + one-tap **Back up now** |

Every banner deep-links to the fixing action per Spec 07 §2.3 discipline. A manual **Back up now** button lives in Settings permanently. *(Decision #5)*

## 6. Google Drive (Phase 2 extension point — designed, not built)

Per planning doc §5.4/§9: one-click cloud copy is Phase 2. This spec fixes only the seam so Phase 2 bolts on without rework: backup generations flow through a `BackupSink` interface (v1 ships exactly one implementation: local folder). The Drive sink will upload the **same verified snapshot + manifest entry** — verification stays local and mandatory; the cloud never receives an unverified byte. OAuth tokens, when that day comes, stored locally encrypted (planning doc §6.3). Nothing else about Drive is specified now. *(Decision #6)*

## 7. Deltas

**None to the database schema** — deliberately. Backup state (destination path, enabled flag, last-attempt record) is machine-level, not book-level, and lives in the Tauri app-config file alongside the manifest; the one thing worse than no backup status is backup status stored only inside the database that needs backing up. No engine changes.

## 8. Decisions needing your sign-off

None compute or post a regulatory figure (standing rule: audited — this spec never touches the ledger at all; restore replaces the whole file through SQLite's own mechanisms). The one to read closely is **#4 — restore semantics**, since it's the only destructive-adjacent flow in the app:

1. **`VACUUM INTO` + immediate verify-every-snapshot** (integrity_check + trial query + checksum); failed snapshots are deleted and reported, never retained. (§2)
2. **Attachments as an incremental mirror**, not per-generation archives — leans on their immutability. (§2)
3. **Retention ladder incl. fiscal-year-end snapshots kept forever.** (§3)
4. **Restore flow:** preview → typed confirmation → current db set aside as a manifest-visible safety copy (never deleted) → audit entry post-restore. Whole-file restore only (per-company recovery = Spec 06 export). Owner + advisor roles only. (§4)
5. **Health states at 48h/7d thresholds** with escalating banner copy and one-tap fix. (§5)
6. **Drive deferred behind a `BackupSink` seam**; verification always local. (§6)

---

*End of Spec 08. Next per §11 order: Spec 09 — Google Sheets one-way push (§11 item 10) — the final spec before scaffolding begins.*
