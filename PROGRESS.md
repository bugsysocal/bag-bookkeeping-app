# LedgerOne — Build Progress
**Approach:** spec-first, per planning doc §11 order. Each spec is reviewed and approved before any code is scaffolded. **No code exists yet — by design.**
Planning doc: `C:\Users\Boyega\Downloads\SME_Finance_App_Planning_Document.md` · Stack: **Tauri + Rust + local SQLite** (agreed 2026-07-03).

## Spec status

| # | Spec | File | Status |
|---|---|---|---|
| 01 | Data model + posting engine | [specs/01-data-model-and-posting-engine.md](specs/01-data-model-and-posting-engine.md) | ✅ **Approved** (v0.2 — tax rules corrected to NTA 2025 / WHT Regs 2024: input VAT on all taxable inputs; `vat_exempt` ₦50M and `cit_exempt` ₦100M/₦250M as **separate independent flags**; ₦2M/TIN calendar-month WHT deduction exemption gated on `cit_exempt`) |
| 02 | COA seed + setup wizard | [specs/02-coa-seed-and-setup-wizard.md](specs/02-coa-seed-and-setup-wizard.md) | ✅ **Approved** (revised: tax flags **Advisor Mode only**, owner gets non-editable ledger-derived threshold banner; re-derivation reminder tied to `fiscal_year_start_month`) |
| 03 | Invoicing + payments | [specs/03-invoicing-and-payments.md](specs/03-invoicing-and-payments.md) | ✅ **Approved** — NGN-only invoicing confirmed for v1; all §9 decisions confirmed **except #9 (zero-total invoices): PENDING — review response was ambiguous; resolve first thing next session** |
| 04 | Expenses/bills + banking/reconciliation | — | ⏭️ **Next.** Carries a recorded obligation from Spec 02 decision #10: no Suspense account exists by design, so reconciliation must give accounts officers an explicit **"unclear — needs review" workflow state** — designed explicitly, not left implicit |
| 05–10 | Statements/reports · Excel I/O · Dashboard/modes UX · Backups · Sheets push | — | Not started (planning doc §11 order) |

## Open items
1. **Spec 03 decision #9** — zero-total invoices: keep blocked vs. allow (free samples / promo stock paper trail; COGS-at-WAC treatment already drafted in the spec).
2. **Spec 02 decision #9** — WHT preset rates (§4.2) still need confirmation against the current WHT Regulations 2024 schedule before ship (seed data, not structure).
3. **Spec 01 §6.2 note** — WHT exemption gated on `cit_exempt` is an interpretive call (WHT Regs "small company" read as the CIT definition); confirm against FIRS guidance with the practice.

## Standing design rules (from reviews)
- Double-entry rigor under the hood, owner-friendly surface; nothing posted is ever deleted (void/reverse only).
- Compliance-critical settings: **advisor-gated edits, owner-facing visibility** (banners derived from ledger data) — never owner edit power.
- Tax rates/thresholds are **data, not code**; reminders follow the fiscal year, not the calendar year.
