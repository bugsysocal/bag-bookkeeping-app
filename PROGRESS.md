# LedgerOne — Build Progress
**Approach:** spec-first, per planning doc §11 order. Each spec is reviewed and approved before any code is scaffolded.
**🏁 SPECIFICATION PHASE COMPLETE (2026-07-03): all nine specs approved. Scaffolding is next.**
Orientation for anyone new: read [specs/00-key-decisions-digest.md](specs/00-key-decisions-digest.md) first — one page, all key decisions, pointers to normative text.
Planning doc: `C:\Users\Boyega\Downloads\SME_Finance_App_Planning_Document.md` · Stack: **Tauri + Rust + local SQLite** (agreed 2026-07-03).

## Spec status

| # | Spec | File | Status |
|---|---|---|---|
| 01 | Data model + posting engine | [specs/01-data-model-and-posting-engine.md](specs/01-data-model-and-posting-engine.md) | ✅ **Approved** (v0.2 — tax rules corrected to NTA 2025 / WHT Regs 2024: input VAT on all taxable inputs; `vat_exempt` ₦50M and `cit_exempt` ₦100M/₦250M as **separate independent flags**; ₦2M/TIN calendar-month WHT deduction exemption gated on `cit_exempt`) |
| 02 | COA seed + setup wizard | [specs/02-coa-seed-and-setup-wizard.md](specs/02-coa-seed-and-setup-wizard.md) | ✅ **Approved** (revised: tax flags **Advisor Mode only**, owner gets non-editable ledger-derived threshold banner; re-derivation reminder tied to `fiscal_year_start_month`) |
| 03 | Invoicing + payments | [specs/03-invoicing-and-payments.md](specs/03-invoicing-and-payments.md) | ✅ **Approved v1.1 — all decisions resolved** (NGN-only invoicing for v1; #9 resolved: zero-total invoices allowed for free samples/promo stock — COGS at WAC, zero revenue, confirm-warning; no JE when no inventory lines) |
| 04 | Expenses/bills + banking/reconciliation | [specs/04-expenses-bills-banking-reconciliation.md](specs/04-expenses-bills-banking-reconciliation.md) | ✅ **Approved v1.0 — all 10 decisions resolved** (#7 amended: write-off threshold + routing accounts are Advisor Mode company settings, seeded ₦5,000/6980/4200; #10: Spec 01 updated to v0.3 carrying the reconciliation `state` + matches-join-table shape as current-approved; #2–5/#8–9 approved via scope audit — none touch VAT/WHT posting or the balance guarantee) |
| 05 | Statements & reports | [specs/05-statements-and-reports.md](specs/05-statements-and-reports.md) | ✅ **Approved v1.0 — all decisions confirmed** (#7 calendar-month VAT periods and #8 computed credit carry-forward individually confirmed; no-closing-entries model + terminology guard [period *lock* ≠ closing *entries*]; live-query discipline extended to WHT cumulative credit) |
| 06 | Excel import/export | [specs/06-excel-import-export.md](specs/06-excel-import-export.md) | ✅ **Approved v1.0** (#2 confirmed with rationale on record: historical open invoices post Dr AR / Cr OBE — never revenue/VAT — to avoid restating filed income or creating phantom VAT liability; rest batch-approved via scope audit) |
| 07 | Dashboard + Owner/Advisor Mode UX | [specs/07-dashboard-and-modes-ux.md](specs/07-dashboard-and-modes-ux.md) | ✅ **Approved v1.0** (#8 unremitted-tax line confirmed with rationale on record; §5 capability table endorsed as the single normative mode-gating source, superseding scattered wording in Specs 01–06) |
| 08 | Backups | [specs/08-backups.md](specs/08-backups.md) | ✅ **Approved v1.0** (batch; reviewer named as right calls: forever-kept fiscal-year-end snapshots, never-destroy restore posture, manifest outside the db) |
| 09 | Google Sheets push | [specs/09-google-sheets-push.md](specs/09-google-sheets-push.md) | ✅ **Approved v1.0** (#1 single-computation-path and #4 tokens-in-OS-store individually confirmed; #5 amended: **Sales by Customer tab added** — advisor's monthly customer-concentration diagnostic. Spec now, build Phase 2) |
| — | **Key-decisions digest** | [specs/00-key-decisions-digest.md](specs/00-key-decisions-digest.md) | ✅ **Filed** — one-page consolidation of all nine specs; the project's orientation document and CLAUDE.md seed |

## Open items
1. **▶ Resume point: posting-engine templates.** Next code: Spec 01 §6 template functions (`post_invoice`, `post_payment`, `post_expense`, `post_bill`, `post_transfer`, `post_drawing`, `post_journal`) in `ledger-core`, each with template tests against the Spec 01 §6 Dr/Cr tables — then Spec 02 COA seed + company-creation function. Tauri app shell + Node install after the engine is proven.

## Build status (updated 2026-07-04)
- ✅ **Workspace scaffolded**: `crates/ledger-core` (UI-independent accounting core; Tauri shell joins the workspace later).
- ✅ **Schema live**: [0001_initial.sql](crates/ledger-core/migrations/0001_initial.sql) — all 27 tables from Specs 01–09 + T1–T8 triggers; migration runner via `PRAGMA user_version`.
- ✅ **First engine slice**: `posting::post_entry` (one-transaction insert→flip protocol, P1/P3/P5 validation in front, triggers as backstop).
- ✅ **14/14 tests passing** — 12 trigger tests (T1–T8, including raw-SQL bypass attacks proving the DB stops unbalanced/tampering writes even without the harness, plus the trial-balance-zero induction test) + 2 date-math unit tests.
- **Environment notes**: machine is **Windows ARM64** (`aarch64-pc-windows-msvc`, Rust 1.96.1). VS Build Tools 2022 installed via winget — the ARM64 MSVC component (`VC.Tools.ARM64`) had to be added separately (the VCTools workload defaults assume x64 host); elevation for VS installer modifications must go through winget (`--force --override`), as direct `setup.exe --quiet` is blocked from non-elevated shells. `cargo` lives at `%USERPROFILE%\.cargo\bin` (PATH refresh needed in preexisting shells). **Node.js not yet installed** — needed when the Tauri shell is scaffolded; get the ARM64 build. `chrono` was dropped from ledger-core (autocfg build-script failure in this environment); timestamps are std-only civil-date math in `ids.rs`.
2. **Spec 02 decision #9** — WHT preset rates (§4.2) still need confirmation against the current WHT Regulations 2024 schedule before ship (seed data, not structure).
3. **Spec 01 §6.2 note** — WHT exemption gated on `cit_exempt` is an interpretive call (WHT Regs "small company" read as the CIT definition); confirm against FIRS guidance with the practice.

## Standing design rules (from reviews)
- Double-entry rigor under the hood, owner-friendly surface; nothing posted is ever deleted (void/reverse only).
- Compliance-critical settings: **advisor-gated edits, owner-facing visibility** (banners derived from ledger data) — never owner edit power.
- Tax rates/thresholds are **data, not code**; reminders follow the fiscal year, not the calendar year.
