# LedgerOne — Key Decisions Digest (Specs 01–09)
**Status:** Specification phase COMPLETE, all nine specs approved 2026-07-03. This is the one-page orientation for anyone touching the project; each line names where the full normative text lives. Planning doc: `SME_Finance_App_Planning_Document.md` (Downloads). Stack: **Tauri + Rust + SQLite (rusqlite)**, Windows-first, offline-first.

## The five invariants (violate none of these, ever)

1. **Balanced or not at all.** Every financial event is one DB transaction; a journal entry posts only if its lines sum to zero (DB trigger T1, not app code). *(Spec 01 §4)*
2. **Nothing posted is ever deleted.** Corrections are reversal entries; documents void, never vanish; audit log is append-only — all trigger-enforced. *(Spec 01 §4, §6.8)*
3. **One posting surface.** UI, importers, reconciliation, and (later) integrations all write the ledger exclusively through the Spec 01 §6 functions. No side doors. *(Spec 01 P2; Spec 04 §7.4; Spec 06 §4)*
4. **No stored derived state.** Retained earnings, VAT credit carry-forward, cumulative WHT credit, account balances, invoice statuses — all live queries/recomputations; **no closing entries exist in this system** ("period lock" = edit-permission control, an unrelated mechanism — see the Spec 05 §4.3 terminology guard). *(Spec 05 #1/#8, §5.2)*
5. **Owner-language surface, advisor-gated depth.** Dr/Cr vocabulary is mechanically lint-banned from Owner Mode strings; every compliance-critical control is Advisor Mode (PIN) with owners getting non-editable, ledger-derived visibility (banners) instead of edit power. *(Spec 07 §4–5; Spec 02 #8)*

## Conventions
Integer **kobo** everywhere (no floats near money) · one signed amount per journal line (+Dr/−Cr) · ULID text keys · ledger always NGN with FX as line metadata · quantities in milliunits · rates in basis points · DD/MM/YYYY display · **tax rates/thresholds are data, not code**. *(Spec 01 §2)*

## Nigerian tax parameters (NTA 2025 / WHT Regs 2024 — verified 2026-07-03)
- **Two independent reliefs, never conflated:** `vat_exempt` (turnover ≤ ₦50M) and `cit_exempt` (≤ ₦100M **and** fixed assets ≤ ₦250M); professional-services firms excluded from both. A ₦70M business charges VAT yet is CIT-exempt. *(Spec 01 §3.1/§9)*
- **Input VAT** recoverable on **all** purchases attributable to taxable supplies (NTA §155(4)) — `vat_claimable` defaults on, per-line advisor override. *(Spec 01 §6.4)*
- **WHT:** recognized at **payment**, not billing. Small-company deduction exemption (gated on `cit_exempt`): supplier holds valid TIN **and** calendar-month aggregate to that supplier ≤ ₦2M — computed live at payment, never stored. No TIN → 2× rate offered as a warned default, never silent. *(Spec 01 §6.2; Spec 04 §3)*
- **VAT reports on calendar months** regardless of fiscal year (FIRS reality); everything else follows `fiscal_year_start_month`. *(Spec 05 #7)*
- ⚠️ Pre-ship checks still open: WHT preset rates vs. current Regulations schedule (Spec 02 §4.2); `cit_exempt`-gates-WHT-exemption interpretation vs. FIRS guidance (Spec 01 §6.2).

## The call that made each spec

| Spec | Key decision(s) |
|---|---|
| **01** Data model + engine | Trigger-enforced balance/immutability; drafts post nothing; WHT-at-payment; overpayments auto-become deposits |
| **02** COA + wizard | Flat 50-account chart (headers are report constructs); atomic company creation; 3 plain-language questions derive both tax flags; opening balances = one journal vs. OBE plug; tax flags Advisor-only + threshold banner from ledger data |
| **03** Invoicing + payments | Posted invoices immutable — void & reissue only; NGN-only invoicing v1 (FX receipts OK, realized diff → 4900); deposits never auto-applied; zero-total invoices allowed (COGS at WAC, no revenue/VAT — free-samples paper trail); WhatsApp = prefilled wa.me + PDF one drag away (honest platform limit) |
| **04** Expenses/bills + reconciliation | Quick expenses ride the bills pipeline (one VAT/WHT code path); **needs-review state replaces Suspense** — workflow quarantine on the statement line, nothing posted while flagged, mandatory note, carries forward, `completed_with_exceptions`; write-off routing + ₦5,000 threshold are advisor settings; cash boxes can't go negative, banks warn |
| **05** Statements & reports | Zero schema/engine deltas — reports are pure queries; no closing entries ever; cash-basis P&L is allocation-proportional with customer-withheld WHT counted as collected and deposits recognized on application; deposits never netted on aging |
| **06** Excel I/O | Five-stage pipeline, skip-bad-rows + exceptions file; **historical open invoices post Dr AR / Cr OBE — never revenue, never VAT** (already-filed income must not restate; phantom VAT liability must not arise) + per-customer anti-double-count guard; round-trip guard: exports are not import formats |
| **07** Dashboard + modes | **§5 capability table is THE normative mode-gating source** (supersedes scattered wording); Advisor Mode = PIN elevation, 15-min timeout, visible badge; banner discipline (priority order, max 2, every banner deep-links); "What I owe" names unremitted VAT/WHT so tax-in-hand never reads as spendable |
| **08** Backups | Every snapshot verified at creation (VACUUM INTO + integrity check + checksum) or deleted; fiscal-year-end snapshots kept **forever** ("books as filed"); restore sets the current db aside — never destroys data on the way to fixing a data problem; manifest lives outside the db it describes |
| **09** Sheets push (spec now, build Phase 2) | **Single computation path** — push serializes Spec 05/07 report outputs, has no queries of its own (a transport layer that can't compute can't drift); full-tab overwrite makes one-way physical; OAuth tokens in OS credential store, never SQLite (backups must not carry credentials); Sales-by-Customer tab for the concentration diagnostic |

## Working agreements (how this project reviews)
- Spec-first: nothing is scaffolded before its spec is approved; `PROGRESS.md` is the authoritative status file.
- Review rule: workflow/display decisions batch-approve after a scope audit; **anything touching how a regulatory-facing number (VAT, WHT, TB) is computed gets flagged individually** — a drafting error there is a filing error.
- Compliance controls: advisor edits, owner visibility; reminders follow the fiscal year, not the calendar.

*Next: scaffolding, in Spec 01 → 09 order so correctness compounds — engine and schema first, with the §4 triggers and posting templates as the first code and the first tests.*
