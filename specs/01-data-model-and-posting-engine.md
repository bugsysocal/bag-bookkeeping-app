# Specification 01 — Data Model & Posting Engine
**Project:** LedgerOne (placeholder) · **Covers:** Planning doc §5.2, §5.3 · **Status:** DRAFT v0.2 (structurally approved; tax rules updated to Nigeria Tax Act 2025 / WHT Regulations 2024, verified 2026-07-03)
**Stack context:** Tauri + Rust backend + local SQLite (rusqlite). All posting logic lives in Rust; the webview UI never touches the database directly.

---

## 1. Scope

This document specifies, tightly enough to generate code from:

1. Global conventions (money, quantities, IDs, dates, currency).
2. The full SQLite schema (§5.2 tables plus the support tables they imply).
3. DB-level integrity enforcement — the actual triggers that make "balanced or not at all" a database guarantee, not an application promise.
4. The system Chart of Accounts keys the posting engine depends on.
5. Every posting template: `postInvoice`, `postPayment`, `postExpense`, `postBill`, `postTransfer`, `postDrawing`, `postJournal` — with exact journal lines, VAT and WHT splits, and rounding rules.
6. Voiding/reversal semantics.

Out of scope here: UI, reports, reconciliation workflow logic (schema included; workflow specified in a later doc), Excel import/export.

---

## 2. Global Conventions

| Concern | Decision | Rationale |
|---|---|---|
| **Money** | `INTEGER` in **kobo** (minor units). Never `REAL`. Column suffix `_kobo`. | Floating point is disqualifying in a ledger. ₦92,233,720,368,547,758 max — no overflow risk. |
| **Quantities** | `INTEGER` in **milliunits** (`quantity_milli`; 2500 = 2.5). | 3dp covers trading/distribution units without floats. |
| **Rates** | Basis points, `INTEGER` (`_bp`; 750 = 7.5%). FX rates in micro-naira per unit (`_micro`; 1 USD = ₦1,500.25 → 1500250000). | Exact arithmetic everywhere. |
| **Journal line sign** | One signed `amount_kobo` per line: **positive = debit, negative = credit**. Entry invariant: `SUM(amount_kobo) = 0`. | Single-column invariant is simpler to enforce and query. Advisor Mode renders Dr/Cr from the sign. |
| **IDs** | `TEXT` ULID primary keys, generated in Rust. | Sortable, globally unique — no renumbering pain when sync/multi-company export arrives in Phase 2/3. |
| **Dates** | `TEXT` ISO-8601 (`YYYY-MM-DD`; timestamps with `Z`). UI renders DD/MM/YYYY. | SQLite-native comparisons; display is a UI concern. |
| **Base currency** | NGN per company. **Every journal line's `amount_kobo` is NGN.** Foreign-currency lines additionally carry `fx_currency` + `fx_amount_kobo`; the rate is derived, never stored as float. | The ledger always balances in one currency; FX is metadata. |
| **Deletion** | Posted journal entries and their lines are immutable (trigger-enforced). Nothing financial is ever deleted — void = reversal entry. `audit_log` is append-only (trigger-enforced). | Planning doc principle 3. |
| **Enums** | `TEXT` + `CHECK (col IN (...))`. | Readable dumps, cheap enforcement. |

All monetary computation happens in Rust in integer kobo; rounding is **half-away-from-zero**, applied at the points named in §8 and nowhere else.

---

## 3. Schema

`PRAGMA foreign_keys = ON;` `PRAGMA journal_mode = WAL;` — set at every connection open.

### 3.1 Companies, users, settings

```sql
CREATE TABLE companies (
  id                TEXT PRIMARY KEY,
  name              TEXT NOT NULL,
  base_currency     TEXT NOT NULL DEFAULT 'NGN',
  tin               TEXT,                                -- company's own Tax Identification Number
  vat_registered    INTEGER NOT NULL DEFAULT 1,          -- operational flag: engine charges/claims VAT iff 1
  -- Two INDEPENDENT statutory reliefs (NTA 2025) — they do not move together; a ₦70M-turnover
  -- company charges VAT yet is CIT-exempt. Both derived by the setup wizard from turnover band,
  -- fixed-assets band, and a professional-services question (the exclusion applies to BOTH reliefs);
  -- both revisited annually. Advisor-editable.
  vat_exempt        INTEGER NOT NULL DEFAULT 0,          -- small business: turnover ≤ ₦50M ⇒ VAT-exempt
                                                         -- (registration mandatory above ₦50M). Informs the
                                                         -- wizard's default for vat_registered
  cit_exempt        INTEGER NOT NULL DEFAULT 0,          -- small company: turnover ≤ ₦100M AND fixed assets
                                                         -- ≤ ₦250M ⇒ exempt from CIT/CGT/Development Levy.
                                                         -- Gates the WHT deduction exemption (§6.2)
  vat_rate_bp       INTEGER NOT NULL DEFAULT 750,        -- 7.5%; configurable, not hardcoded
  inventory_enabled INTEGER NOT NULL DEFAULT 0,          -- opt-in at setup (§4.7)
  soft_close_through TEXT,                               -- owner-tier: warn on edits ≤ this date
  hard_close_through TEXT,                               -- advisor-tier: trigger-blocked ≤ this date
  created_at        TEXT NOT NULL
);

CREATE TABLE users (                                     -- resolves open decision #2: two roles from day one
  id         TEXT PRIMARY KEY,
  company_id TEXT NOT NULL REFERENCES companies(id),
  name       TEXT NOT NULL,
  role       TEXT NOT NULL CHECK (role IN ('owner','staff','advisor')),
  pin_hash   TEXT,                                       -- advisor mode PIN (argon2)
  created_at TEXT NOT NULL
);
```

### 3.2 Chart of accounts

```sql
CREATE TABLE accounts (
  id         TEXT PRIMARY KEY,
  company_id TEXT NOT NULL REFERENCES companies(id),
  code       TEXT NOT NULL,                              -- '1010', '6200', ...
  name       TEXT NOT NULL,
  class      TEXT NOT NULL CHECK (class IN
             ('asset','liability','equity','income','cogs','expense')),
  system_key TEXT,                                       -- engine lookup handle; see §5. NULL for user accounts
  is_bank    INTEGER NOT NULL DEFAULT 0,                 -- 1 ⇒ has a bank_accounts row
  is_system  INTEGER NOT NULL DEFAULT 0,                 -- locked: no rename of class, no deactivation
  is_active  INTEGER NOT NULL DEFAULT 1,
  UNIQUE (company_id, code),
  UNIQUE (company_id, system_key)
);
```

Users may add/rename accounts **within class constraints** (rename = name only; class and system_key immutable after creation — app-enforced, plus a trigger blocking class changes). Normal balance is derived from `class`, never stored.

### 3.3 Contacts & products

```sql
CREATE TABLE contacts (
  id                 TEXT PRIMARY KEY,
  company_id         TEXT NOT NULL REFERENCES companies(id),
  kind               TEXT NOT NULL CHECK (kind IN ('customer','supplier','both')),
  name               TEXT NOT NULL,
  phone              TEXT,                               -- primary channel (WhatsApp)
  email              TEXT,
  tin                TEXT,                               -- supplier TIN gates the WHT deduction exemption (§6.2);
                                                         -- no TIN ⇒ WHT Regs 2024 prescribe deduction at 2× rate
  address            TEXT,
  payment_terms_days INTEGER NOT NULL DEFAULT 0,         -- 0 = due on receipt
  is_active          INTEGER NOT NULL DEFAULT 1,
  created_at         TEXT NOT NULL
);
-- Opening balances are NOT columns: the setup wizard posts them as journal entries
-- (Dr AR / Cr Opening Balance Equity, etc.) so the trial balance is correct from day one.

CREATE TABLE products (
  id                 TEXT PRIMARY KEY,
  company_id         TEXT NOT NULL REFERENCES companies(id),
  kind               TEXT NOT NULL CHECK (kind IN ('product','service')),
  name               TEXT NOT NULL,
  sku                TEXT,
  sale_price_kobo    INTEGER NOT NULL DEFAULT 0,
  is_vatable         INTEGER NOT NULL DEFAULT 1,         -- default VAT toggle per line
  track_inventory    INTEGER NOT NULL DEFAULT 0,         -- only meaningful if company.inventory_enabled
  income_account_id  TEXT REFERENCES accounts(id),       -- default 4000 Sales
  cogs_account_id    TEXT REFERENCES accounts(id),       -- default 5000 COGS
  is_active          INTEGER NOT NULL DEFAULT 1
);
```

### 3.4 Documents: invoices & bills

```sql
CREATE TABLE document_sequences (                        -- sequential, gap-free numbering (§4.2)
  company_id  TEXT NOT NULL REFERENCES companies(id),
  doc_type    TEXT NOT NULL CHECK (doc_type IN ('invoice','quote','receipt')),
  prefix      TEXT NOT NULL,                             -- 'INV-', 'QUO-', 'RCT-'
  next_number INTEGER NOT NULL DEFAULT 1,
  PRIMARY KEY (company_id, doc_type)
);
-- Incremented inside the same DB transaction that creates the document ⇒ gap-free by construction.
-- Draft invoices already consume a number (visible as Draft in the register), so voided drafts
-- never create an unexplained numbering gap.

CREATE TABLE invoices (
  id               TEXT PRIMARY KEY,
  company_id       TEXT NOT NULL REFERENCES companies(id),
  contact_id       TEXT NOT NULL REFERENCES contacts(id),
  number           TEXT NOT NULL,                        -- 'INV-000123', immutable
  kind             TEXT NOT NULL DEFAULT 'invoice' CHECK (kind IN ('invoice','quote')),
  status           TEXT NOT NULL DEFAULT 'draft' CHECK (status IN
                   ('draft','sent','partially_paid','paid','void')),
                   -- 'overdue' is DERIVED (status IN sent/partially_paid AND due_date < today), never stored
  issue_date       TEXT NOT NULL,
  due_date         TEXT NOT NULL,
  currency         TEXT NOT NULL DEFAULT 'NGN',
  subtotal_kobo    INTEGER NOT NULL,                     -- Σ line nets, after line discounts, ex-VAT
  vat_kobo         INTEGER NOT NULL,
  total_kobo       INTEGER NOT NULL,                     -- subtotal + vat
  amount_paid_kobo INTEGER NOT NULL DEFAULT 0,           -- cache; source of truth = payment_allocations
  notes            TEXT,
  journal_entry_id TEXT REFERENCES journal_entries(id),  -- NULL while draft/quote
  voided_by_entry  TEXT REFERENCES journal_entries(id),
  created_by       TEXT REFERENCES users(id),
  created_at       TEXT NOT NULL,
  UNIQUE (company_id, number)
);

CREATE TABLE invoice_lines (
  id              TEXT PRIMARY KEY,
  invoice_id      TEXT NOT NULL REFERENCES invoices(id),
  line_no         INTEGER NOT NULL,
  product_id      TEXT REFERENCES products(id),          -- NULL = free-description line
  description     TEXT NOT NULL,
  quantity_milli  INTEGER NOT NULL,
  unit_price_kobo INTEGER NOT NULL,
  discount_bp     INTEGER NOT NULL DEFAULT 0,            -- per-line % discount
  vat_applied     INTEGER NOT NULL,                      -- per-line VAT toggle (§4.2)
  net_kobo        INTEGER NOT NULL,                      -- computed once at posting; immutable after
  vat_kobo        INTEGER NOT NULL,
  income_account_id TEXT NOT NULL REFERENCES accounts(id)
);

CREATE TABLE bills (
  id               TEXT PRIMARY KEY,
  company_id       TEXT NOT NULL REFERENCES companies(id),
  contact_id       TEXT NOT NULL REFERENCES contacts(id),
  reference        TEXT,                                 -- supplier's own invoice number
  status           TEXT NOT NULL DEFAULT 'draft' CHECK (status IN
                   ('draft','open','partially_paid','paid','void')),
  bill_date        TEXT NOT NULL,
  due_date         TEXT NOT NULL,
  currency         TEXT NOT NULL DEFAULT 'NGN',
  subtotal_kobo    INTEGER NOT NULL,
  vat_kobo         INTEGER NOT NULL,                     -- claimable input VAT only (see §6.4)
  total_kobo       INTEGER NOT NULL,
  wht_applicable   INTEGER NOT NULL DEFAULT 0,           -- §4.3: WHT split happens at PAYMENT (§6.2)
  wht_rate_bp      INTEGER,                              -- e.g. 500 = 5% services, 1000 = 10% rent
  amount_paid_kobo INTEGER NOT NULL DEFAULT 0,
  journal_entry_id TEXT REFERENCES journal_entries(id),
  voided_by_entry  TEXT REFERENCES journal_entries(id),
  created_by       TEXT REFERENCES users(id),
  created_at       TEXT NOT NULL
);

CREATE TABLE bill_lines (
  id                 TEXT PRIMARY KEY,
  bill_id            TEXT NOT NULL REFERENCES bills(id),
  line_no            INTEGER NOT NULL,
  product_id         TEXT REFERENCES products(id),
  description        TEXT NOT NULL,
  quantity_milli     INTEGER NOT NULL DEFAULT 1000,
  unit_cost_kobo     INTEGER NOT NULL,
  vat_claimable      INTEGER NOT NULL DEFAULT 1,         -- NTA 2025 input-VAT rule; see §6.4
  net_kobo           INTEGER NOT NULL,
  vat_kobo           INTEGER NOT NULL,
  expense_account_id TEXT NOT NULL REFERENCES accounts(id) -- expense, inventory, or fixed-asset account
);

CREATE TABLE wht_rate_presets (                          -- seeded, user-editable; rates are DATA not code
  id      TEXT PRIMARY KEY,
  company_id TEXT NOT NULL REFERENCES companies(id),
  label   TEXT NOT NULL,                                 -- 'Professional/consultancy services', 'Rent', ...
  rate_bp INTEGER NOT NULL
);
```

### 3.5 Payments & allocations

One table, both directions — a payment may settle several invoices or bills (partial payments, §5.2).

```sql
CREATE TABLE payments (
  id               TEXT PRIMARY KEY,
  company_id       TEXT NOT NULL REFERENCES companies(id),
  direction        TEXT NOT NULL CHECK (direction IN ('in','out')),
  contact_id       TEXT REFERENCES contacts(id),
  bank_account_id  TEXT NOT NULL REFERENCES bank_accounts(id),
  payment_date     TEXT NOT NULL,
  amount_kobo      INTEGER NOT NULL CHECK (amount_kobo > 0),  -- cash that actually moved
  wht_kobo         INTEGER NOT NULL DEFAULT 0,           -- in: withheld BY customer; out: withheld FROM supplier
  method           TEXT CHECK (method IN ('transfer','cash','pos','cheque','other')),
  reference        TEXT,
  journal_entry_id TEXT NOT NULL REFERENCES journal_entries(id),
  created_by       TEXT REFERENCES users(id),
  created_at       TEXT NOT NULL
);

CREATE TABLE payment_allocations (
  id          TEXT PRIMARY KEY,
  payment_id  TEXT NOT NULL REFERENCES payments(id),
  target_type TEXT NOT NULL CHECK (target_type IN ('invoice','bill')),
  target_id   TEXT NOT NULL,
  amount_kobo INTEGER NOT NULL CHECK (amount_kobo > 0)   -- gross amount of the document being settled
);
-- Rule: Σ allocations ≤ amount_kobo + wht_kobo. Unallocated remainder on an inbound
-- payment becomes a customer deposit (Unearned Revenue) — see postPayment §6.2.
```

### 3.6 The ledger (heart of the schema)

```sql
CREATE TABLE journal_entries (
  id                TEXT PRIMARY KEY,
  company_id        TEXT NOT NULL REFERENCES companies(id),
  entry_date        TEXT NOT NULL,
  memo              TEXT NOT NULL,                       -- mandatory for ALL entries, not just manual
  source_type       TEXT NOT NULL CHECK (source_type IN
                    ('invoice','payment','expense','bill','transfer','drawing','capital',
                     'manual','reversal','opening_balance','inventory_adjustment','deposit_application')),
  source_id         TEXT,                                -- FK to the originating document (polymorphic)
  is_posted         INTEGER NOT NULL DEFAULT 0,          -- flipping 0→1 fires the balance trigger (§4)
  reverses_entry_id TEXT REFERENCES journal_entries(id), -- set on reversal entries
  reversed_by_entry_id TEXT REFERENCES journal_entries(id),
  created_by        TEXT REFERENCES users(id),
  created_at        TEXT NOT NULL,
  posted_at         TEXT
);

CREATE TABLE journal_lines (
  id            TEXT PRIMARY KEY,
  entry_id      TEXT NOT NULL REFERENCES journal_entries(id),
  line_no       INTEGER NOT NULL,
  account_id    TEXT NOT NULL REFERENCES accounts(id),
  amount_kobo   INTEGER NOT NULL CHECK (amount_kobo != 0),  -- +debit / −credit, ALWAYS NGN
  contact_id    TEXT REFERENCES contacts(id),            -- REQUIRED on AR/AP lines (subledger dimension)
  memo          TEXT,
  fx_currency   TEXT,                                    -- set only on foreign-currency bank lines
  fx_amount_kobo INTEGER                                 -- amount in fx_currency minor units; rate derived
);
CREATE INDEX idx_jl_account ON journal_lines(account_id);
CREATE INDEX idx_jl_entry   ON journal_lines(entry_id);
CREATE INDEX idx_jl_contact ON journal_lines(contact_id) WHERE contact_id IS NOT NULL;
CREATE INDEX idx_je_date    ON journal_entries(company_id, entry_date);
```

Account balances, AR/AP aging, trial balance, and all statements are **queries over `journal_lines`** — never cached columns (except the two `amount_paid_kobo` display caches, which are recomputable).

### 3.7 Banking & reconciliation

```sql
CREATE TABLE bank_accounts (
  id                   TEXT PRIMARY KEY,
  company_id           TEXT NOT NULL REFERENCES companies(id),
  account_id           TEXT NOT NULL UNIQUE REFERENCES accounts(id),  -- 1:1 with a COA asset account
  label                TEXT NOT NULL,                    -- 'GTBank Current', 'POS Wallet', 'Petty Cash'
  kind                 TEXT NOT NULL CHECK (kind IN ('bank','cash','pos_wallet','domiciliary')),
  currency             TEXT NOT NULL DEFAULT 'NGN',
  bank_name            TEXT,
  account_number_last4 TEXT,
  last_reconciled_date TEXT,
  is_active            INTEGER NOT NULL DEFAULT 1
);
-- Opening balance: posted by the setup wizard as a journal entry
-- (Dr Bank / Cr 3000 Opening Balance Equity), not stored as a column.

CREATE TABLE fx_rates (                                  -- manual or fetched reference rates (§4.4)
  id         TEXT PRIMARY KEY,
  company_id TEXT NOT NULL REFERENCES companies(id),
  currency   TEXT NOT NULL,
  rate_micro INTEGER NOT NULL,                           -- ₦ per 1 unit, micro precision
  rate_date  TEXT NOT NULL,
  source     TEXT NOT NULL CHECK (source IN ('manual','fetched')),
  UNIQUE (company_id, currency, rate_date)
);

CREATE TABLE reconciliations (
  id                     TEXT PRIMARY KEY,
  company_id             TEXT NOT NULL REFERENCES companies(id),
  bank_account_id        TEXT NOT NULL REFERENCES bank_accounts(id),
  statement_date         TEXT NOT NULL,
  statement_balance_kobo INTEGER NOT NULL,
  status                 TEXT NOT NULL DEFAULT 'in_progress'
                         CHECK (status IN ('in_progress','completed')),
  completed_at           TEXT,
  created_at             TEXT NOT NULL
);

CREATE TABLE reconciliation_lines (                      -- one row per imported statement line
  id                TEXT PRIMARY KEY,
  reconciliation_id TEXT NOT NULL REFERENCES reconciliations(id),
  stmt_date         TEXT NOT NULL,
  stmt_description  TEXT,
  stmt_amount_kobo  INTEGER NOT NULL,                    -- signed: + credit to bank, − debit
  matched_line_id   TEXT REFERENCES journal_lines(id),   -- NULL = unmatched (prompts entry creation)
  match_kind        TEXT CHECK (match_kind IN ('auto','manual','created'))
);
-- Reconciliation lock: completing a reconciliation stamps bank_accounts.last_reconciled_date;
-- posting-engine precondition P6 (§6.0) rejects new/reversal entries touching that bank
-- account dated ≤ that date, except through the reconciliation module itself.
```

### 3.8 Inventory & audit

```sql
CREATE TABLE inventory_movements (                       -- only written when company.inventory_enabled
  id               TEXT PRIMARY KEY,
  company_id       TEXT NOT NULL REFERENCES companies(id),
  product_id       TEXT NOT NULL REFERENCES products(id),
  movement_date    TEXT NOT NULL,
  kind             TEXT NOT NULL CHECK (kind IN ('opening','purchase','sale','adjustment','reversal')),
  quantity_milli   INTEGER NOT NULL,                     -- signed: + in, − out
  unit_cost_kobo   INTEGER NOT NULL,                     -- WAC at time of movement for outflows
  total_cost_kobo  INTEGER NOT NULL,
  journal_entry_id TEXT REFERENCES journal_entries(id),
  created_at       TEXT NOT NULL
);
-- Weighted-average cost: on inflow, WAC = (qty_on_hand×WAC + qty_in×unit_cost) / (qty_on_hand+qty_in),
-- computed in integer kobo, rounded half-away-from-zero. Outflows post at current WAC.
-- Negative stock is rejected at posting time (precondition P7).

CREATE TABLE audit_log (
  id          TEXT PRIMARY KEY,
  company_id  TEXT NOT NULL,
  user_id     TEXT,
  action      TEXT NOT NULL,                             -- 'invoice.posted', 'entry.reversed', ...
  entity_type TEXT NOT NULL,
  entity_id   TEXT NOT NULL,
  detail_json TEXT,
  created_at  TEXT NOT NULL
);
```

---

## 4. DB-Level Integrity Enforcement (the triggers)

These make the core promises structural. Posting protocol: within **one transaction** (`BEGIN IMMEDIATE`), insert the entry with `is_posted = 0`, insert all lines, then `UPDATE journal_entries SET is_posted = 1, posted_at = ... WHERE id = ?`. That update fires validation; any `RAISE(ABORT)` rolls back the whole event. **An entry commits balanced or it does not commit.**

```sql
-- T1: entries must balance and have ≥ 2 lines to post
CREATE TRIGGER trg_je_balance BEFORE UPDATE OF is_posted ON journal_entries
WHEN NEW.is_posted = 1 AND OLD.is_posted = 0
BEGIN
  SELECT RAISE(ABORT, 'journal entry does not balance')
  WHERE (SELECT COALESCE(SUM(amount_kobo), 0) FROM journal_lines WHERE entry_id = NEW.id) != 0;
  SELECT RAISE(ABORT, 'journal entry needs at least two lines')
  WHERE (SELECT COUNT(*) FROM journal_lines WHERE entry_id = NEW.id) < 2;
END;

-- T2/T3: posted lines are immutable
CREATE TRIGGER trg_jl_no_update BEFORE UPDATE ON journal_lines
WHEN (SELECT is_posted FROM journal_entries WHERE id = OLD.entry_id) = 1
BEGIN SELECT RAISE(ABORT, 'posted journal lines are immutable'); END;

CREATE TRIGGER trg_jl_no_delete BEFORE DELETE ON journal_lines
WHEN (SELECT is_posted FROM journal_entries WHERE id = OLD.entry_id) = 1
BEGIN SELECT RAISE(ABORT, 'posted journal lines cannot be deleted'); END;

-- T4: journal entries are NEVER deleted (unposted drafts are cleaned by app-level GC, via a flag)
CREATE TRIGGER trg_je_no_delete BEFORE DELETE ON journal_entries
WHEN OLD.is_posted = 1
BEGIN SELECT RAISE(ABORT, 'posted entries cannot be deleted; post a reversal'); END;

-- T5: hard close — no posting into a hard-closed period
CREATE TRIGGER trg_je_hard_close BEFORE UPDATE OF is_posted ON journal_entries
WHEN NEW.is_posted = 1 AND NEW.entry_date <=
     (SELECT COALESCE(hard_close_through, '0000-00-00') FROM companies WHERE id = NEW.company_id)
BEGIN SELECT RAISE(ABORT, 'period is hard-closed'); END;

-- T6/T7: audit log is append-only
CREATE TRIGGER trg_audit_no_update BEFORE UPDATE ON audit_log
BEGIN SELECT RAISE(ABORT, 'audit log is append-only'); END;
CREATE TRIGGER trg_audit_no_delete BEFORE DELETE ON audit_log
BEGIN SELECT RAISE(ABORT, 'audit log is append-only'); END;

-- T8: account class is immutable; system accounts cannot be deactivated
CREATE TRIGGER trg_acct_lock BEFORE UPDATE ON accounts
BEGIN
  SELECT RAISE(ABORT, 'account class is immutable') WHERE NEW.class != OLD.class;
  SELECT RAISE(ABORT, 'system accounts cannot be deactivated or re-keyed')
  WHERE OLD.is_system = 1 AND (NEW.is_active = 0 OR NEW.system_key IS NOT OLD.system_key);
END;
```

Soft close (`soft_close_through`) is a warning surfaced by the app layer, not a trigger — per §4.5 of the planning doc, owners get friction, advisors get the hard lock.

---

## 5. System Accounts (engine dependencies)

The posting engine resolves accounts by `system_key`, never by code — codes are display/user-space. Seeded per company, `is_system = 1`:

| system_key | Code | Name | Class |
|---|---|---|---|
| `AR` | 1100 | Accounts Receivable | asset |
| `INVENTORY` | 1200 | Inventory | asset |
| `VAT_INPUT` | 1310 | VAT Receivable (Input) | asset |
| `WHT_RECEIVABLE` | 1320 | WHT Receivable (credit notes) | asset |
| `AP` | 2100 | Accounts Payable | liability |
| `VAT_OUTPUT` | 2210 | VAT Payable (Output) | liability |
| `WHT_PAYABLE` | 2220 | WHT Payable | liability |
| `UNEARNED_REVENUE` | 2300 | Customer Deposits (Unearned Revenue) | liability |
| `OPENING_BALANCE_EQUITY` | 3000 | Opening Balance Equity | equity |
| `OWNER_CAPITAL` | 3100 | Owner's Capital | equity |
| `OWNER_DRAWINGS` | 3200 | Owner's Drawings | equity |
| `RETAINED_EARNINGS` | 3900 | Retained Earnings | equity |
| `SALES_DEFAULT` | 4000 | Sales Revenue | income |
| `FX_GAIN_LOSS` | 4900 | FX Gain/Loss | income |
| `COGS_DEFAULT` | 5000 | Cost of Goods Sold | cogs |
| `BANK_CHARGES` | 6900 | Bank Charges | expense |
| `ROUNDING` | 6990 | Rounding Differences | expense |

(The full COA seed — 6200 Utilities/Power and the rest of the OpEx tree — belongs to Spec 02 and doesn't gate this document; the engine only needs the keys above.)

---

## 6. Posting Engine

### 6.0 Universal rules (apply to every function)

- **P1 — One transaction.** Each posting function is exactly one `BEGIN IMMEDIATE … COMMIT`. Document row(s), journal entry, lines, sequence increments, inventory movements, and the audit_log row all commit together or not at all.
- **P2 — UI never writes the ledger.** Tauri commands map 1:1 to these functions; there is no other write path to `journal_entries`/`journal_lines`.
- **P3 — Validation before SQL.** Amounts > 0, dates parseable and not absurd (±10y guard), accounts active and of the expected class, contact present on AR/AP lines. Trigger T1 is the backstop, not the first line of defense.
- **P4 — Soft-close warning.** If `entry_date ≤ soft_close_through`, the function returns a "requires confirmation" result once; the confirmed retry proceeds (and is audit-logged as such).
- **P5 — Every entry gets a human-readable memo**, auto-composed ("Invoice INV-000123 — Chidinma Stores") — the advisor should be able to read the raw journal.
- **P6 — Reconciliation lock.** Entries with a line on a bank account dated ≤ that account's `last_reconciled_date` are rejected (except entries created by the reconciliation module).
- **P7 — No negative stock.** An outflow that would drive quantity-on-hand below zero is rejected with a friendly error.
- **P8 — AR/AP lines always carry `contact_id`** — this is what makes "who owes me" a query, not a report module.

Notation below: **Dr** positive / **Cr** negative `amount_kobo`; all NGN.

### 6.1 `postInvoice(invoice_id)` — Draft → Sent

**Draft invoices and quotes have no journal entry.** Posting fires on the Draft→Sent transition (or quote→invoice conversion then send).

Per line: `net = round(qty_milli × unit_price_kobo / 1000 × (10000 − discount_bp) / 10000)`; `vat = vat_applied ? round(net × vat_rate_bp / 10000) : 0` (skipped entirely if `vat_registered = 0`).

| Line | Account | Amount | Notes |
|---|---|---|---|
| Dr | `AR` | total (net + VAT) | carries `contact_id` |
| Cr | line's `income_account_id` | net | one Cr per distinct income account |
| Cr | `VAT_OUTPUT` | Σ line VAT | omitted if 0 |

**If inventory enabled**, for each `track_inventory` line, appended to the *same entry*:

| Line | Account | Amount |
|---|---|---|
| Dr | product's `cogs_account_id` | qty × current WAC |
| Cr | `INVENTORY` | same |

plus an `inventory_movements` row (kind `sale`, negative qty) per line. Side effects: status → `sent`, `journal_entry_id` set, audit row.

### 6.2 `postPayment(payment)` — both directions

**Direction `in` (customer receipt).** Inputs: bank account, cash received, allocations to invoices (gross amounts), optional `wht_kobo` withheld by the customer (Nigerian B2B reality: customer remits WHT to FIRS and pays you net; you hold a tax credit).

| Line | Account | Amount | Notes |
|---|---|---|---|
| Dr | Bank account's COA account | cash received | fx fields set if domiciliary |
| Dr | `WHT_RECEIVABLE` | `wht_kobo` | omitted if 0 |
| Cr | `AR` | Σ allocations | with `contact_id` |
| Cr | `UNEARNED_REVENUE` | unallocated remainder | customer deposit (§4.2); `contact_id` set |

Invariant: cash + WHT = Σ allocations + deposit remainder. Invoice statuses and `amount_paid_kobo` caches update in the same transaction; a receipt document (RCT sequence) is generated.

**Direction `out` (supplier payment against bills).** WHT is recognized **at payment**, matching Nigerian practice (deduct when you pay, remit to FIRS): for each allocated bill flagged `wht_applicable`, `wht = round(allocated_net_portion × wht_rate_bp / 10000)` — WHT applies to the ex-VAT amount.

| Line | Account | Amount | Notes |
|---|---|---|---|
| Dr | `AP` | Σ allocations (gross) | with `contact_id` |
| Cr | Bank account | cash paid = allocations − WHT | |
| Cr | `WHT_PAYABLE` | Σ WHT withheld | omitted if 0 |

**Small-company WHT deduction exemption (WHT Regulations 2024, effective Jan 2025).** If `companies.cit_exempt = 1` — WHT is an income-tax collection mechanism, so the Regulations' "small company" concept is gated on the CIT small-company flag, not the ₦50M VAT threshold; the flag is advisor-editable if FIRS guidance says otherwise — the payer is exempt from deducting WHT on a payment when **both** hold:
1. the supplier has a valid `contacts.tin`, and
2. cumulative payments to that supplier **within the calendar month** (this payment included) do not exceed ₦2,000,000.

The engine evaluates this at payment time (it's a month-aggregate check, so it cannot be precomputed on the bill) and defaults the WHT split **off** when the exemption applies, showing the owner a plain-language note ("No tax deduction needed — supplier has a TIN and this month's payments are under ₦2M"). Advisor-overridable in both directions. Conversely, if a WHT-applicable payment is being made to a supplier with **no TIN**, the Regulations prescribe deduction at **twice** the prescribed rate — the engine surfaces this as a warning and offers the doubled rate as the default rather than silently applying it (the owner may instead obtain the supplier's TIN and re-enter).

**Deposit application** (applying a held customer deposit to a new invoice) is its own small entry, `source_type = 'deposit_application'`: **Dr `UNEARNED_REVENUE` / Cr `AR`**, both with `contact_id`.

### 6.3 `postExpense(expense)` — immediate cash expense (no AP)

Quick-entry form: date, payee (free text or contact), category (COA-mapped expense account), amount paid, bank account, optional attachment. **The amount the user enters is the cash that left the bank** — owners type what they paid.

| Line | Account | Amount | Notes |
|---|---|---|---|
| Dr | chosen expense account | net (or full amount) | see VAT note below |
| Dr | `VAT_INPUT` | backed-out VAT | only if VAT toggle on and `vat_registered` |
| Cr | Bank account | amount paid | |

VAT on quick expenses: the form offers a "price includes VAT" toggle (default from the category). If on and the company is VAT-registered, the engine backs the VAT out of the entered amount (`vat = round(paid × rate_bp / (10000 + rate_bp))`) and posts it to `VAT_INPUT` per the NTA 2025 rule (§6.4); otherwise the full amount lands on the expense account.

If the payee required a WHT deduction (rare on quick expenses, but supported — e.g. paying a contractor cash): user enters gross; **Dr expense (gross) / Cr bank (net) / Cr `WHT_PAYABLE` (withheld)**.

Implementation note: an expense is a document row in a lightweight `expenses` table? **No** — decision: cash expenses are modeled as a `bills` row with `status='paid'` created and settled in one transaction (bill entry + payment entry), keeping payee history, VAT treatment, and reporting on a single code path. The quick-entry form is UI sugar over `postBill` + `postPayment`. *(Flagged for review — the alternative is a standalone expenses table with its own single combined entry: one less join, but two code paths for VAT/WHT. I recommend the single path.)*

### 6.4 `postBill(bill_id)` — supplier bill (creates AP)

**Nigerian input-VAT rule, encoded (NTA 2025 §155(4), effective 1 Jan 2026):** input VAT incurred on **any** taxable supply — goods, services, and fixed assets — is recoverable against output VAT, to the extent it is attributable to the making of taxable supplies. Hence `vat_claimable` per bill line **defaults to 1** whenever `vat_registered = 1`, regardless of the line's account class. The advisor can override to 0 per line for inputs attributable to exempt supplies (mixed-supply apportionment is out of scope for v1 — the per-line override is the mechanism). If `vat_registered = 0` (which is the wizard default when `vat_exempt = 1`), no VAT is ever claimable and vendor-charged VAT is absorbed into the line cost. Note for capital purchases: where VAT due on an asset was not charged, the expenditure doesn't qualify as eligible capital expenditure under the NTA — a Spec 06 (reports) concern, but the data to detect it lives here.

| Line | Account | Amount | Notes |
|---|---|---|---|
| Dr | line's `expense_account_id` | net (+ VAT if not claimable) | one Dr per line |
| Dr | `VAT_INPUT` | Σ claimable VAT | omitted if 0 |
| Cr | `AP` | bill total | with `contact_id` |

If inventory enabled and a line targets `INVENTORY`: `inventory_movements` row (kind `purchase`, positive qty, unit cost = net/qty) and WAC recalculation, same transaction. WHT: **nothing at bill time** — the flag and rate ride on the bill; the split happens in `postPayment` (§6.2).

### 6.5 `postTransfer(transfer)` — inter-account movement

Never income, never expense (§4.4).

Same currency:

| Line | Account | Amount |
|---|---|---|
| Dr | destination bank | amount |
| Cr | source bank | amount |
| Dr | `BANK_CHARGES` | fee (optional; Cr source bank increases accordingly) |

Cross-currency (e.g. NGN → USD domiciliary): user enters both legs — amount out (NGN) and amount received (USD) plus the applied rate. NGN values must balance; both legs' NGN equivalents are computed from the actual money that moved, so **no FX gain/loss arises on the transfer itself** (unrealized revaluation is a separate Advisor Mode journal against `FX_GAIN_LOSS`, out of scope here). The USD line carries `fx_currency='USD'`, `fx_amount_kobo` = cents received.

### 6.6 `postDrawing(drawing)` — owner took money out (or put money in)

The guilt-free button. `direction`:

| direction | Lines |
|---|---|
| `out` ("Owner took money out") | Dr `OWNER_DRAWINGS` / Cr bank account |
| `in` ("Owner put money in") | Dr bank account / Cr `OWNER_CAPITAL` (`source_type='capital'`) |

No contact, no VAT, no WHT, ever. Plain-language memo auto-set.

### 6.7 `postJournal(entry)` — Advisor Mode only

Arbitrary lines. Extra rules beyond P1–P8: caller must hold advisor role (PIN-verified); memo is mandatory and user-written (not auto-composed); lines on `AR`/`AP` must carry `contact_id`; posting to bank-linked accounts is allowed but P6 applies. This is also the mechanism for opening balances (`source_type='opening_balance'`, counter-account `OPENING_BALANCE_EQUITY`) and manual depreciation (§8 of the planning doc).

### 6.8 `voidEntry(entry_id)` / document voids

Nothing is deleted. `voidEntry` creates a new entry, `source_type='reversal'`, same-account **negated** lines, `reverses_entry_id` set, cross-linked via `reversed_by_entry_id`, dated **today** by default (advisor may back-date within open periods — voiding into a closed period is exactly what period locks exist to stop).

Document-level voids orchestrate: `voidInvoice` reverses the invoice entry **and** its COGS/inventory effect (inventory_movements `reversal` row restores stock at the original movement's cost, not current WAC); blocked if payments are allocated (unallocate first — the UI walks the user through it). `voidBill` blocked likewise while payments exist. Payments are voided by reversing the payment entry and deleting no allocation rows — allocations are marked void via the payment's reversal (allocation rows get a `voided` mirror? **No** — decision: `payment_allocations` rows are hard-deleted only while their payment's entry is unposted; voiding a posted payment reverses the entry and flags the payment row `voided` — allocations remain as historical record, excluded from balance queries by the reversal's ledger effect).

---

## 7. Invariants the statements inherit

Because every event flows through §6:

1. Trial balance sums to zero at every instant (T1, by induction).
2. Balance sheet balances by construction (planning doc §4.6.6).
3. AR on the balance sheet ≡ Σ open invoice balances ≡ "Who owes me" (P8 + single AR account + contact dimension). Same for AP.
4. Bank balances per ledger ≡ Σ journal lines on that account — reconciliation compares this against the statement, never against a cached number.
5. VAT report = activity on `VAT_OUTPUT` minus `VAT_INPUT` for the period; WHT schedule = activity on `WHT_PAYABLE` (to remit) and `WHT_RECEIVABLE` (credits held).

## 8. Rounding policy (complete list of rounding points)

1. Invoice/bill line net: once per line, half-away-from-zero, at posting.
2. Line VAT: once per line, on the rounded net. Totals are sums of rounded lines — the printed invoice always foots.
3. WHT: once per payment per bill, on the ex-VAT allocated portion.
4. WAC unit cost: once per inflow recalculation.
5. Cross-currency legs: NGN amounts are entered/known, not computed — no rounding. Any residual kobo in edge cases (e.g. deposit application splits) goes to `ROUNDING` (6990), which an advisor reviews; it should stay at a few kobo per month or something is wrong.

## 9. Decisions needing your sign-off

1. **Signed single-amount journal lines** (+Dr/−Cr) rather than separate debit/credit columns — display renders Dr/Cr. (§2)
2. **Cash expenses ride the bills pipeline** (auto-paid bill) rather than a separate expenses table — one code path for VAT/WHT. (§6.3)
3. **WHT recognized at payment time**, not bill time. (§6.2/6.4)
4. ~~Input VAT default-claimable only on inventory/COGS lines~~ **RESOLVED 2026-07-03:** per NTA 2025 §155(4), input VAT defaults to claimable on **all** lines when VAT-registered (goods, services, fixed assets, attributable to taxable supplies); advisor override per line remains. (§6.4)
5. **Draft invoices consume sequence numbers** (gap-free ledger, but voided drafts show in the register rather than leaving silent gaps). (§3.4)
6. **Opening balances as journal entries** against 3000 Opening Balance Equity, not metadata columns. (§3.3/3.7)
7. **ULID text primary keys** for sync-friendliness later, at a small size cost now. (§2)
8. **Quantities in milliunits (3dp)** — sufficient for trading/distribution? (§2)
9. ~~Verify current FIRS parameters~~ **RESOLVED 2026-07-03** (web-verified against NTA 2025 / WHT Regs 2024 commentary; two-threshold split corrected per accountant review):
   - **VAT** (`vat_exempt`): registration mandatory above **₦50M annual turnover** (raised from ₦25M — the planning doc's figure is outdated); small businesses ≤ ₦50M are VAT-exempt.
   - **CIT/CGT/Development Levy** (`cit_exempt`): separate, higher threshold — turnover ≤ **₦100M and** total fixed assets ≤ **₦250M**. The two reliefs are independent: a company can be VAT-liable yet CIT-exempt.
   - **Professional-services firms are excluded from both reliefs** regardless of size.
   - **WHT deduction exemption** for small-company payers (gated on `cit_exempt`): supplier holds valid TIN and payments to that supplier ≤ ₦2M in the calendar month (WHT Regs 2024, effective Jan 2025). No supplier TIN ⇒ prescribed deduction at 2× rate.
   - Setup wizard (Spec 02) must therefore ask: approximate annual turnover band, fixed-assets band, and whether the business is a professional-services firm — deriving `vat_registered`, `vat_exempt`, and `cit_exempt` from those answers rather than asking about tax law directly.
10. **New (from the WHT exemption):** the ₦2M test is a **calendar-month aggregate per supplier**, so exemption status is computed at payment time, not stored on the bill. (§6.2)

---

*End of Spec 01. Next per §11 order: Spec 02 — COA seed data + company setup wizard.*
