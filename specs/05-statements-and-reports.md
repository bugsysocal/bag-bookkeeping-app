# Specification 05 — Statements & Reports
**Project:** LedgerOne (placeholder) · **Covers:** Planning doc §4.6, §11 item 6 · **Status:** APPROVED v1.0 — all decisions confirmed 2026-07-03 (#7/#8 individually confirmed by reviewer as regulatory-facing items)
**Depends on:** Specs 01–04 (all approved). This spec adds **no schema and no engine surface** — every report is a pure query over `journal_lines`, which is precisely what the Spec 01 §7 invariants were built to buy. If a report needs a new table, something upstream is wrong.

---

## 1. Scope & Principles

Owner-tier reports (dashboard-first), formal statements (advisor-quality, print/PDF-ready), tax reports (VAT, WHT — FIRS-aligned), and contact statements of account. Export mechanics (xlsx rendering) are Spec 07; delivery (PDF/WhatsApp) reuses Spec 03 §7.

| # | Principle |
|---|---|
| R1 | **Auto-generated from the ledger, never assembled.** No report cell is editable; no report stores data. |
| R2 | **Fiscal-aware periods.** Month/quarter/YTD derive from `fiscal_year_start_month`; custom ranges allowed everywhere. |
| R3 | **Accrual is the ledger truth; cash basis is a P&L toggle** (§4.2) — never a different ledger. |
| R4 | **Two voices:** owner reports speak plain business language ("Money In"); formal statements use accounting convention — negatives in parentheses, proper section headers, ₦ and DD/MM/YYYY throughout. |
| R5 | **Every number drills down.** Any figure taps through to its journal lines and on to source documents. Auditability is a UI feature. |
| R6 | Comparatives (prior period + prior year) default **on** for formal statements, off for owner reports. |

## 2. Classification Map (the one lookup reports share)

Accounts map to statement sections by `class` + code band + `system_key` — data already in the COA, no new columns:

| Statement section | Rule |
|---|---|
| Cash & Bank | `is_bank = 1` |
| Receivables | `AR` + 1320 WHT Receivable + 1400 + 1450 |
| Inventory | `INVENTORY` |
| Fixed Assets (net) | 1500–1589 cost − 1590 accumulated depreciation |
| Current Liabilities | 2100–2399 + 2500 |
| Loans | 2410–2420 |
| Equity | 3xxx + computed earnings (§4.3) |
| Cash-flow: Operating | net profit + 6950 add-back + Δ(receivables, inventory, prepayments, VAT/WHT nets, payables, accruals, customer deposits) |
| Cash-flow: Investing | Δ 15xx at cost |
| Cash-flow: Financing | Δ 2410, 2420, 3100, 3200 |

The indirect cash-flow statement **reconciles to Δ Cash & Bank by construction** — the three categories partition all non-bank accounts, and the trial balance sums to zero (Spec 01 T1), so the statement cannot fail to tie.

## 3. Owner-Tier Reports (dashboard-first, planning doc §4.6.1–4)

1. **Cash position** — per Spec 04 §5: every active account, NGN balance, consolidated total; FX accounts show native balance alongside.
2. **Who owes me / Whom do I owe** — open invoices/bills grouped per contact, aged by **days past `due_date`**: `Current · 1–30 · 31–60 · 61–90 · 90+`. Customer deposits shown as a separate line per contact, **never netted** against what they owe (a deposit for order B doesn't reduce invoice A — netting would misstate both). Tap-through: contact → open documents → payments.
3. **Profit this month/quarter** — simplified accrual P&L: *Money In (sales) − Cost of goods − Running costs = Profit*. Beneath it, one deliberate note line: *"You took out ₦X this period (not a business cost)"* — drawings surfaced for honesty, excluded from profit, phrased guilt-free per the planning doc.
4. **Sales by customer / by product** — from posted `invoice_lines` (net of line discounts, ex-VAT), period-selectable, both count and value, top-N with "everyone else" rollup.

## 4. Formal Statements (planning doc §4.6.5–8)

### 4.1 Income Statement
Sections: Revenue (4xxx except 4900) → COGS (5xxx) → **Gross Profit + margin %** → OpEx (6xxx) → Operating Profit → FX gain/loss + interest → **Net Profit**. Periods: month/quarter/YTD/custom; comparatives per R6; accrual/cash toggle per §4.2.

### 4.2 Cash-basis P&L (the toggle's algorithm — normative)
Derived from the accrual ledger via payments, not a second ledger:
- **Revenue:** recognized at payment allocation date, proportionally: `allocated_amount × (invoice net ÷ invoice total)` — the VAT slice of each collection is excluded. **WHT withheld by customers counts as collected** (the tax credit is collection in kind — Dr WHT Receivable is part of the settlement).
- Unallocated deposits are **not** revenue until applied (application date = recognition date).
- **Expenses:** quick expenses at payment date (they already settle same-day); bill costs at supplier-payment allocation date, proportionally, WHT-withheld portion counting as paid.
- Depreciation (6950) and other pure journals excluded; drawings/transfers were never P&L anyway.
- Header carries a fixed caption: *"Cash basis — derived from settled amounts; VAT excluded."* *(Decision #2)*

### 4.3 Balance Sheet
Sections per §2; **always balanced by construction** (Spec 01 §7). Equity block:
- 3000/3100/3200 at posted balances; 3200 Drawings shown as a deduction line.
- **Retained/Current Earnings are computed, never posted: LedgerOne has no closing entries, ever.** Retained Earnings line = 3900 posted balance (advisor adjustments only) **+ Σ all income/COGS/expense lines dated before the current fiscal year**; Current Year Earnings = same sum within the current fiscal year. No year-end close routine exists to run, mis-run, or reverse — one less thing an informal-system user can break, and the balance sheet is correct at any as-of date without ceremony. *(Decision #1, approved)*

> **Terminology guard — "period close" ≠ "closing entries."** These are unrelated mechanisms and must never be conflated: **period close** (Spec 01 §3.1 `soft_close_through`/`hard_close_through`, trigger T5; Spec 02 §5.9) is an **edit-permission lock** — it stops postings *into* past dates and posts nothing itself. **Closing entries** (zeroing temporary accounts into retained earnings) are the *rejected* legacy mechanism this decision replaces with live computation. A hard-closed year still has its earnings computed on the fly; the lock just guarantees the inputs to that computation stop moving. All specs use "period lock" for the former where ambiguity is possible.

### 4.4 Cash Flow Statement (indirect)
Per the §2 map: Operating (net profit + depreciation add-back + working-capital deltas) / Investing / Financing, closing with *Net change in cash = closing − opening Cash & Bank* — ties by construction. FX-denominated bank movements ride at their NGN ledger amounts (no translation adjustments in v1; unrealized FX is an advisor journal per Spec 01, and lands in Operating via 4900 when posted).

### 4.5 Trial Balance & General Ledger (Advisor Mode)
TB: as-of date, every account, Dr/Cr columns rendered from sign (Spec 01 §2), zero-balance suppression toggle, grand totals equal — displayed, because advisors check. GL detail: per account, period, opening balance, chronological lines with running balance, closing; filter by contact on AR/AP (the subledger view P8 paid for).

## 5. Tax Reports (planning doc §4.6.9)

Rendered only when relevant: VAT report hidden entirely when `vat_registered = 0`; WHT schedules hidden when no 2220/1320 activity ever.

### 5.1 VAT Report (monthly, FIRS-aligned)
Per calendar month (VAT filing is calendar-monthly regardless of fiscal year; due by the 21st of the following month — shown as a reminder line, not enforced):
- **Output VAT**: 2210 activity — invoice-level schedule (number, customer, TIN if held, net, VAT).
- **Input VAT**: 1310 activity — bill-level schedule (supplier, TIN, net, VAT claimed).
- **Net payable / (credit carried forward)**, with prior-month credit brought forward as a display line (computed from cumulative 2210 − 1310 net, not stored).
- Excel export is the FIRS-ready schedule (planning doc §6.2).

### 5.2 WHT Schedules
- **Remittance schedule** (what we withheld — 2220 by month): payment date, supplier, TIN, invoice ref, gross ex-VAT base, rate, WHT amount; total to remit. Doubles as the FIRS upload sheet.
- **Credit schedule** (what customers withheld from us — 1320): receipt date, customer, TIN, base, WHT suffered — the evidence pack for offsetting against CIT.
- **Cumulative WHT credit available is a live query** (Σ 1320 activity, less any advisor journals applying credits against CIT), never a stored balance — same no-stored-state discipline as computed retained earnings and the VAT credit carry-forward. Because WHT credit notes accumulate across periods toward CIT offset, a stored balance would be exactly the kind of number that drifts out of sync with actual collections; a query cannot drift. *(Added per review 2026-07-03.)*

## 6. Contact Statement of Account (planning doc §4.1)

Per customer or supplier, any period: opening balance, chronological documents (invoices/bills, payments, deposits and applications, voids shown struck-through), closing balance, aging strip at the bottom. PDF via the Spec 03 §7 pipeline including one-click WhatsApp share — this is the "send Chidinma her statement" feature, and it reuses everything already specified.

## 7. Deltas

**None.** No schema changes, no engine changes, no new settings. (Report layout/branding polish belongs to Spec 08 UX.)

## 8. Decisions — review status 2026-07-03

1. ✅ **No closing entries, ever** — **APPROVED** (reviewer: the right call, not merely acceptable — closing entries are manual-ledger legacy that add failure modes for zero benefit in a query-driven system). Confirmed no tension with period locks; see the §4.3 terminology guard: "period lock" (edit-permission control) vs "closing entries" (rejected mechanism) are unrelated. (§4.3)
2. ✅ **Cash-basis algorithm** — **APPROVED**, with two treatments confirmed on record as correct: (a) customer-withheld WHT counts as collected — the customer remits to FIRS on our behalf; economically we've been paid and hold a WHT credit note instead of cash; (b) deposits recognized only on application, never on receipt — the correct treatment for Nigeria's prepayment-heavy SME trade. (§4.2)
3. ✅ **Aging by days past due date** — approved via scope audit: owner-facing display, no regulatory number touched. (§3.2)
4. ✅ **Deposits never netted** — approved via scope audit: affects aging/contact statements only; the balance sheet already reads 1100 and 2300 separately from the ledger, so netting was never a balance-sheet risk. (§3.2)
5. ✅ **Drawings note line on owner P&L** — approved via scope audit: owner-facing display. (§3.3)
6. ✅ **Negatives in parentheses + comparatives default-on** — approved via scope audit: pure presentation; touches the *rendering* of formal statements including the TB, but no computation. (R4/R6)
7. ✅ **Calendar-month VAT periods — CONFIRMED by reviewer 2026-07-03:** FIRS VAT filing is monthly by calendar month regardless of a business's fiscal year; there is no fiscal-year VAT concept in Nigerian practice. Approved as drafted. (§5.1)
8. ✅ **VAT credit carry-forward as live query — CONFIRMED by reviewer 2026-07-03**, with the consistency rationale on record: same no-stored-state discipline as the WHT credit means one less rule for a future advisor to remember. Approved. (§5.1)

---

*End of Spec 05. Next per §11 order: Spec 06/07 — Excel import/export (planning doc §11 item 7).*
