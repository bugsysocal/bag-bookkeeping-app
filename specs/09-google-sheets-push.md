# Specification 09 — Google Sheets One-Way Push
**Project:** LedgerOne (placeholder) · **Covers:** Planning doc §6.3, §6.4, §11 item 10 · **Status:** APPROVED v1.0 (2026-07-03; #1 and #4 individually confirmed; #5 amended per review: Sales by Customer tab added for the customer-concentration diagnostic; rest batch-approved). **This approval completes the specification phase.**
**Depends on:** Specs 01–08. **Roadmap note:** §11 puts this spec last in the *specification* order, but the roadmap (§9) places implementation in **Phase 2** — this document exists now so Phase 1 code leaves the right seams, and so the spec phase is complete before scaffolding. Nothing in Phase 1 ships Sheets code.

---

## 1. Scope & The One Constraint

On-demand or scheduled **one-way push** of key registers to a linked Google Sheet: the advisor-review motion (planning doc §6.4 — client runs the desktop app; the practice reviews a synced sheet/dashboard without touching their machine).

**Two-way sync is out of scope permanently at this layer, not deferred** (planning doc §6.3: conflict resolution between an editable sheet and a double-entry ledger is a correctness minefield). If remote *entry* is ever needed, the sanctioned path is a Phase 3 companion capture form posting through the Spec 01 engine — never sheet reads. This spec treats one-way as an architectural constraint, and §3's design makes violating it structurally pointless (every push overwrites).

## 2. What Gets Pushed (tab set)

One spreadsheet per company, created and owned by the app: **"LedgerOne — {Company Name}"**. Tabs:

| Tab | Content | Source |
|---|---|---|
| `Read Me` | do-not-edit notice, company, connection status, per-tab push timestamps, push log (last 50) | this spec |
| `Dashboard` | the five Spec 07 tiles + unremitted-tax line, as of push time | Spec 07 §2.1 |
| `Invoices` | invoice register (open + last 12 months) | Spec 03 |
| `Payments` | payments register, both directions | Spec 03/04 |
| `Expenses & Bills` | combined register | Spec 04 |
| `Aging` | AR and AP aging by contact | Spec 05 §3.2 |
| `Sales by Customer` | period + trailing-12-month view per customer | Spec 05 §3.4 — added per review: the advisor's monthly customer-concentration diagnostic (dangerous dependence on one or two accounts; quiet decline of a previously strong customer), reviewable remotely |
| `Trial Balance` | as of push time | Spec 05 §4.5 |
| `VAT & WHT` | current + prior month VAT summary; WHT remittance/credit summaries | Spec 05 §5 |

**The single-computation-path rule (the key correctness decision):** push code contains **no queries of its own against raw ledger data**. Every tab serializes the output of the corresponding Spec 05/07 report function — the same code path that renders the screen and the Excel export. A number on the sheet is the number in the app, by construction; there is no second implementation of the trial balance or the VAT summary to drift out of agreement. This is designed specifically so the push layer never touches how a regulatory-facing figure is computed. *(Decision #1)*

## 3. Push Mechanics

- **Full-tab overwrite, every push.** Each tab is cleared and rewritten wholesale (one batch update per tab). Idempotent, self-healing, and it makes the one-way contract physical: any edit made in the sheet is gone on the next push. The `Read Me` tab and a banner row on every tab say exactly that: *"Refreshed by LedgerOne — do not edit; changes here will be overwritten and never read."* *(Decision #2)*
- **Consistency:** each push reads all report outputs inside one snapshot (single read transaction), so tabs never disagree with each other about the as-of moment; the timestamp is stamped on every tab.
- **Triggers:** manual (**Push to Google Sheet** button) always; optional daily schedule (fires when app is running and online — offline-first principle 1: sync is additive, never required, never blocking). Failures show a status line in Settings ("Last push failed — will retry when online"), no banner unless pushes have been failing > 7 days *with the schedule enabled* (then the Spec 07 banner-zone slot 5, dismissible).
- **Volume:** SME scale is trivially within Sheets API batch limits; one spreadsheet, eight tabs, one batchUpdate per tab.

## 4. Auth, Privacy & Token Handling

- **Google OAuth**, minimal scopes: `spreadsheets` + `drive.file` (the app can touch **only files it created** — it cannot see, list, or read anything else in the owner's Drive). *(Decision #3)*
- **Tokens live in the OS credential store (Windows DPAPI/Credential Manager), never in SQLite.** Deliberate interlock with Spec 08: the database is backed up, copied to USB drives, and restored onto other machines — credentials must never ride along. A restored database on a new machine has a disconnected Sheets state, which is the correct behavior. *(Decision #4)*
- **Consent surface:** the connect flow lists in plain language exactly which tabs will be shared and with whom the sheet's link is subsequently shared is the *owner's* action in Google, not the app's. Disconnect is one click (revokes token, leaves the sheet in place — it's the owner's file).
- Per-company link state (`spreadsheet_id`, `last_push_at`, schedule flag) is book-level data → small table (§5).

## 5. Deltas (additive)

```sql
CREATE TABLE sheets_push_state (
  company_id     TEXT PRIMARY KEY REFERENCES companies(id),
  spreadsheet_id TEXT,                    -- Google file id; NULL = never connected
  schedule       TEXT NOT NULL DEFAULT 'manual' CHECK (schedule IN ('manual','daily')),
  last_push_at   TEXT,
  last_result    TEXT                     -- 'ok' | error summary, display only
);
```
No engine changes; no report changes (Decision #1 forbids them by design). Tokens: no schema — OS store only.

## 6. Decisions needing your sign-off

Standing-rule audit: #1 is the only decision that even approaches regulatory numbers, and its entire content is that the push layer **must not** have its own computation path for them — it mandates reuse of the approved Spec 05 report functions. Calling it out first because it concerns the *transport* of filing-facing figures, even though its direction is the safe one. The rest are mechanics.

1. **Single computation path** — tabs serialize Spec 05/07 report outputs verbatim; push code never queries raw ledger data. (§2)
2. **Full-tab overwrite** every push; do-not-edit banner on every tab; sheet edits are overwritten and never read. (§3)
3. **Minimal scopes** (`spreadsheets` + `drive.file` only). (§4)
4. **Tokens in the OS credential store, never in SQLite** — so backups/restores never carry credentials. (§4)
5. ✅ **Tab set — RESOLVED 2026-07-03: Sales by Customer added** per reviewer (customer-concentration is a recurring monthly diagnostic for the advisory review motion; pushing it enables remote spotting without machine access). Tab set now final as listed in §2. (§2)
6. **Failure surfacing:** quiet status line; banner only after 7 days of scheduled-push failure. (§3)
7. **Spec now, build Phase 2** — Phase 1 ships no Sheets code; this spec exists so Phase 1 leaves the correct seams (report functions already serializable per Decision #1) and the spec phase closes complete. (header note)

---

*End of Spec 09 — the final specification. On sign-off, the spec phase is complete: next is the consolidated key-decisions summary, then scaffolding begins (Spec 01 §5.1 stack: Tauri + Rust + SQLite).*
