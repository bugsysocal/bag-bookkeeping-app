# Specification 04 — Expenses/Bills, Banking & Reconciliation
**Project:** LedgerOne (placeholder) · **Covers:** Planning doc §4.3, §4.4, §11 items 4–5 · **Status:** APPROVED v1.2 — all decisions closed (2026-07-03; #7 amended: write-off threshold **and** routing accounts are company-level settings; #2–5, #8–9 approved via scope audit — none touch VAT/WHT posting or the balance guarantee. **2026-07-07: Decision #11 CLOSED — no in-app bypass of the write-off limit, ever, for any role; above-limit lines route to a manual journal + ordinary manual-match, see §7.5/§9.**)
**Depends on:** Spec 01 (posting templates §6.2–§6.6, P1–P8, T1–T8), Spec 02 (COA, roles, tax flags), Spec 03 (allocation/void patterns, mirrored here).
**Carried obligation (Spec 02 decision #10):** no Suspense account exists by design → this spec designs the explicit **"unclear — needs review"** reconciliation state (§7).

---

## 1. Scope

Money-out and the bank layer: quick cash expenses vs. supplier bills (two distinct flows), supplier payments with the NTA 2025/WHT Regs 2024 WHT split, recurring expenses, the multi-bank sub-ledger, inter-account transfers, owner's drawings, bank statement import (CSV/Excel), and the full reconciliation workflow including the needs-review state and reconciliation locks.

Ledger effects were fixed in Spec 01; this spec defines workflows, validations, and the additive deltas (§8). Out of scope: VAT/WHT *reports* (Spec 06), bank feeds (Phase 2).

---

## 2. Expenses & Bills — Two Flows, One Pipeline

The UI presents two clearly separated flows (planning doc §4.3); both ride the bills pipeline per the approved Spec 01 §6.3 decision:

| | **Quick expense** ("Money Out — paid now") | **Supplier bill** ("I owe this, due later") |
|---|---|---|
| Owner mental model | "I paid for diesel" | "Mainland Suppliers will collect ₦900k on the 30th" |
| Data | `bills` row `status='paid'`, created + settled in **one transaction** | `bills` row `status='open'`, settled later via supplier payment |
| Ledger | bill entry + payment entry together (Spec 01 §6.3/§6.4) | bill entry now (Dr expense / Dr VAT input / Cr AP); payment entry at settlement |
| WHT | rare; gross-entry variant (Spec 01 §6.3) | flag + rate on bill; **split happens at payment** (§3) |

**Quick expense form:** date, payee (free text or contact — free text does *not* create a contact; repeat payees get a "save as supplier?" nudge), category (COA-mapped dropdown: expense + COGS accounts only, per Spec 02 picker rules), amount paid, bank/cash account, "price includes VAT" toggle (back-out per Spec 03/Spec 01 §6.3), optional receipt photo/file → `attachments` (§8.2). Keyboard-fast: tab-through, duplicate-last-entry (Spec 08 owns polish; the data path is here).

**Supplier bill form:** supplier (contact required — AP lines need `contact_id`, P8), supplier's invoice ref, bill date, due date (defaults from supplier terms), lines (description, qty, unit cost, account, `vat_claimable` defaulting per Spec 01 §6.4 NTA rule), WHT-applicable flag + rate preset picker. Inventory lines create movements + WAC recalc at posting (Spec 01 §6.4). Draft bills post nothing; posting on save-as-open (mirrors Spec 03 draft semantics).

**Validations** mirror Spec 03 V1–V5 with the obvious substitutions; plus: B1 — a quick expense from a **cash/POS account cannot drive it negative** (a cash box holds what it holds); bank-kind accounts warn but allow (overdrafts are real). *(Decision #1)*

## 3. Supplier Payments (direction `out`)

Mirror of Spec 03 §5.1: pick supplier → open bills grid, FIFO by due date, editable allocations. Ledger per Spec 01 §6.2: Dr AP gross / Cr Bank net / Cr WHT Payable.

**WHT computation at payment (verified parameters, Specs 01/02):**

1. For each allocated bill flagged `wht_applicable`: `wht = round(allocated ex-VAT portion × wht_rate_bp / 10000)`.
2. **Exemption check** (computed live, never stored on the bill): `cit_exempt = 1` AND supplier has valid `contacts.tin` AND calendar-month aggregate payments to that supplier (this payment included) ≤ ₦2,000,000 → WHT defaults **off**, with the plain-language note (Spec 01 §6.2). Advisor-overridable both directions.
3. **No supplier TIN** on a WHT-applicable payment → warning + 2× preset rate offered as default, never silently applied (Spec 01 §6.2).
4. The payment screen shows the supplier the net they'll receive and the WHT to be remitted — the number the owner reads out during the transfer call.

Voids mirror Spec 03 §6 (payment void → bill statuses recomputed; bill void blocked while active allocations exist).

## 4. Recurring Expenses

Planning doc §4.3: rent, salaries, subscriptions. **v1 = remind + one-click draft, never silent auto-posting** — an unattended ledger write violates the "nothing silent" discipline that runs through Specs 01–03. `recurring_templates` (§8.2) stores a bill/expense prototype + schedule (monthly/quarterly/yearly, day-of-month, next_due). On/after `next_due`, a dashboard prompt: *"Rent (₦450,000) is due — record it?"* → one click opens the pre-filled form → normal posting path. Skipped occurrences are logged, not lost. Auto-draft (creating unposted drafts automatically) is the only automation; auto-*post* is explicitly rejected. *(Decision #2)*

## 5. Multi-Bank Sub-Ledger & Transfers

Structure is fully inherited (Spec 01 §3.7, Spec 02 §5.4): each bank/cash account = one COA asset account (1010–1099) + `bank_accounts` metadata; opening balances live in the opening JE; running balance = `SUM(journal_lines.amount_kobo)` over the account — never a cached column.

**Dashboard cash position:** Σ NGN balances of all active accounts. Foreign-currency accounts show two numbers: the NGN ledger balance (historical, what the books say) and, in Advisor Mode only, an indicative revaluation at the latest `fx_rates` entry with the unrealized difference (planning doc §4.4) — revaluation *posting* remains a manual advisor journal (Spec 01 §6.5).

**Transfers ("Move Money", first-class primary action):** posting per Spec 01 §6.5 — never income, never expense. Form: from-account, to-account (must differ), amount, date, optional fee (→ 6900, same entry), reference. Cross-currency: both legs entered (NGN out, FX received + rate) — no gain/loss arises on the transfer itself (Spec 01 §6.5). Validations: B1 cash-negative rule applies to the source; reconciliation lock P6 applies to both legs.

**Owner's drawings ("Owner took money out" — the guilt-free button):** posting per Spec 01 §6.6 — Dr 3200 Drawings / Cr bank, or the `in` direction to 3100 Capital. Lives beside Move Money in the banking screen and as a payee-type shortcut inside the quick-expense form (choosing it routes to `postDrawing`, so "I paid myself" never lands in Expenses even when the owner starts down the wrong path). No VAT, no WHT, no contact — structurally impossible, not just discouraged.

## 6. Bank Statement Import (CSV/Excel)

1. **Format mapping.** Nigerian banks export inconsistent CSV/Excel layouts. First import per bank account runs a mapping step: pick columns for date / description / amount (single signed **or** separate debit+credit columns), date format, header rows to skip, sign convention. Saved as a `bank_import_profiles` row (§8.2); subsequent imports are one click. Profiles are per bank account, editable.
2. **Normalization.** Amounts → signed kobo from the *bank account's* perspective (+ = money in). Dates → ISO. Description trimmed verbatim (matching evidence — never rewritten).
3. **Deduplication.** Each line gets `import_hash = SHA-256(bank_account_id, date, amount_kobo, normalized description, occurrence_index)` — `occurrence_index` disambiguates genuinely identical same-day lines (two identical POS settlements are normal). Re-importing an overlapping statement skips existing hashes and reports "38 lines imported, 12 already present." *(Decision #3)*
4. Imported lines land in `reconciliation_lines` (Spec 01 §3.7) with `state='unmatched'`, tied to an open reconciliation session (§7.1).

## 7. Reconciliation Workflow

### 7.1 Session

Per bank account: **Start reconciliation** → statement end date + statement closing balance (typed from the paper/PDF statement — the tie-out target) → import/append statement lines (§6). One open session per account at a time. The screen shows the running equation the whole time:

```
statement closing balance
− unmatched + needs-review statement lines
= matched ledger balance
+ outstanding ledger items (in books, not yet on statement)
= ledger balance at statement date          ✓/✗ difference shown live
```

### 7.2 Auto-match pass

A statement line auto-matches a ledger line on the same bank account when: exact `amount_kobo`, date within **±3 days**, ledger line not already matched in any reconciliation, and the match is **unique** (exactly one candidate). Ambiguous candidates (two ₦50,000 transfers same week) are *not* guessed — the line stays unmatched with candidates pre-listed for one-click manual choice. Auto-matches are visually distinct and individually revocable before completion. *(Decision #4)*

### 7.3 Manual matching

Unmatched statement line → candidate ledger lines (same account, amount-proximity and date-sorted). **1:N allowed**: one statement line may match several ledger lines summing exactly to it (owner records two supplier payments; bank shows one bulk transfer). N:1 (several statement lines to one ledger entry — POS settlements paid in tranches) likewise, sum-exact. No partial/fuzzy-amount matches ever — amounts tie exactly or they don't match. *(Decision #5)*

### 7.4 Create-from-line

Unmatched bank line with no ledger counterpart = a real, missing entry. Guided creation, prefilled from the line:

| Line looks like | Prefilled flow | Posts via |
|---|---|---|
| debit, small, fee-worded | Bank charge → 6900 | `postExpense` |
| debit, other | Quick expense / supplier payment / transfer-out / drawing | respective Spec 01 functions |
| credit | Customer payment (opens Spec 03 §5 form) / transfer-in / deposit / owner capital | respective functions |

The created entry auto-matches its source line (`match_kind='created'`). All P-rules apply — creation from reconciliation is ordinary posting, not a bypass.

### 7.5 The **Needs Review** state (the no-Suspense mechanism)

**Principle:** an unclear line is a *workflow* problem, not a *ledger* problem. While a line is in needs-review, **nothing is posted anywhere** — the ledger stays clean, which is exactly why no Suspense account needs to exist. The quarantine lives on the statement line, in the reconciliation module, visibly.

**Who and when:** any role — this is specifically the accounts officer's mechanism ("I don't know what this ₦180,000 debit is; Oga will know"). One click on any unmatched line: **"I'm not sure — flag for review."** A note is **mandatory** (*"What do you know about this line?"* — even "no idea, 15 March, GTB app shows nothing" is evidence).

**Fields** (on `reconciliation_lines`, §8.1): `state='needs_review'`, `review_note` (mandatory, appendable — a mini-thread), `flagged_by`, `flagged_at`, `resolved_by`, `resolved_at`, `resolution` (`matched` / `entry_created` / `written_off` / `import_error`).

**Surfacing:**
- Reconciliation screen: dedicated **Needs review (n)** tab, lines with notes and age.
- Dashboard: persistent badge (*"3 bank lines need review"*) for owner and advisor — same visibility-without-silent-resolution pattern as the Spec 02 threshold banner. It nags until zero.
- Advisor Mode: needs-review queue across all accounts, oldest first, with notes — the advisor's monthly triage list.

**Completing a session around them:** completion does **not** require zero needs-review lines (review must not block reconciling everything else). Completion requires: every statement line is `matched` / `entry_created` / `written_off` / `needs_review` (i.e., none merely `unmatched` — every line got a decision, even if the decision is "unclear"), and the §7.1 equation ties **with needs-review lines shown as a named exception block**. Such a session completes as `completed_with_exceptions`; its needs-review lines **carry forward** into the next session automatically (`carried_from_id`), so they cannot age out of sight. `last_reconciled_date` is stamped either way — the lock (P6) protects what *was* agreed. *(Decision #6)*

**Resolution paths** (from the queue, any time, including after session completion):
1. **Matched** — to a ledger line that turned out to exist (or exists now). Pure state change; no posting.
2. **Converted to entry** — §7.4 flow, informed by the note thread. Ordinary posting.
3. **Written off** — for genuinely unexplainable residuals **at or below the company's write-off limit**: posts a real entry (two-line, balanced; deliberately **no VAT treatment** — no input-VAT back-out on written-off debits; advisor journals it if ever material). Targets and threshold are **company-level settings** (§8.3), seeded per below, advisor-editable per client (advisory practices flex these by client size — per the advisor-gated-controls rule these are Advisor Mode settings, seeded at company creation, not wizard questions for owners): debit lines → `writeoff_debit_account_id` (seeded 6980 Miscellaneous; fee-worded lines suggest 6900); credit lines → `writeoff_credit_account_id` (seeded 4200 Other Income). Guardrails: staff can never write off, regardless of amount; owner or advisor may write off **at or below** `writeoff_limit_kobo` (seeded ₦5,000). Every write-off records the note thread in the entry memo and audit log. *(Decision #7, approved as amended)*

   ✅ **Decision #11 (CLOSED, final — 2026-07-07) — there is no in-app bypass of the write-off limit, for any role, ever.** Above `writeoff_limit_kobo`, this resolution path does not exist at all — not for staff, not for the owner, not for an elevated advisor session. The only way to clear an above-limit line is: the advisor posts an ordinary **manual journal entry** (Advisor Mode, `postJournal`) covering the amount, then matches this statement line to that journal's bank leg through the **existing, ordinary manual-match flow (§7.3)** — exactly as if it were any other unmatched line. This requires **no new engine surface**: `manual_match` already accepts any posted journal line on the account, sum-exact, regardless of what posted it. The write-off action itself (`ledger_core::recon::write_off`) refuses unconditionally above the limit, for every role — this is not a placeholder pending a future elevated-bypass feature; it is the permanent design. Rationale: an amount large enough to exceed the company's own write-off threshold is, by definition, material enough to warrant a real accounting judgment (which account, which period, what the residual actually represents) rather than a one-click "clear it" button — even from an elevated session. Putting that judgment through the same manual-journal path as any other advisor adjustment, rather than inventing a second write-off tier, also means there is exactly one place in the codebase that posts arbitrary advisor-judgment entries, not two.
4. **Import error** — line was garbage (mangled row, duplicate the hash missed): marked `resolution='import_error'`, excluded from the equation, no posting. Advisor or owner only.

### 7.6 Outstanding ledger items & the lock

Ledger lines on the account with no statement match (cheque not presented, transfer recorded early) are listed as outstanding — normal, no action. Items outstanding across **two completed sessions** are flagged (*"recorded 9 weeks ago, never appeared at the bank — is this real?"*) with a shortcut to void (Spec 03 §6 semantics) or flag for advisor. Completion stamps `last_reconciled_date`; P6 (Spec 01) blocks new/reversal postings to the account dated on or before it, except through this module.

## 8. Deltas (additive) to Specs 01–03

### 8.1 `reconciliation_lines` — replace the bare `match_kind` model
```sql
-- amended columns (supersedes Spec 01 §3.7 sketch):
state            TEXT NOT NULL DEFAULT 'unmatched' CHECK (state IN
                 ('unmatched','matched','entry_created','needs_review','written_off','import_error')),
match_kind       TEXT CHECK (match_kind IN ('auto','manual','created')),   -- when matched
import_hash      TEXT NOT NULL,                       -- §6.3 dedup; UNIQUE per bank account
review_note      TEXT,                                -- mandatory when state='needs_review'; append-only thread
flagged_by       TEXT REFERENCES users(id),
flagged_at       TEXT,
resolved_by      TEXT REFERENCES users(id),
resolved_at      TEXT,
resolution       TEXT CHECK (resolution IN ('matched','entry_created','written_off','import_error')),
carried_from_id  TEXT REFERENCES reconciliation_lines(id)   -- §7.5 carry-forward chain
```
Plus: `reconciliations.status` CHECK gains `'completed_with_exceptions'`; matches become a join table `reconciliation_matches (reconciliation_line_id, journal_line_id)` to support 1:N/N:1 (§7.3), replacing the single `matched_line_id` column.

### 8.2 New tables
- `attachments (id, company_id, entity_type CHECK IN ('bill','payment','journal_entry'), entity_id, filename, stored_path, created_by, created_at)` — files under `{app_data}/companies/{id}/attachments/`, path-referenced, never blobbed into SQLite. *(Decision #8)*
- `recurring_templates (id, company_id, kind CHECK IN ('expense','bill'), template_json, frequency CHECK IN ('weekly','monthly','quarterly','yearly'), day_of_month, next_due, is_active, created_at)`.
- `bank_import_profiles (id, company_id, bank_account_id, label, mapping_json, date_format, sign_convention, header_rows, created_at)`.

### 8.3 Engine & settings
- `journal_entries.source_type` CHECK gains `'reconciliation_writeoff'`.
- `companies` + `writeoff_limit_kobo INTEGER NOT NULL DEFAULT 500000` (₦5,000), + `writeoff_debit_account_id TEXT REFERENCES accounts(id)` (seeded → 6980), + `writeoff_credit_account_id TEXT REFERENCES accounts(id)` (seeded → 4200) — all three Advisor Mode settings, seeded at company creation (Spec 02 wizard does not ask owners about write-off policy).
- No new posting functions: reconciliation creates entries only through the existing Spec 01 surface.

## 9. Decisions — ALL APPROVED 2026-07-03

Review outcome: #1, #6, #10 approved as drafted (#10: update Spec 01 to carry the new shape as current-approved — done, Spec 01 v0.3). #7 approved with amendment: threshold **and** routing accounts become company-level Advisor Mode settings (reflected in §7.5/§8.3). #2, #3, #4, #5, #8, #9 approved under the reviewer's scope rule after audit: none touch VAT/WHT posting logic or the double-entry balance guarantee — they are matching/workflow/storage mechanics, and any entries they lead to flow through the approved Spec 01 posting surface. Nearest-the-line note: #7 write-offs post real entries, but two-line balanced with no VAT treatment (conservative).

1. **Negative-balance policy by account kind:** cash/POS accounts block going negative; bank accounts warn but allow (overdrafts exist). (§2, §5)
2. **Recurring = remind + one-click prefilled draft; auto-posting rejected outright.** (§4)
3. **Import dedup** by content hash with occurrence-index for identical same-day lines; overlapping re-imports skip-and-report. (§6)
4. **Auto-match rule:** exact amount + ±3 days + unique candidate; ambiguity never guessed. Tolerance window configurable per company later if needed — shipping fixed at 3. (§7.2)
5. **Manual 1:N and N:1 matching, sum-exact only** — no fuzzy-amount matching anywhere. (§7.3)
6. **Needs-review completion semantics:** sessions complete around flagged lines as `completed_with_exceptions`; flagged lines carry forward automatically; every line must receive *a* decision (no silent `unmatched` at completion); mandatory note on flagging. (§7.5)
7. **Write-off targets and guardrail:** debits → 6980 (or 6900 if fee-worded), credits → 4200; owner or advisor may write off ≤ ₦5,000 (setting), staff never. **Refined by Decision #11 (below): above the limit there is no write-off action at all, for any role — see #11 for the final resolution path.** (§7.5)
8. **Attachments as files on disk, paths in DB** — keeps the SQLite file small enough for the Spec 09 rotating-backup scheme. (§8.2)
9. **Stale outstanding items** (unpresented across two completed sessions) get a prompt, not an auto-action. (§7.6)
10. **Reconciliation-lines schema rework** (§8.1: `state` column + matches join table) supersedes the Spec 01 §3.7 sketch — flagging explicitly since it amends an approved spec, though nothing built on the old shape yet. (§8.1)
11. ✅ **CLOSED, final (2026-07-07) — no in-app bypass of the write-off amount limit, ever, for any role.** Above `writeoff_limit_kobo`, the write-off action is refused unconditionally — not gated by elevation, not offered to an elevated advisor session, not a future feature. Resolution: post an ordinary manual journal entry (Advisor Mode) and match this statement line to it via the existing manual-match flow (§7.3) — no new engine surface required. See the full rationale under §7.5 path 3.

---

*End of Spec 04 — all decisions closed. Next per §11 order: Spec 06 — Excel import/export.*
