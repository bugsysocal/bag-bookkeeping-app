# Specification 06 — Excel Import & Export
**Project:** LedgerOne (placeholder) · **Covers:** Planning doc §6 (excl. 6.3 Sheets sync — Phase 2/Spec 10), §5.4 export-everything, §11 item 7 · **Status:** APPROVED v1.0 (2026-07-03; #2 individually confirmed as the key accounting judgment; #1/#3–#7 batch-approved under the reviewer's scope-audit rule)
**Depends on:** Specs 01–05. Governing principle (planning doc #6): **spreadsheets are bridges, never the database** — SQLite remains the sole source of truth on both sides of every flow here.

---

## 1. Scope

Import: onboarding importer (contacts, products, opening balances, open invoices/bills), bulk transaction import (advisor historical cleanup). Export: any register/report to `.xlsx`, FIRS-ready VAT/WHT schedules, full ledger export, "export everything." Bank statement import is already specified (Spec 04 §6) and shares this spec's parsing layer.

Implementation note (not a decision): read via `calamine` (xlsx/xls/csv), write via `rust_xlsxwriter` — both pure Rust, fully offline, no Excel installation required.

## 2. Import Pipeline (shared by all importers)

Every import runs the same five stages — **no import path writes to the database outside stage 5, and stage 5 only calls the approved posting/creation surface**:

1. **Parse** — file → raw rows (encoding-tolerant; xlsx/xls/csv).
2. **Map** — columns → fields. Template files (§3) skip this; arbitrary files get the mapping UI (same pattern as Spec 04 bank profiles). Dates parsed DD/MM/YYYY first, ISO fallback; amounts must parse as numbers — `"₦1,500,000"` text is cleaned (₦, commas, spaces stripped), anything else is a row error.
3. **Validate** — per row, against the same rules the forms enforce (a spreadsheet is not a validation bypass): required fields, known references, duplicate detection (§3 per importer).
4. **Preview** — full table shown with per-row status: ✓ ready · ⚠ warning (imports with note) · ✗ error (won't import) + reason in plain language. Nothing has been written yet.
5. **Commit** — valid rows import in one DB transaction; rejected rows export as an **exceptions file** — the original row plus a `Rejection reason` column — for fix-and-reimport. Re-importing a fixed exceptions file dedupes cleanly against the rows that already made it. *(Decision #1: skip-bad-rows + exceptions file, not all-or-nothing — an owner with 900 clean contacts and 40 dirty ones must not be blocked; an advisor doing cleanup gets a worklist instead of a mystery.)*

Every commit writes an `import_batches` row (§6) and audit-log entries; a batch is **not** undoable as a unit in v1 (posted entries follow void/reverse rules like everything else) — the preview stage is the safety, stated plainly on the commit button.

## 3. Onboarding Importers (the adoption-critical path)

Downloadable templates (one workbook, one sheet per type, header comments explaining each column) — or map-your-own via stage 2.

| Importer | Columns (template) | Dedup rule | Creates |
|---|---|---|---|
| **Contacts** | name*, kind (customer/supplier/both), phone, email, TIN, address, terms days, | name + phone (warn ⚠, skip exact re-import) | `contacts` rows |
| **Products** | name*, kind, SKU, sale price, cost, VAT yes/no, qty on hand†, unit cost† | name or SKU | `products` (+ opening stock movement† at entered unit cost → WAC baseline) |
| **Open invoices** | invoice no.*, customer*, issue date*, due date*, original total*, **balance outstanding*** | company + invoice no. | see §3.1 |
| **Open bills** | supplier*, their ref, bill date*, due date*, balance outstanding*, WHT flag + rate | supplier + ref + date | mirror of §3.1 on the AP side |

† only when `inventory_enabled`. Customer/supplier names resolve against existing contacts (exact, then case/space-insensitive with ⚠ confirm); unknown names auto-create contact stubs (⚠).

### 3.1 Open-invoice import — the correctness rule that matters

Historical open invoices import as **posted invoice documents whose journal entry is Dr AR (balance outstanding, per contact) / Cr 3000 Opening Balance Equity** — dated the invoice's issue date (or the opening-balance date if the period is locked), `source_type='opening_balance'`.

- **Never Cr Revenue, never Cr VAT Output:** that revenue and VAT belong to the prior system's periods — re-recognizing them would double-count income and create phantom VAT liability on the next FIRS filing.
- Only the **outstanding balance** posts (an 60%-paid invoice enters at its 40%); the PDF/original-total field is retained for display so the customer's statement reads sensibly.
- **Anti-double-count guard:** the wizard's "who owes you" lump entries (Spec 02 §5.5) and detailed invoice imports are mutually exclusive *per customer* — importing invoices for a customer who already has a lump opening AR line is blocked with a guided fix (advisor voids the lump line for that contact first). Same rule mirrored for bills/AP. *(Decision #2)*

The imported documents are real invoices thereafter: payments allocate against them, aging ages them, statements list them.

## 4. Bulk Transaction Import (Advisor Mode only)

For historical cleanup (planning doc §6.1). Template: date*, type* (`expense` / `receipt` / `supplier_payment` / `transfer` / `drawing`), amount*, bank account*, contact, category/account code, second account (transfers), memo, VAT-inclusive flag, WHT amount. Each valid row routes through the **matching Spec 01 posting function** — a bulk import is a fast hand, not a side door: every P-rule, period lock, and trigger applies per row. Rows land as individual entries (normal memos, normal audit), tagged with the batch id in the audit log. Advisor-only because category/account mapping errors at volume are an advisor-grade risk. *(Decision #3)*

Manual journals are deliberately **not** bulk-importable in v1 — a spreadsheet of raw Dr/Cr lines is the highest-risk import imaginable; the advisor posts those individually through `postJournal`. *(Decision #4)*

## 5. Export

### 5.1 Registers & reports
Every register (invoices, bills, payments, expenses, contacts, products) and every Spec 05 report exports to `.xlsx` with real formatting: typed columns (dates as dates, amounts as numbers with ₦ #,##0.00 format — never text), frozen header row, auto-filter on registers, company name + period + generation timestamp in a header block. What you see on screen is what lands in the file (current filters/period respected).

### 5.2 FIRS schedules
The Spec 05 §5 VAT report and WHT schedules export as filing-shaped workbooks (one sheet per month in the selected range) — these are the planning doc's "FIRS-ready" deliverables and the advisory practice's monthly handoff.

### 5.3 Full ledger & export-everything
- **Full ledger export** (advisor/auditor handoff): one workbook — Journal sheet (entry id, date, memo, source, account code/name, Dr, Cr, contact), TB-by-month sheet, COA sheet.
- **Export everything** (planning doc §5.4 — "the owner's data is never hostage"): a user-chosen folder gets one xlsx per register + the full ledger workbook + a README.txt stating what each file is and that this is a *readable copy*, **not a backup** (restore-capable backup is Spec 09's job — stated in the README precisely because an owner will otherwise assume it). One click, no options. *(Decision #5)*

### 5.4 Round-trip guard
Exports are **not** import formats. Report/register exports carry a `Generated by LedgerOne — for reading and analysis; re-importing this file is not supported` banner row, and importers reject files bearing it (except the designated templates, which carry a template marker instead). This is principle 6 enforced mechanically: the moment an exported sheet can round-trip, the spreadsheet starts becoming the database. *(Decision #6)*

## 6. Deltas (additive)

```sql
CREATE TABLE import_batches (
  id          TEXT PRIMARY KEY,
  company_id  TEXT NOT NULL REFERENCES companies(id),
  kind        TEXT NOT NULL CHECK (kind IN ('contacts','products','open_invoices','open_bills',
                                            'bulk_transactions','bank_statement')),
  filename    TEXT NOT NULL,
  rows_total  INTEGER NOT NULL,
  rows_ok     INTEGER NOT NULL,
  rows_error  INTEGER NOT NULL,
  created_by  TEXT REFERENCES users(id),
  created_at  TEXT NOT NULL
);
```
(Bank statement imports adopt this table too — Spec 04 §6 amended by reference.) No engine changes: importers create documents and post exclusively through the existing surface.

## 7. Decisions needing your sign-off

1. **Skip-bad-rows + exceptions file** rather than all-or-nothing imports; preview is the safety, batches are not unit-undoable. (§2)
2. ✅ **Open historical invoices post Dr AR / Cr OBE at outstanding balance — never revenue, never VAT** — with the per-customer anti-double-count guard against wizard lump entries. **CONFIRMED by reviewer 2026-07-03, with the rationale on record:** the revenue was already earned and (presumably) already reported to FIRS under the prior system — re-crediting 4000 on migration would restate income already filed on, and re-crediting 2210 on an already-filed invoice would create a phantom VAT liability that either double-pays or misstates the current period's return. OBE routing brings the balance onto the books without touching anything income-statement-facing; the double-count guard is the correct belt-and-suspenders check. (§3.1)
3. **Bulk transaction import is Advisor Mode only.** (§4)
4. **No bulk journal import in v1** — raw Dr/Cr spreadsheets are the one bridge too far. (§4)
5. **Export-everything = folder of xlsx + README explicitly disclaiming backup status.** (§5.3)
6. **Round-trip guard** — exports carry a banner and are rejected by importers; only marked templates import. (§5.4)
7. **Contacts/products dedup rules as specified** (name+phone / name-or-SKU, warn-and-skip). (§3)

---

*End of Spec 06. Next per §11 order: Spec 07 — Dashboard + Owner/Advisor mode UX (§11 item 8).*
