# Specification 02 — COA Seed Data & Company Setup Wizard
**Project:** LedgerOne (placeholder) · **Covers:** Planning doc §4.5, §9 Phase 1, §11 item 2 · **Status:** APPROVED v1.0 (2026-07-03, with decision #8 revised per review: tax flags Advisor-only + threshold banner)
**Depends on:** Spec 01 (approved) — schema, system_keys, posting engine, NTA 2025 flag split (`vat_exempt` / `cit_exempt`).

---

## 1. Scope

1. The full seed Chart of Accounts, extending the 17 system accounts fixed in Spec 01 §5.
2. Seed data beyond the COA: document sequences, WHT rate presets.
3. The company setup wizard: flow, exact questions (owner language), derivation logic for the tax flags, opening-balance posting, multi-bank setup, users & roles.
4. Two small amendments to the Spec 01 schema that this spec surfaced (§6).

Out of scope: wizard visual design (Spec 08 UX), Excel onboarding import (Spec 07 — the wizard links to it but works without it).

---

## 2. COA Structural Decisions

- **Flat chart, no parent hierarchy in v1.** `accounts` has no `parent_id` (Spec 01). Grouping for statements is by **code band** (e.g., "Cash & Bank" = `is_bank` accounts; "Fixed Assets" = 1500–1599). Report headers are presentation-layer constructs. Cheap now; a `parent_id` migration later is additive. *(Decision #1)*
- **Everything is seeded for every company; modules hide, never omit.** Inventory-related accounts (1200, 5000) exist even when `inventory_enabled = 0` — they're simply excluded from owner-facing pickers. Enabling inventory later is then a settings change, not a migration. Same for payroll accounts (6000, 2230, 2240) ahead of the Phase 2 payroll-lite module. *(Decision #2)*
- **Code bands are reserved by convention:** 1010–1099 bank/cash accounts (wizard-created, `is_bank = 1`); user-added accounts get the next free code in the band matching their class. Codes are user-visible but never engine-meaningful (engine resolves by `system_key` only, per Spec 01).
- **Contra accounts carry no special flag.** 1590 Accumulated Depreciation (asset class, credit-normal) works because balances are signed; "normal balance" derived from class is a display hint, not a constraint.

## 3. Seed Chart of Accounts

`[S]` = `is_system = 1`, locked per Spec 01 T8 (system_keys in parentheses). Names are owner-facing and renameable except where locked; classes immutable always.

### 1000s — Assets

| Code | Name | Notes |
|---|---|---|
| 1010–1099 | *(Bank & cash accounts — created by wizard step 4)* | `is_bank = 1`; e.g. 1010 GTBank Current, 1020 POS Wallet, 1030 Petty Cash, 1040 USD Domiciliary |
| 1100 | Accounts Receivable | [S] (`AR`) |
| 1200 | Inventory | [S] (`INVENTORY`) — hidden when inventory off |
| 1310 | VAT Receivable (Input) | [S] (`VAT_INPUT`) — hidden when `vat_registered = 0` |
| 1320 | WHT Receivable | [S] (`WHT_RECEIVABLE`) — tax credits held from customers withholding |
| 1400 | Prepayments & Deposits Paid | Prepaid rent, supplier deposits |
| 1450 | Staff Loans & Advances | Salary advances — informal-sector reality, seeded so it never lands in Expenses |
| 1510 | Motor Vehicles | |
| 1520 | Furniture & Fittings | |
| 1530 | Computers & Office Equipment | |
| 1540 | Plant & Machinery | |
| 1550 | Generators & Power Equipment | Nigerian context: gensets are material capex, deserve their own line |
| 1590 | Accumulated Depreciation | Contra-asset; Advisor Mode journals only (planning doc §8) |

### 2000s — Liabilities

| Code | Name | Notes |
|---|---|---|
| 2100 | Accounts Payable | [S] (`AP`) |
| 2210 | VAT Payable (Output) | [S] (`VAT_OUTPUT`) — hidden when `vat_registered = 0` |
| 2220 | WHT Payable | [S] (`WHT_PAYABLE`) |
| 2230 | PAYE Payable | Seeded ahead of payroll-lite; advisor journals meanwhile |
| 2240 | Pension Payable | Same |
| 2250 | Company Income Tax Payable | Hidden from owner pickers when `cit_exempt = 1` |
| 2300 | Customer Deposits (Unearned Revenue) | [S] (`UNEARNED_REVENUE`) |
| 2410 | Bank Loans & Overdrafts | |
| 2420 | Director's / Owner's Loan | Owner lent the business money — distinct from capital, very common |
| 2500 | Accrued Expenses | Advisor Mode |

### 3000s — Equity

| Code | Name | Notes |
|---|---|---|
| 3000 | Opening Balance Equity | [S] (`OPENING_BALANCE_EQUITY`) — wizard plug account; advisor reclassifies to 3100/3900 after review; hidden from all owner pickers |
| 3100 | Owner's Capital | [S] (`OWNER_CAPITAL`) |
| 3200 | Owner's Drawings | [S] (`OWNER_DRAWINGS`) |
| 3900 | Retained Earnings | [S] (`RETAINED_EARNINGS`) — year-end close target |

### 4000s — Revenue

| Code | Name | Notes |
|---|---|---|
| 4000 | Sales Revenue | [S] (`SALES_DEFAULT`) — default `income_account_id` for products |
| 4100 | Service Income | Default for `kind = 'service'` products |
| 4200 | Other Income | Scrap sales, commissions, sundry |
| 4300 | Interest Income | |
| 4900 | FX Gain/Loss | [S] (`FX_GAIN_LOSS`) — Advisor Mode revaluations |

### 5000s — Cost of Goods Sold

| Code | Name | Notes |
|---|---|---|
| 5000 | Cost of Goods Sold | [S] (`COGS_DEFAULT`) — auto-posted by `postInvoice` when inventory on |
| 5100 | Purchases (Goods for Resale) | **The COGS path when inventory is OFF:** stock bills post here directly; no per-sale COGS entry exists. Hidden when inventory is ON (bills route to 1200 instead) |
| 5200 | Carriage Inwards & Clearing | Freight, customs clearing — trading/import reality |
| 5300 | Direct Labour | Light manufacturing |

### 6000s — Operating Expenses

| Code | Name | Notes |
|---|---|---|
| 6000 | Salaries & Wages | |
| 6100 | Rent | |
| 6200 | Utilities & Power | **Planning-doc-mandated distinct line** — electricity, diesel, generator running. Owners may add 62xx splits (e.g. 6210 Diesel) if they want the detail |
| 6300 | Transport & Logistics | Vehicle fuel, deliveries, dispatch |
| 6400 | Marketing & Advertising | |
| 6500 | Communication & Internet | Airtime, data — a real line item here |
| 6600 | Professional & Legal Fees | |
| 6650 | Licenses, Levies & Permits | LG levies, signage, regulatory — fragmented and frequent in Nigeria |
| 6700 | Repairs & Maintenance | |
| 6750 | Insurance | |
| 6800 | Office Supplies & Consumables | |
| 6850 | Staff Welfare & Entertainment | |
| 6870 | Security Services | |
| 6900 | Bank & POS Charges | [S] (`BANK_CHARGES`) — also the reconciliation module's default for unmatched charge lines |
| 6930 | Bad Debts Written Off | Advisor Mode |
| 6950 | Depreciation Expense | Advisor Mode journals |
| 6980 | Miscellaneous Expenses | Deliberately last and unglamorous; if it grows, the advisor splits it |
| 6990 | Rounding Differences | [S] (`ROUNDING`) — hidden from all pickers |

**Seed count: 17 system + 33 user-space = 50 accounts** before wizard-created bank accounts. Small enough to scroll, complete enough that "add account" is rare in month one — every added account is a crack an informal-system user can fall into (planning doc principle 3).

## 4. Other Seed Data

### 4.1 Document sequences (per company)

| doc_type | prefix | next_number |
|---|---|---|
| invoice | `INV-` | wizard-asked (see step 7) |
| quote | `QUO-` | 1 |
| receipt | `RCT-` | 1 |

### 4.2 WHT rate presets (`wht_rate_presets`, user-editable data)

⚠️ **Rates below are the commonly cited WHT Regulations 2024 figures and MUST be confirmed against the current Regulations schedule during implementation review** — they are seed *data*, editable in settings, never code. Corporate-recipient rates seeded (target users transact mostly B2B); the label carries the scope.

| Label | rate_bp |
|---|---|
| Supply of goods | 200 |
| Services / contracts (general) | 200 |
| Professional & consultancy fees | 500 |
| Rent & hire of equipment | 1000 |
| Interest & dividends | 1000 |
| Construction | 200 |

(Reminder from Spec 01 §6.2: no supplier TIN ⇒ engine offers 2× the preset as the default, with warning.)

---

## 5. Company Setup Wizard

### 5.0 Ground rules

- **W1 — Atomic creation.** The wizard collects everything, then commits in **one transaction**: company row, 50-account COA, bank accounts, contacts, products (if entered), sequences, WHT presets, users, opening journal entry, audit rows. Cancel at any step = nothing exists. A half-configured company is unrepresentable. *(Decision #3)*
- **W2 — Owner language throughout.** No question mentions VAT law, thresholds, debits, or equity. Every derived flag shows a plain-language consequence the user confirms.
- **W3 — Everything is revisitable**, but at different privilege levels (table in §5.9).
- **W4 — Skippable ≠ omitted.** Opening balances, products, and the second user can be skipped; the wizard ends with a checklist of what was skipped, and the dashboard nags gently until resolved or dismissed.
- **W5 — Multi-company.** The wizard is re-runnable ("Add another business") — advisor persona manages several clients (planning doc §5.2).

### 5.1 Step 1 — Business basics

Fields: business name*, business type (dropdown: Trading/Distribution · Services · Light Manufacturing · Agency/Brokerage · Mixed), phone, address, TIN (optional — "Your FIRS Tax Identification Number, if you have one. You can add it later."), logo upload (for invoices), **financial year start month** (default January; phrased "When does your business year start? Most businesses: January").

Writes: `companies` row fields incl. `fiscal_year_start_month` (schema amendment, §6).

### 5.2 Step 2 — Tax profile (the three questions)

Asked in owner language; **the user answers about their business, never about tax law**:

| # | Question (exact wording) | Answers | Feeds |
|---|---|---|---|
| Q1 | "Roughly how much does your business sell in a year?" | Under ₦50M · ₦50M–₦100M · Over ₦100M | `T` |
| Q2 | *(only if T ≠ Over ₦100M)* "Is the total value of your business property — vehicles, equipment, machinery, buildings — under ₦250 million?" | Yes · No · Not sure *(Not sure ⇒ treated as No, flagged for advisor)* | `A` |
| Q3 | "Is your business a professional practice — law, accountancy, consulting, engineering services, medical practice, or similar?" | Yes · No | `P` |
| Q4 | "Are you registered for VAT with FIRS?" | Yes · No · Not sure | override for `vat_registered` |

**Derivation (from Spec 01 §9, NTA 2025 two-threshold split):**

```
vat_exempt      = (T = Under ₦50M)                  AND NOT P
cit_exempt      = (T ≠ Over ₦100M) AND (A = Yes)    AND NOT P
vat_registered  = Q4 if answered Yes/No
                  else NOT vat_exempt               -- suggested default, user confirms
```

Edge the wizard must handle: `vat_exempt = 1` but Q4 = Yes (voluntarily/already registered) ⇒ `vat_registered = 1` wins — the engine charges VAT. `vat_exempt = 0` but Q4 = No ⇒ accept but show: *"Businesses selling above ₦50M must register for VAT with FIRS. LedgerOne will not add VAT to your invoices until your advisor switches this on."* — recorded in audit_log; the app never blocks setup on a compliance gap, it surfaces it. *(Decision #4)*

Confirmation screen (plain language): *"✓ You'll charge 7.5% VAT on your invoices. ✓ You don't need to deduct tax when paying suppliers with a TIN, for payments under ₦2M a month. ✗ Company income tax applies to your business."* — each line maps to `vat_registered`, `cit_exempt` (WHT exemption), `cit_exempt` respectively.

### 5.3 Step 3 — Stock question

*"Do you buy goods and hold them to resell? (If you mainly sell services, or buy only when a customer orders, choose No — you can switch this on later.)"* → `inventory_enabled`.

Switching ON later: allowed anytime (Settings), prompts opening-stock entry. Switching OFF later: Advisor Mode only, requires zero quantity on hand across all products. *(Decision #5)*

### 5.4 Step 4 — Bank & cash accounts

Grid, one row per account: label*, type (Bank account · POS wallet · Cash box · Domiciliary), bank name, last 4 digits, currency (NGN default; USD/GBP/EUR for domiciliary), **balance right now** + as-of date, and for foreign currency: *"What rate should we use to show this in naira?"* (creates the `fx_rates` row).

Pre-filled suggestion rows: one bank account + "Petty Cash". Minimum one account to proceed. Each row creates an `accounts` row (next code in 1010–1099, `is_bank = 1`) + `bank_accounts` row. Balances feed step 6's journal — **not** stored as columns (Spec 01 §3.7).

### 5.5 Step 5 — Who owes you / whom do you owe (optional, skippable)

Two lists, owner language:

- *"Who owes you money right now?"* — rows: customer name, phone, amount, since when. Creates `contacts` (kind customer) + feeds opening AR lines (per-contact, satisfying Spec 01 P8).
- *"Who do you owe money to?"* — supplier name, phone, amount. Creates `contacts` (kind supplier) + opening AP lines.
- If `inventory_enabled`: *"What stock are you holding?"* — product name, quantity, what you paid per unit. Creates `products` (+ opening `inventory_movements`, kind `opening`, establishing initial WAC).
- Advanced (collapsed): equipment/vehicles at cost → 15xx lines; outstanding loans → 2410/2420 lines.

Skip link: *"Start fresh — I'll only record things from today."* Also: *"Have all this in Excel?"* → points to the Spec 07 importer (Phase 1 but separately specced); wizard fields remain the no-Excel path.

### 5.6 Step 6 — Opening balances journal (system-generated, invisible to owner)

One compound journal entry, `source_type = 'opening_balance'`, dated the as-of date (default: first day of current financial year, or today if mid-year start — user picks in step 5 header), memo "Opening balances at setup":

| Line | Account | Amount |
|---|---|---|
| Dr | each bank/cash account | balance entered (fx fields set for domiciliary) |
| Dr | 1100 AR | per customer, one line each, `contact_id` set |
| Dr | 1200 Inventory | Σ qty × unit cost (if inventory on) |
| Dr | 15xx Fixed assets | as entered |
| Cr | 2100 AP | per supplier, one line each, `contact_id` set |
| Cr | 2410/2420 Loans | as entered |
| Dr/Cr | **3000 Opening Balance Equity** | the plug — whatever makes the entry balance |

Balanced **by construction** (OBE is the residual), posted through `postJournal` internals under W1's single transaction, subject to Spec 01 T1 like everything else. Negative bank balances (overdraft) allowed — they simply post Cr. If the user entered nothing, no entry is created.

**Worked example** (₦ for readability): GTBank 2,400,000 · Petty cash 150,000 · Customers owe 1,850,000 (2 contacts) · Owes supplier 900,000 · Stock 3,100,000 →
Dr 1010 2,400,000 · Dr 1030 150,000 · Dr 1100 1,100,000 (Chidinma Stores) · Dr 1100 750,000 (Emeka & Sons) · Dr 1200 3,100,000 · Cr 2100 900,000 (Mainland Suppliers) · **Cr 3000 6,600,000**. Trial balance = 0 from the first second of the company's existence.

Advisor's post-setup job (surfaced as an Advisor Mode task): reclassify OBE into 3100 Capital / 3900 Retained Earnings once real prior-period figures are known.

### 5.7 Step 7 — Invoice numbering

*"What number should your first invoice have?"* — default 1 (renders INV-000001); a migrating business continuing from a paper book can enter e.g. 2451. Prefix editable here (default `INV-`). Quotes and receipts start at 1 silently. *(Decision #6)*

### 5.8 Step 8 — People & PIN

1. **Owner** (required): name → `users` row, role `owner`.
2. **Accounts officer** (optional, skippable): name → role `staff`. Plain-language framing: *"Does someone else enter records for you? They'll get their own login so you can see who recorded what."* (open decision #2 of the planning doc: two roles from day one — this is where it lands).
3. **Advisor Mode PIN** (required, set by owner; 6 digits, argon2-hashed per Spec 01): *"This PIN protects the accountant's area. Your advisor will ask you for it when they review your books."* Advisor `users` rows are created later, on first advisor session, PIN-gated. *(Decision #7)*

Role capability matrix (v1):

| Capability | owner | staff | advisor |
|---|---|---|---|
| Invoices, expenses, payments, transfers, drawings | ✓ | ✓ | ✓ |
| Void documents | ✓ | — | ✓ |
| Edit company settings / tax flags | ✓ | — | ✓ |
| Advisor Mode (journals, TB, GL, close, reclassify OBE) | PIN | — | PIN |
| Manage users | ✓ | — | — |

### 5.9 Step 9 — Review & create

Summary card per step → single **Create my books** button → W1 atomic commit → dashboard, with the W4 skipped-items checklist. Where answers are revisited later:

| Setting | Where editable |
|---|---|
| Name, phone, logo, address | Settings (owner) |
| Tax flags (`vat_registered`, `vat_exempt`, `cit_exempt`), VAT rate | **Advisor Mode only (PIN-gated)**; changes audit-logged. Owner gets a **non-editable dashboard banner** the moment a threshold crossing is detected from the ledger itself: fiscal-YTD revenue > ₦50M while `vat_registered = 0` → *"Your sales have passed ₦50M — you may now need to register for VAT. Talk to your advisor."*; fiscal-YTD revenue > ₦100M while `cit_exempt = 1` → equivalent CIT wording. Banner clears only when an advisor updates the flags or acknowledges it in Advisor Mode. Re-derivation reminder (re-ask the §5.2 questions) fires at the start of each **fiscal year** (`fiscal_year_start_month`), not calendar January *(Decision #8, revised)* |
| `fiscal_year_start_month` | Advisor Mode only once any entry is posted |
| Inventory on | Settings (owner) |
| Inventory off | Advisor Mode, zero-stock precondition |
| Opening balances | Advisor Mode only (void + repost the opening entry) |
| Add/edit bank accounts, users | Settings (owner) |

---

## 6. Amendments to Spec 01 (schema deltas)

1. `companies` **+ `fiscal_year_start_month INTEGER NOT NULL DEFAULT 1`** (`CHECK (fiscal_year_start_month BETWEEN 1 AND 12)`) — required by retained-earnings close and "profit this quarter"; wizard step 1 sets it.
2. `companies` **+ `business_type TEXT`** (`CHECK (business_type IN ('trading','services','manufacturing','agency','mixed'))`) — wizard step 1; drives nothing in the engine (display/analytics only), so nullable-in-spirit but defaulted to `'mixed'`.

Both are additive; no Spec 01 template or trigger changes.

## 7. Decisions needing your sign-off

1. **Flat COA** (no `parent_id`) with code-band grouping; headers live in the report layer. (§2)
2. **Seed everything, hide by module** — inventory/payroll/VAT accounts always exist; pickers filter. (§2)
3. **Wizard commits atomically** at the end — cancel leaves zero rows. (§5.0)
4. **Compliance gaps warn, never block** — e.g., >₦50M turnover but not VAT-registered: recorded, surfaced, not enforced. (§5.2)
5. **Inventory: on anytime, off only via Advisor Mode at zero stock.** (§5.3)
6. **Invoice numbering can start from an arbitrary number** for migrating businesses (sequential and gap-free from there per Spec 01). (§5.7)
7. **Owner sets the Advisor PIN at setup**; advisor user rows created on first advisor session. Alternative: defer PIN to first advisor visit — I prefer setup-time so Advisor Mode is never PIN-less. (§5.8)
8. ~~Tax flags owner-editable in Settings~~ **REVISED per review 2026-07-03:** tax flags are **Advisor Mode only (PIN-gated)**. The "threshold crossing shouldn't wait for the advisor" concern is answered with visibility, not edit power: a non-editable dashboard banner fires from ledger data the moment fiscal-YTD revenue crosses ₦50M (while unregistered) or ₦100M (while CIT-exempt). Annual re-derivation reminder tied to `fiscal_year_start_month`, not calendar year. (§5.9)
9. ⚠️ **WHT preset rates (§4.2) need confirmation** against the current WHT Regulations 2024 schedule before ship — seed data, not structure.
10. **COA completeness check (your accountant's eye):** anything missing for the ₦20M–₦500M trading/services/light-manufacturing segment? Candidates I left out deliberately: Discounts Allowed (line discounts already net off revenue per Spec 01), Suspense (an invitation to dump things), separate Diesel line (owners can split 6200 themselves). **CONFIRMED 2026-07-03, with a recorded obligation on Spec 04:** the no-Suspense decision holds only if reconciliation gives accounts officers an explicit "unclear — needs review" mechanism (a workflow state, not a COA account). Spec 04 must design this explicitly, not leave it implicit.

---

*End of Spec 02. Next per §11 order: Spec 03 — Invoicing + payments flow.*
