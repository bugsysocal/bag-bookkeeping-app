# Specification 07 — Dashboard & Owner/Advisor Mode UX
**Project:** LedgerOne (placeholder) · **Covers:** Planning doc §7, §4.6 owner-tier hierarchy, §11 item 8 · **Status:** APPROVED v1.0 (2026-07-03; #8 individually confirmed; #1–#7 batch-approved under the standing scope rule; reviewer endorsed §5 capability-table centralization pre-scaffolding)
**Depends on:** Specs 01–06 (all approved). This spec consolidates the mode-gating rules scattered across Specs 01–06 into one normative table (§5) — where wording differs, **this table wins** and the older spec should be read through it.

---

## 1. Scope

Dashboard composition and banner discipline, navigation structure, the two-mode model (Owner default / Advisor PIN-elevated), the owner-language lexicon and formatting standards, keyboard-fast entry for the accounts-officer persona, empty states, and offline-first UX rules. Visual design (colors, type, spacing) is implementation-stage; this spec fixes structure and behavior.

## 2. Dashboard (planning doc §7: five numbers, glanceable)

### 2.1 The five tiles
1. **Cash today** — consolidated NGN position (Spec 04 §5); tap → per-account list.
2. **Who owes me** — total open AR; tap → aging by customer (Spec 05 §3.2).
3. **What I owe** — total open AP + unremitted WHT/VAT shown as a secondary line (*"incl. ₦X tax collected, not yet remitted"* — the owner must never mistake tax-in-hand for spendable cash); tap → aging by supplier + tax lines.
4. **Profit this month** — simplified accrual P&L figure (Spec 05 §3.3); tap → owner P&L.
5. **Overdue invoices** — count + value (derived per Spec 03 §2); tap → overdue list, each row one tap from a WhatsApp reminder (Spec 03 §7).

Every tile is a live query (Spec 05 R1 discipline — nothing cached, nothing stored).

### 2.2 Primary actions
**New Invoice · Record Expense · Record Payment** — persistently visible on every screen (toolbar), not just Home. Everything else is secondary navigation.

### 2.3 Banner zone (the only place the app nags)
One stacking area under the tiles. Priority order, **max two visible** + *"n more"* expander — a wall of banners teaches owners to ignore banners:

1. **Compliance** (Spec 02 §5.9): threshold crossings — non-editable, advisor-clearable only.
2. **Needs review** (Spec 04 §7.5): flagged bank lines badge.
3. **Backup health** (Spec 09 will define triggers; slot reserved).
4. **Recurring due** (Spec 04 §4): *"Rent (₦450,000) is due — record it?"*
5. **Setup gaps** (Spec 02 W4): skipped-items checklist, dismissible.

Rule: banners either **deep-link to the fixing action** or (compliance) name who can fix it. No banner is ever purely decorative. *(Decision #2)*

## 3. Navigation

Left rail, owner language:

| Item | Contains |
|---|---|
| **Home** | dashboard |
| **Sales** | invoices, quotes, customers |
| **Purchases** | expenses, bills, suppliers |
| **Bank & Cash** | accounts, Move Money, Owner took money out, reconciliation, statement import |
| **Products** | price list (+ stock if inventory on) |
| **Reports** | owner-tier first; formal statements one click below (planning doc principle 5) |
| **Settings** | company, users, bank accounts, recurring, import/export |
| **Advisor** 🔒 | PIN gate → §5 capabilities. Visible but locked in Owner Mode — advertising that a deeper layer exists builds trust ("real books under the hood") and routes the advisor conversation. *(Decision #3)* |

## 4. Owner Language & Formatting (normative lexicon)

### 4.1 Lexicon
| Owner Mode says | Never says (Owner Mode) |
|---|---|
| Money In / Money Out | receipts/disbursements |
| Who owes me / What I owe | accounts receivable / payable |
| Bank & Cash | treasury, liquidity |
| Customer paid ahead | unearned revenue, customer deposit liability |
| Owner took money out / put money in | drawings, capital contribution |
| Tax collected for FIRS / Tax held back | VAT output, WHT payable |
| Record it | post, journalize |

**Forbidden in Owner Mode UI strings:** debit, credit, journal, ledger, posting, accrual, liability, equity, contra, trial balance. These words appear only in Advisor Mode and on formal statements (planning doc thesis). Enforced mechanically: all UI strings live in one reviewed string table, and a lint script greps the owner-scope strings for the forbidden list on every build. *(Decision #1)*

### 4.2 Formatting
₦ with thousands separators everywhere; dates DD/MM/YYYY; negatives in parentheses on formal statements only (Spec 05 R4). Amount inputs: separators auto-insert while typing; decimal point enters kobo; suffix shorthand **`k` = thousand, `m` = million** (`2.5m` → live-preview `₦2,500,000` before commit — preview mandatory, silent expansion forbidden). *(Decision #4)*

## 5. The Two Modes (consolidated, normative)

**Model:** Advisor Mode is a **PIN elevation over the current session**, not a separate login. Eligible: `owner` and `advisor` role users; `staff` never (Spec 02 matrix). Entry: 6-digit PIN (argon2, Spec 01). Exit: manual, or **auto-exit after 15 minutes idle** (`advisor_timeout_minutes`, company setting). While elevated: persistent badge + distinct accent color on every screen — a screenshot must be unambiguous about mode. Mode transitions (enter, exit, timeout, failed PIN) are audit-logged. 5 failed attempts → 15-minute lockout, audit-logged. *(Decision #5)*

**Advisor Mode capability table** (single source of truth; consolidates Specs 01–06):

| Capability | Origin |
|---|---|
| Manual journals (`postJournal`), incl. depreciation, FX revaluation, deemed-supply VAT | Spec 01 §6.7 |
| Trial balance, GL detail, raw journal views | Spec 05 §4.5 |
| Hard period lock (`hard_close_through`) | Spec 01 §3.1/T5 |
| Tax flags (`vat_registered`, `vat_exempt`, `cit_exempt`), VAT rate, WHT presets | Spec 02 #8 revised |
| Compliance banner clearing / acknowledgment | Spec 02 §5.9 |
| WHT exemption override (both directions), incl. `cit_exempt` gating judgment | Spec 01 §6.2 / Spec 04 §3 |
| ~~Write-off above `writeoff_limit_kobo`~~ **SUPERSEDED 2026-07-07 — Spec 04 §9 Decision #11 (closed, final): there is no Advisor-Mode bypass of the write-off limit, for any role, ever.** Above the limit, the only path is an ordinary manual journal entry matched via the standard manual-match flow — not a mode capability. Write-off *routing settings* (the limit amount itself, and the write-off Dr/Cr accounts) remain an Advisor-only capability in principle, but **no settings command exists yet to change them post-setup** — they're seeded once at company creation and are currently read-only. | Spec 04 §7.5, §9 Decision #11 |
| Needs-review queue (cross-account triage view) | Spec 04 §7.5 |
| OBE reclassification to Capital/Retained Earnings | Spec 02 §5.6 |
| Opening-balance void & repost | Spec 02 §5.9 |
| Inventory module OFF (zero-stock precondition) | Spec 02 §5.3 |
| `fiscal_year_start_month` change after first posting | Spec 02 §5.9 |
| Bulk transaction import | Spec 06 §4 |
| Back-dating reversals within open periods | Spec 03 §6 |

**Implementation-status footnote (added 2026-07-19, not part of the original approval — kept here so this table doesn't quietly drift further from reality):** as of Spec 07 build start, only two rows above have both an engine function *and* a Tauri command gated with `require_advisor_elevated`: **Trial balance, GL detail** (`trial_balance`/`general_ledger` commands) and, by extension, their `.xlsx` exports (Spec 06). Everything else in the table is either engine-only with no UI/IPC surface at all (manual journals — `engine::post_journal` exists, no Tauri command wraps it; OBE reclassification; opening-balance void & repost; bulk transaction import), or a company setting that's only ever set once at wizard time with no post-setup edit command (hard period lock, tax flags/VAT rate/WHT presets, inventory on/off, `fiscal_year_start_month`, write-off routing settings) — so there's nothing yet to *gate*. **Compliance banner clearing/acknowledgment** has no banner UI at all yet (that's Spec 07 §2.3's own job to build). One capability-gating question flagged during Spec 07 build, not resolved by this table as written: "WHT exemption override (both directions)" — the routine case of a supplier with no TIN needing an explicit withholding-amount decision at payment time (`EngineError::WhtDecisionRequired`) is currently reachable by *any* session role via `record_payment_out`, not gated to Advisor Mode, because a payment can't stall on advisor availability. Whether that's the capability this table means, versus a not-yet-built "permanently mark this supplier WHT-exempt" or "override the `cit_exempt` gating judgment" settings screen, is genuinely ambiguous from the table's wording alone — see PROGRESS.md's "Flagged for review" for the decision on record.

Everything not listed is Owner Mode (subject to the Spec 02 role matrix — staff restrictions ride the *role*, not the mode).

## 6. Keyboard-Fast Entry (accounts-officer persona, planning doc §7)

- **Tab order** on every form matches visual order; grids: Enter = next row, Esc = discard row.
- **Ctrl+D — duplicate last entry** of the current type, dates advanced to today, editable before save (the "same diesel purchase every Monday" path).
- **Date fields:** `t` = today, `+n`/`-n` = days from today, bare day-number = that day this month.
- **Pickers** (contact/product/account): type-ahead prefix + contained-word match, arrow/Enter select; **inline create** ("+ New customer 'Chidinma Stores'") without leaving the form — created with just the name, dedupe nudge if a near-match exists (Spec 06 §3 rules), details completable later. *(Decision #6)*
- Global: Ctrl+1/2/3 = the three primary actions; Ctrl+K = go-anywhere search (contacts, invoices by number, amounts).

## 7. Empty States, Errors, Offline

- **Empty states teach:** every empty list names the action that fills it ("No invoices yet — create your first" + the Excel-import path for migrating businesses, Spec 06 §3).
- **Errors name real things:** validation messages use the owner's nouns and numbers — *"You're selling 12 crates of Peak Milk but only 8 are in stock"*, never "constraint violation P7." Every engine precondition (Spec 01 P1–P8) gets an owner-language message in the string table; the advisor sees the technical code in a detail line.
- **Offline-first (planning doc principle 1):** no core flow ever blocks on network. Online-adjacent affordances (wa.me open, FX rate fetch) fail soft with a stated fallback ("Couldn't fetch today's rate — using your last one: ₦1,520/$, from 28/06"). No spinners on anything local — SQLite queries at SME volume render instantly or the query is wrong.

## 8. Deltas (additive)

- `companies` + `advisor_timeout_minutes INTEGER NOT NULL DEFAULT 15`.
- New `user_prefs (user_id, pref_key, pref_value, PRIMARY KEY(user_id, pref_key))` — last-used bank account, dismissed setup nags, grid column widths. UI state only; **nothing financial may ever live here** (stated in the schema comment).
- Audit-log action vocabulary gains `mode.entered` / `mode.exited` / `mode.timeout` / `mode.pin_failed` / `mode.lockout`. No engine changes.

## 9. Decisions needing your sign-off

1. **Forbidden-lexicon enforcement is mechanical** — single string table + build-time lint against the banned-words list for owner-scope strings. (§4.1)
2. **Banner discipline:** fixed priority order, max two visible + expander, every banner deep-links or names who can fix it. (§2.3)
3. **Advisor rail item visible-but-locked in Owner Mode** rather than hidden. (§3)
4. **Amount shorthand `k`/`m` with mandatory live preview.** (§4.2)
5. **Advisor Mode = PIN elevation, 15-min idle auto-exit, visible badge, failed-attempt lockout, all transitions audit-logged.** (§5)
6. **Inline contact/product creation from pickers** with dedupe nudge — speed for the accounts officer without opening the free-text-category crack (it creates a real, deduped record, not a string). (§6)
7. **The §5 capability table is normative** — it consolidates and supersedes scattered mode wording in Specs 01–06 where they differ. (§5)
8. ✅ **Tile #3 shows unremitted tax as a named secondary line** — **CONFIRMED by reviewer 2026-07-03**, with the rationale on record: undisclosed, the tile teaches owners that tax-collected-but-unremitted is spendable cash — the exact habit that produces a VAT shortfall at filing time. A named line off existing balances is visible enough to correct behavior without violating the plain-language dashboard discipline, and carries no regulatory risk in the number itself (no new computation). (§2.1)

---

*End of Spec 07. Next per §11 order: Spec 08 — Backups (§11 item 9), then Spec 09 — Google Sheets push (§11 item 10).*
