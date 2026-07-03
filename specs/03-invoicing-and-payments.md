# Specification 03 — Invoicing & Payments Flow
**Project:** LedgerOne (placeholder) · **Covers:** Planning doc §4.2, §11 item 3 · **Status:** APPROVED v1.0 (2026-07-03) — **except decision #9 (zero-total invoices), pending confirmation at next session**
**Depends on:** Spec 01 (approved — posting templates §6.1/§6.2, void semantics §6.8), Spec 02 (approved — sequences, COA, roles).

---

## 1. Scope

Money-in, end to end: quotes, invoices, the invoice state machine, customer payments and allocations, WHT withheld by customers, deposits/prepayments and their application, receipts, voids, and delivery (PDF / WhatsApp / email). The *ledger effects* of all of this were fixed in Spec 01 — this spec defines the workflows, state transitions, validations, and documents wrapped around those posting calls, plus a handful of additive schema/engine deltas (§8).

Out of scope: supplier-side payments (with bills, Spec 04 — same `payments` table, `direction='out'` templates already in Spec 01 §6.2), customer statement-of-account PDFs (Spec 06 reports), keyboard-UX details (Spec 08).

---

## 2. Invoice State Machine

States stored in `invoices.status` (Spec 01): `draft → sent → partially_paid → paid`, terminal `void`. **`overdue` is derived, never stored:** `status IN ('sent','partially_paid') AND due_date < today`. Status after payment changes is always **recomputed from allocations**, never incrementally toggled:

```
balance = total_kobo − Σ(active allocations)      -- active = allocation's payment not voided
status  = paid            if balance = 0
        = partially_paid  if 0 < balance < total_kobo
        = sent            if balance = total_kobo
```

| From | Event | Guards | To | Effects |
|---|---|---|---|---|
| *(new)* | create | customer exists; ≥ 0 lines | `draft` | invoice + INV number consumed (Spec 01 §3.4); **no journal entry** |
| `draft` | edit | — | `draft` | free edit: lines, dates, customer — drafts are workpaper |
| `draft` | send | V1–V7 (§4); soft-close P4 | `sent` | **`postInvoice()`** (Dr AR / Cr Revenue / Cr VAT Output + COGS if inventory); `sent_at`; deposit-application prompt (§5.4); PDF available |
| `draft` | void | — | `void` | no JE existed; row kept, number visible in register (Spec 01 decision #5) |
| `sent` | record payment | via §5 | `partially_paid` / `paid` | recompute; receipt generated |
| `sent`/`partially_paid` | edit | **BLOCKED** | — | posted invoices are immutable — fix = void & reissue (§6) |
| `sent`/`partially_paid`/`paid` | void | **no active allocations** (§6) | `void` | `voidInvoice()`: reversal JE + inventory restore (Spec 01 §6.8) |
| any | payment voided | — | recomputed | may move `paid → partially_paid → sent` |

## 3. Quotes / Proforma

Quotes live in `invoices` with `kind='quote'`, own sequence (`QUO-`), and **never post** — no journal entry at any state. Lifecycle: `draft → sent → converted | void` (status CHECK extended, §8). **Convert** (one click, planning doc §4.2): creates a fresh `draft` invoice — new INV number, lines copied, prices re-defaulted **off** (quoted prices are honored verbatim; a warning shows if the product's list price has since changed), `converted_from_id` links back; quote → `converted` (terminal). A quote converts at most once; edit-and-reconvert means void the draft and convert again — the link keeps the trail.

## 4. Invoice Creation — Fields, Defaults, Validations

Form defaults: `issue_date` = today; `due_date` = issue + contact's `payment_terms_days` (editable); lines from product picker (price, description, `vat_applied` from `products.is_vatable`) or free-description; `income_account_id` from product, default 4000; per-line discount %; VAT column hidden entirely when `vat_registered = 0`.

Validations at **send** (draft save is permissive — half-finished drafts are fine):

| # | Rule |
|---|---|
| V1 | ≥ 1 line; every line: `quantity_milli > 0`, `unit_price_kobo ≥ 0`, `discount_bp ∈ [0, 10000]` |
| V2 | `due_date ≥ issue_date` |
| V3 | `issue_date` within open period (hard close blocks — T5; soft close confirms — P4) and ≤ 14 days in the future (fat-finger guard; advisor unrestricted) |
| V4 | customer `is_active`; free-description lines require a chosen income account |
| V5 | inventory lines: quantity available ≥ quantity sold (P7) — error names the product and shortfall in owner language |
| V6 | **currency is NGN** — v1 invoices are NGN-only (§9 decision #2); FX receipts still supported (§5.3) |
| V7 | zero-total invoices blocked (min total 1 kobo) — a ₦0 "invoice" is a delivery note, not a receivable |

Amounts (`net_kobo`, `vat_kobo`) are computed and frozen at posting per Spec 01 §6.1/§8 — the stored invoice always foots against its own PDF forever, regardless of later rate changes.

## 5. Recording Customer Payments

Entry points: **Record Payment** (global primary action) → pick customer; or from an open invoice (customer + allocation pre-filled).

### 5.1 The form

1. Customer, date, bank/cash account (Spec 02 accounts), method, reference.
2. **Amount received** — the cash that actually landed (owners copy it off the bank alert).
3. **"Did the customer deduct tax (WHT)?"** — collapsed by default. Expanded: WHT amount field + helper chips computing 5% / 10% (from `wht_rate_presets`) of the **ex-VAT** portion of the selected allocations. Manual entry always wins — customers' computations are frequently off by design or error; we record what actually happened.
4. **Allocation grid**: all open invoices (oldest `due_date` first) with balances. Auto-allocation FIFO by due date fills `amount + WHT` across invoices; every cell editable; over-allocation blocked per invoice (≤ its balance).
5. Remainder line, live: *"₦X will be kept as a deposit for this customer."*

Save calls `postPayment(direction='in')` (Spec 01 §6.2: Dr Bank + Dr WHT Receivable / Cr AR per allocation / Cr Unearned Revenue remainder), recomputes affected invoice statuses, assigns a receipt (§5.5) — one transaction.

### 5.2 Deposits without an invoice

Same form, zero allocations: the whole amount goes to Unearned Revenue against the contact. Owner-facing label: *"Customer paid ahead."* This is the planning doc's customer-deposit flow (§4.2) — first-class, not an edge case.

### 5.3 Foreign-currency receipts (NGN invoice, USD domiciliary account)

Allowed in v1. The form asks the FX amount received and the rate applied (prefilled from latest `fx_rates`); NGN equivalent = the bank line's `amount_kobo`, with `fx_currency`/`fx_amount_kobo` set. If NGN equivalent ≠ Σ allocations, the difference posts to `FX_GAIN_LOSS` as a **realized** FX line — an additive engine delta to `postPayment` (§8.3). No revaluation here; that stays an Advisor Mode journal (Spec 01 §6.5).

### 5.4 Applying deposits

Wherever a customer with a deposit balance appears — on invoice send, and inside the payment form — the app offers: *"[Customer] has ₦X paid ahead. Use it for this invoice?"* Acceptance posts the Spec 01 `deposit_application` entry (Dr Unearned Revenue / Cr AR) and recomputes status. Deposits are never auto-applied silently — the owner confirms, because the customer may have paid ahead *for something else*.

**Deposit refund** ("customer wants their money back"): new small engine function `refundDeposit()` — Dr `UNEARNED_REVENUE` (contact) / Cr Bank, guarded by available deposit balance ≥ refund (§8.3). Owner-accessible; plain-language button on the customer screen.

### 5.5 Receipts

Every inbound payment gets a sequential `RCT-` number (stored on `payments.receipt_number`, §8.2) and a receipt PDF: business identity, customer, date, amount in words + figures, allocations table ("toward INV-000123 …"), WHT acknowledged if present, deposit remainder if present, running "balance owed after this payment." Voiding a payment (§6) marks the receipt PDF VOID — the number is never reused.

## 6. Voids (user-facing workflows over Spec 01 §6.8)

- **Void payment**: reverses the payment JE; allocations stay as historical record but become inactive; affected invoice statuses recompute; receipt marked VOID. Confirm dialog states consequences in plain language.
- **Void invoice with payments attached**: blocked, with a guided path: *"₦X has already been received against this invoice. First void the payment(s), or move the money to the customer's deposit."* The second option is a one-click reallocation: void payment + repost same payment with zero allocations (money becomes deposit) — two entries, full audit trail, no deleted history.
- **Void posted invoice (no active payments)**: `voidInvoice()` — reversal JE dated today by default (advisor may choose date within open periods), inventory restored at original movement cost, status `void`. The reissue path pre-fills a new draft from the voided invoice (`converted_from_id` reused as provenance link).
- Both void actions: owner and advisor roles only (Spec 02 role matrix — `staff` cannot void).

## 7. Delivery Channels

All deliveries are logged in `document_deliveries` (§8.2) — the register shows "sent via WhatsApp, 03/07/2026".

- **PDF**: HTML template → PDF via WebView2 `PrintToPdf` in a hidden Tauri window (Windows-first; the template is plain HTML/CSS so a cross-platform renderer can swap in later). A4; naira formatting, DD/MM/YYYY; saved to `{app_data}/companies/{id}/documents/INV-000123.pdf`. Templates carry logo, TIN, bank account details for payment ("pay into"), VAT line only when registered.
- **WhatsApp** (primary channel — contacts are phone-first): normalize phone to E.164 (`0803…` → `+234803…`); open `https://wa.me/<phone>?text=<greeting + doc number + amount + due date + "PDF attached">`; the PDF is saved and its folder opened/highlighted for a drag-attach. **Honest limitation, stated up front:** wa.me cannot programmatically attach files — v1 is "message prefilled, file one drag away." True auto-attach needs the WhatsApp Business API (online, paid) — Phase 3 per planning doc §8.
- **Email**: v1 = `mailto:` with prefilled subject/body + the saved PDF path surfaced for manual attach (same honesty). Configurable SMTP send is Phase 2 (§9 decision #6). Email is tertiary for this segment; WhatsApp is the money path.

## 8. Deltas (additive) to Spec 01

### 8.1 Schema — columns
- `invoices` **+ `sent_at TEXT`**, **+ `converted_from_id TEXT REFERENCES invoices(id)`** (quote→invoice and void→reissue provenance).
- `invoices.status` CHECK gains `'converted'` (reachable by quotes only — app-enforced).
- `payments` **+ `receipt_number TEXT`** (UNIQUE per company, `direction='in'` only), **+ `voided INTEGER NOT NULL DEFAULT 0`** (formalizes the Spec 01 §6.8 payment-void flag).

### 8.2 Schema — new table
```sql
CREATE TABLE document_deliveries (
  id         TEXT PRIMARY KEY,
  company_id TEXT NOT NULL REFERENCES companies(id),
  doc_type   TEXT NOT NULL CHECK (doc_type IN ('invoice','quote','receipt')),
  doc_id     TEXT NOT NULL,
  channel    TEXT NOT NULL CHECK (channel IN ('whatsapp','email','pdf_export','print')),
  recipient  TEXT,                        -- phone or email as used
  created_at TEXT NOT NULL
);
```

### 8.3 Engine
- `postPayment(in)`: optional realized-FX line to `FX_GAIN_LOSS` when the bank line's NGN equivalent ≠ Σ allocations (FX receipts, §5.3).
- New `refundDeposit(contact, bank_account, amount)`: Dr `UNEARNED_REVENUE` / Cr Bank; precondition: contact's available deposit balance ≥ amount. Joins the Spec 01 §5.3 function list.

All deltas are additive; no existing template, trigger, or invariant changes.

## 9. Decisions needing your sign-off

1. **Posted invoices are immutable — void & reissue is the only correction path.** No "edit sent invoice," ever, including typos. This is the audit-integrity hill; the reissue flow makes it painless. (§2)
2. **NGN-only invoicing in v1.** Foreign-currency *receipts* into domiciliary accounts are supported with realized FX; foreign-currency *invoices* (USD AR, revaluation, FX on the doc itself) are deferred. **APPROVED for v1 (2026-07-03).** (§4 V6)
3. **Auto-allocation is FIFO by due date**, fully editable. Alternative (oldest invoice first by issue date) differs when terms vary; due-date FIFO matches "pay off what's most overdue." (§5.1)
4. **Deposits are never auto-applied** — always a confirm prompt. Silent application would misstate "who owes me" for owners who take deposits against future orders. (§5.4)
5. **WhatsApp/email v1 = prefilled message + PDF one drag away** (no programmatic attach — platform limitation, stated honestly rather than half-working). (§7)
6. **SMTP email send deferred to Phase 2**; v1 is mailto. (§7)
7. **Receipt numbers live on `payments`** (no separate receipts table) — a receipt is a rendering of a payment, not an entity. (§5.5, §8.1)
8. **"Move payment to deposit"** as the guided alternative when voiding a paid invoice — void + repost mechanics preserve full history. (§6)
9. ⏳ **Zero-total invoices blocked** (V7) — **PENDING: review response was ambiguous (2026-07-03); confirm at next session.** Options on the table: (a) keep the block as specified, or (b) allow zero-total invoices so free samples/promotional stock get a paper trail — in which case V7 becomes a confirm-warning, and inventory-tracked lines still post COGS at WAC (Dr COGS / Cr Inventory) with zero revenue, so the giveaway shows its true cost on the P&L. (§4)
10. **WHT helper offers preset chips but never auto-computes silently** — the recorded WHT is always what the customer actually withheld. (§5.1)

---

*End of Spec 03. Next per §11 order: Spec 04 — Expenses/bills + WHT/VAT handling, and banking/reconciliation — carrying the recorded obligation: an explicit "unclear — needs review" reconciliation state (no Suspense account exists, by design).*
