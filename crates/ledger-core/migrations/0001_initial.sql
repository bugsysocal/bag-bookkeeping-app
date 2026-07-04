-- LedgerOne migration 0001 — consolidated schema
-- Normative sources: Spec 01 v0.3 (core + triggers), Spec 02 (company columns),
-- Spec 03 §8 (invoice/payment columns, document_deliveries), Spec 04 §8 (reconciliation
-- shape, attachments, recurring, import profiles, write-off settings), Spec 06 §6
-- (import_batches), Spec 07 §8 (advisor timeout, user_prefs), Spec 09 §5 (sheets_push_state).
-- Conventions (Spec 01 §2): money INTEGER kobo; journal lines signed (+Dr/−Cr); TEXT ULIDs;
-- ISO-8601 dates; ledger always NGN, FX as metadata.

-- ===== 3.1 Companies, users =====

CREATE TABLE companies (
  id                       TEXT PRIMARY KEY,
  name                     TEXT NOT NULL,
  base_currency            TEXT NOT NULL DEFAULT 'NGN',
  tin                      TEXT,
  vat_registered           INTEGER NOT NULL DEFAULT 1,  -- operational: engine charges/claims VAT iff 1
  -- Two INDEPENDENT statutory reliefs (NTA 2025) — they do not move together (Spec 01 §9):
  vat_exempt               INTEGER NOT NULL DEFAULT 0,  -- small business: turnover <= N50M
  cit_exempt               INTEGER NOT NULL DEFAULT 0,  -- small company: <= N100M AND assets <= N250M;
                                                        -- gates the WHT deduction exemption (Spec 01 §6.2)
  vat_rate_bp              INTEGER NOT NULL DEFAULT 750,
  inventory_enabled        INTEGER NOT NULL DEFAULT 0,
  soft_close_through       TEXT,                        -- period LOCK, not closing entries (Spec 05 §4.3)
  hard_close_through       TEXT,
  fiscal_year_start_month  INTEGER NOT NULL DEFAULT 1 CHECK (fiscal_year_start_month BETWEEN 1 AND 12),
  business_type            TEXT NOT NULL DEFAULT 'mixed'
                           CHECK (business_type IN ('trading','services','manufacturing','agency','mixed')),
  writeoff_limit_kobo      INTEGER NOT NULL DEFAULT 500000,  -- N5,000; Advisor Mode setting (Spec 04 #7)
  writeoff_debit_account_id  TEXT REFERENCES accounts(id),   -- seeded -> 6980
  writeoff_credit_account_id TEXT REFERENCES accounts(id),   -- seeded -> 4200
  advisor_timeout_minutes  INTEGER NOT NULL DEFAULT 15,
  created_at               TEXT NOT NULL
);

CREATE TABLE users (
  id         TEXT PRIMARY KEY,
  company_id TEXT NOT NULL REFERENCES companies(id),
  name       TEXT NOT NULL,
  role       TEXT NOT NULL CHECK (role IN ('owner','staff','advisor')),
  pin_hash   TEXT,
  created_at TEXT NOT NULL
);

-- ===== 3.2 Chart of accounts =====

CREATE TABLE accounts (
  id         TEXT PRIMARY KEY,
  company_id TEXT NOT NULL REFERENCES companies(id),
  code       TEXT NOT NULL,
  name       TEXT NOT NULL,
  class      TEXT NOT NULL CHECK (class IN ('asset','liability','equity','income','cogs','expense')),
  system_key TEXT,                 -- engine lookup handle; NULL for user accounts
  is_bank    INTEGER NOT NULL DEFAULT 0,
  is_system  INTEGER NOT NULL DEFAULT 0,
  is_active  INTEGER NOT NULL DEFAULT 1,
  UNIQUE (company_id, code),
  UNIQUE (company_id, system_key)
);

-- ===== 3.3 Contacts & products =====

CREATE TABLE contacts (
  id                 TEXT PRIMARY KEY,
  company_id         TEXT NOT NULL REFERENCES companies(id),
  kind               TEXT NOT NULL CHECK (kind IN ('customer','supplier','both')),
  name               TEXT NOT NULL,
  phone              TEXT,
  email              TEXT,
  tin                TEXT,          -- gates WHT deduction exemption; no TIN => 2x rate warned
  address            TEXT,
  payment_terms_days INTEGER NOT NULL DEFAULT 0,
  is_active          INTEGER NOT NULL DEFAULT 1,
  created_at         TEXT NOT NULL
);
-- Opening balances are journal entries against 3000 OBE, never columns (Spec 01 §3.3).

CREATE TABLE products (
  id                TEXT PRIMARY KEY,
  company_id        TEXT NOT NULL REFERENCES companies(id),
  kind              TEXT NOT NULL CHECK (kind IN ('product','service')),
  name              TEXT NOT NULL,
  sku               TEXT,
  sale_price_kobo   INTEGER NOT NULL DEFAULT 0,
  is_vatable        INTEGER NOT NULL DEFAULT 1,
  track_inventory   INTEGER NOT NULL DEFAULT 0,
  income_account_id TEXT REFERENCES accounts(id),
  cogs_account_id   TEXT REFERENCES accounts(id),
  is_active         INTEGER NOT NULL DEFAULT 1
);

-- ===== 3.4 Documents =====

CREATE TABLE document_sequences (
  company_id  TEXT NOT NULL REFERENCES companies(id),
  doc_type    TEXT NOT NULL CHECK (doc_type IN ('invoice','quote','receipt')),
  prefix      TEXT NOT NULL,
  next_number INTEGER NOT NULL DEFAULT 1,
  PRIMARY KEY (company_id, doc_type)
);

CREATE TABLE invoices (
  id                TEXT PRIMARY KEY,
  company_id        TEXT NOT NULL REFERENCES companies(id),
  contact_id        TEXT NOT NULL REFERENCES contacts(id),
  number            TEXT NOT NULL,
  kind              TEXT NOT NULL DEFAULT 'invoice' CHECK (kind IN ('invoice','quote')),
  status            TEXT NOT NULL DEFAULT 'draft' CHECK (status IN
                    ('draft','sent','partially_paid','paid','void','converted')),
                    -- 'converted' reachable by quotes only (app-enforced, Spec 03 §8);
                    -- 'overdue' is DERIVED, never stored (Spec 03 §2)
  issue_date        TEXT NOT NULL,
  due_date          TEXT NOT NULL,
  currency          TEXT NOT NULL DEFAULT 'NGN',   -- v1: NGN-only invoicing (Spec 03 V6)
  subtotal_kobo     INTEGER NOT NULL,
  vat_kobo          INTEGER NOT NULL,
  total_kobo        INTEGER NOT NULL,
  amount_paid_kobo  INTEGER NOT NULL DEFAULT 0,    -- display cache; truth = payment_allocations
  notes             TEXT,
  sent_at           TEXT,
  converted_from_id TEXT REFERENCES invoices(id),  -- quote->invoice / void->reissue provenance
  journal_entry_id  TEXT REFERENCES journal_entries(id),  -- NULL while draft/quote/zero-total-no-stock
  voided_by_entry   TEXT REFERENCES journal_entries(id),
  created_by        TEXT REFERENCES users(id),
  created_at        TEXT NOT NULL,
  UNIQUE (company_id, number)
);

CREATE TABLE invoice_lines (
  id                TEXT PRIMARY KEY,
  invoice_id        TEXT NOT NULL REFERENCES invoices(id),
  line_no           INTEGER NOT NULL,
  product_id        TEXT REFERENCES products(id),
  description       TEXT NOT NULL,
  quantity_milli    INTEGER NOT NULL,
  unit_price_kobo   INTEGER NOT NULL,
  discount_bp       INTEGER NOT NULL DEFAULT 0,
  vat_applied       INTEGER NOT NULL,
  net_kobo          INTEGER NOT NULL,   -- computed once at posting; immutable after
  vat_kobo          INTEGER NOT NULL,
  income_account_id TEXT NOT NULL REFERENCES accounts(id)
);

CREATE TABLE bills (
  id               TEXT PRIMARY KEY,
  company_id       TEXT NOT NULL REFERENCES companies(id),
  contact_id       TEXT NOT NULL REFERENCES contacts(id),
  reference        TEXT,
  status           TEXT NOT NULL DEFAULT 'draft' CHECK (status IN
                   ('draft','open','partially_paid','paid','void')),
  bill_date        TEXT NOT NULL,
  due_date         TEXT NOT NULL,
  currency         TEXT NOT NULL DEFAULT 'NGN',
  subtotal_kobo    INTEGER NOT NULL,
  vat_kobo         INTEGER NOT NULL,
  total_kobo       INTEGER NOT NULL,
  wht_applicable   INTEGER NOT NULL DEFAULT 0,  -- split happens at PAYMENT (Spec 01 §6.2)
  wht_rate_bp      INTEGER,
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
  vat_claimable      INTEGER NOT NULL DEFAULT 1,  -- NTA 2025 §155(4): default claimable (Spec 01 §6.4)
  net_kobo           INTEGER NOT NULL,
  vat_kobo           INTEGER NOT NULL,
  expense_account_id TEXT NOT NULL REFERENCES accounts(id)
);

CREATE TABLE wht_rate_presets (
  id         TEXT PRIMARY KEY,
  company_id TEXT NOT NULL REFERENCES companies(id),
  label      TEXT NOT NULL,
  rate_bp    INTEGER NOT NULL
);

-- ===== 3.5 Payments =====

CREATE TABLE payments (
  id               TEXT PRIMARY KEY,
  company_id       TEXT NOT NULL REFERENCES companies(id),
  direction        TEXT NOT NULL CHECK (direction IN ('in','out')),
  contact_id       TEXT REFERENCES contacts(id),
  bank_account_id  TEXT NOT NULL REFERENCES bank_accounts(id),
  payment_date     TEXT NOT NULL,
  amount_kobo      INTEGER NOT NULL CHECK (amount_kobo > 0),
  wht_kobo         INTEGER NOT NULL DEFAULT 0,  -- in: withheld BY customer; out: withheld FROM supplier
  method           TEXT CHECK (method IN ('transfer','cash','pos','cheque','other')),
  reference        TEXT,
  receipt_number   TEXT,                        -- RCT sequence, direction='in' only (Spec 03 §8)
  voided           INTEGER NOT NULL DEFAULT 0,
  journal_entry_id TEXT NOT NULL REFERENCES journal_entries(id),
  created_by       TEXT REFERENCES users(id),
  created_at       TEXT NOT NULL
);

CREATE TABLE payment_allocations (
  id          TEXT PRIMARY KEY,
  payment_id  TEXT NOT NULL REFERENCES payments(id),
  target_type TEXT NOT NULL CHECK (target_type IN ('invoice','bill')),
  target_id   TEXT NOT NULL,
  amount_kobo INTEGER NOT NULL CHECK (amount_kobo > 0)
);

-- ===== 3.6 The ledger (heart of the schema) =====

CREATE TABLE journal_entries (
  id                   TEXT PRIMARY KEY,
  company_id           TEXT NOT NULL REFERENCES companies(id),
  entry_date           TEXT NOT NULL,
  memo                 TEXT NOT NULL,
  source_type          TEXT NOT NULL CHECK (source_type IN
                       ('invoice','payment','expense','bill','transfer','drawing','capital',
                        'manual','reversal','opening_balance','inventory_adjustment',
                        'deposit_application','reconciliation_writeoff')),
  source_id            TEXT,
  is_posted            INTEGER NOT NULL DEFAULT 0,
  reverses_entry_id    TEXT REFERENCES journal_entries(id),
  reversed_by_entry_id TEXT REFERENCES journal_entries(id),
  created_by           TEXT REFERENCES users(id),
  created_at           TEXT NOT NULL,
  posted_at            TEXT
);

CREATE TABLE journal_lines (
  id             TEXT PRIMARY KEY,
  entry_id       TEXT NOT NULL REFERENCES journal_entries(id),
  line_no        INTEGER NOT NULL,
  account_id     TEXT NOT NULL REFERENCES accounts(id),
  amount_kobo    INTEGER NOT NULL CHECK (amount_kobo != 0),  -- +debit / -credit, ALWAYS NGN
  contact_id     TEXT REFERENCES contacts(id),               -- REQUIRED on AR/AP lines (P8, app-enforced)
  memo           TEXT,
  fx_currency    TEXT,
  fx_amount_kobo INTEGER
);

CREATE INDEX idx_jl_account ON journal_lines(account_id);
CREATE INDEX idx_jl_entry   ON journal_lines(entry_id);
CREATE INDEX idx_jl_contact ON journal_lines(contact_id) WHERE contact_id IS NOT NULL;
CREATE INDEX idx_je_date    ON journal_entries(company_id, entry_date);

-- ===== 3.7 Banking & reconciliation =====

CREATE TABLE bank_accounts (
  id                   TEXT PRIMARY KEY,
  company_id           TEXT NOT NULL REFERENCES companies(id),
  account_id           TEXT NOT NULL UNIQUE REFERENCES accounts(id),
  label                TEXT NOT NULL,
  kind                 TEXT NOT NULL CHECK (kind IN ('bank','cash','pos_wallet','domiciliary')),
  currency             TEXT NOT NULL DEFAULT 'NGN',
  bank_name            TEXT,
  account_number_last4 TEXT,
  last_reconciled_date TEXT,
  is_active            INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE fx_rates (
  id         TEXT PRIMARY KEY,
  company_id TEXT NOT NULL REFERENCES companies(id),
  currency   TEXT NOT NULL,
  rate_micro INTEGER NOT NULL,
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
                         CHECK (status IN ('in_progress','completed','completed_with_exceptions')),
  completed_at           TEXT,
  created_at             TEXT NOT NULL
);

-- Shape per approved Spec 04 #10: workflow state machine on the statement line,
-- incl. the needs-review quarantine (no Suspense account exists, by design).
CREATE TABLE reconciliation_lines (
  id                TEXT PRIMARY KEY,
  reconciliation_id TEXT NOT NULL REFERENCES reconciliations(id),
  stmt_date         TEXT NOT NULL,
  stmt_description  TEXT,              -- verbatim from import; matching evidence, never rewritten
  stmt_amount_kobo  INTEGER NOT NULL,  -- signed: + credit to bank, - debit
  state             TEXT NOT NULL DEFAULT 'unmatched' CHECK (state IN
                    ('unmatched','matched','entry_created','needs_review','written_off','import_error')),
  match_kind        TEXT CHECK (match_kind IN ('auto','manual','created')),
  import_hash       TEXT NOT NULL,     -- SHA-256(bank_account_id, date, amount, desc, occurrence_index)
  review_note       TEXT,              -- mandatory when needs_review (app-enforced); append-only thread
  flagged_by        TEXT REFERENCES users(id),
  flagged_at        TEXT,
  resolved_by       TEXT REFERENCES users(id),
  resolved_at       TEXT,
  resolution        TEXT CHECK (resolution IN ('matched','entry_created','written_off','import_error')),
  carried_from_id   TEXT REFERENCES reconciliation_lines(id)
);
-- Dedup: unique on hash for ORIGINAL lines only — carried-forward copies legitimately share it.
CREATE UNIQUE INDEX idx_rl_import_hash ON reconciliation_lines(import_hash)
  WHERE carried_from_id IS NULL;

CREATE TABLE reconciliation_matches (   -- 1:N and N:1, sum-exact (Spec 04 §7.3)
  reconciliation_line_id TEXT NOT NULL REFERENCES reconciliation_lines(id),
  journal_line_id        TEXT NOT NULL REFERENCES journal_lines(id),
  PRIMARY KEY (reconciliation_line_id, journal_line_id)
);

-- ===== 3.8 Inventory, audit, support tables =====

CREATE TABLE inventory_movements (
  id               TEXT PRIMARY KEY,
  company_id       TEXT NOT NULL REFERENCES companies(id),
  product_id       TEXT NOT NULL REFERENCES products(id),
  movement_date    TEXT NOT NULL,
  kind             TEXT NOT NULL CHECK (kind IN ('opening','purchase','sale','adjustment','reversal')),
  quantity_milli   INTEGER NOT NULL,   -- signed: + in, - out
  unit_cost_kobo   INTEGER NOT NULL,
  total_cost_kobo  INTEGER NOT NULL,
  journal_entry_id TEXT REFERENCES journal_entries(id),
  created_at       TEXT NOT NULL
);

CREATE TABLE audit_log (
  id          TEXT PRIMARY KEY,
  company_id  TEXT NOT NULL,
  user_id     TEXT,
  action      TEXT NOT NULL,
  entity_type TEXT NOT NULL,
  entity_id   TEXT NOT NULL,
  detail_json TEXT,
  created_at  TEXT NOT NULL
);

CREATE TABLE document_deliveries (
  id         TEXT PRIMARY KEY,
  company_id TEXT NOT NULL REFERENCES companies(id),
  doc_type   TEXT NOT NULL CHECK (doc_type IN ('invoice','quote','receipt')),
  doc_id     TEXT NOT NULL,
  channel    TEXT NOT NULL CHECK (channel IN ('whatsapp','email','pdf_export','print')),
  recipient  TEXT,
  created_at TEXT NOT NULL
);

CREATE TABLE attachments (
  id          TEXT PRIMARY KEY,
  company_id  TEXT NOT NULL REFERENCES companies(id),
  entity_type TEXT NOT NULL CHECK (entity_type IN ('bill','payment','journal_entry')),
  entity_id   TEXT NOT NULL,
  filename    TEXT NOT NULL,
  stored_path TEXT NOT NULL,   -- files on disk, never blobbed (Spec 04 #8)
  created_by  TEXT REFERENCES users(id),
  created_at  TEXT NOT NULL
);

CREATE TABLE recurring_templates (
  id            TEXT PRIMARY KEY,
  company_id    TEXT NOT NULL REFERENCES companies(id),
  kind          TEXT NOT NULL CHECK (kind IN ('expense','bill')),
  template_json TEXT NOT NULL,
  frequency     TEXT NOT NULL CHECK (frequency IN ('weekly','monthly','quarterly','yearly')),
  day_of_month  INTEGER,
  next_due      TEXT NOT NULL,
  is_active     INTEGER NOT NULL DEFAULT 1,
  created_at    TEXT NOT NULL
);

CREATE TABLE bank_import_profiles (
  id              TEXT PRIMARY KEY,
  company_id      TEXT NOT NULL REFERENCES companies(id),
  bank_account_id TEXT NOT NULL REFERENCES bank_accounts(id),
  label           TEXT NOT NULL,
  mapping_json    TEXT NOT NULL,
  date_format     TEXT NOT NULL,
  sign_convention TEXT NOT NULL,
  header_rows     INTEGER NOT NULL DEFAULT 0,
  created_at      TEXT NOT NULL
);

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

CREATE TABLE sheets_push_state (
  company_id     TEXT PRIMARY KEY REFERENCES companies(id),
  spreadsheet_id TEXT,
  schedule       TEXT NOT NULL DEFAULT 'manual' CHECK (schedule IN ('manual','daily')),
  last_push_at   TEXT,
  last_result    TEXT
);
-- OAuth tokens: OS credential store ONLY, never this database (Spec 09 #4).

CREATE TABLE user_prefs (
  user_id    TEXT NOT NULL REFERENCES users(id),
  pref_key   TEXT NOT NULL,
  pref_value TEXT,
  PRIMARY KEY (user_id, pref_key)
);
-- UI state only; nothing financial may ever live here (Spec 07 §8).

-- ===== Spec 01 §4 — Integrity triggers (the core promises, structurally) =====

-- T1: entries must balance and have >= 2 lines to post
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

-- T4: posted journal entries are NEVER deleted
CREATE TRIGGER trg_je_no_delete BEFORE DELETE ON journal_entries
WHEN OLD.is_posted = 1
BEGIN SELECT RAISE(ABORT, 'posted entries cannot be deleted; post a reversal'); END;

-- T5: hard close — no posting into a hard-closed period
CREATE TRIGGER trg_je_hard_close BEFORE UPDATE OF is_posted ON journal_entries
WHEN NEW.is_posted = 1 AND OLD.is_posted = 0 AND NEW.entry_date <=
     (SELECT COALESCE(hard_close_through, '0000-00-00') FROM companies WHERE id = NEW.company_id)
BEGIN SELECT RAISE(ABORT, 'period is hard-closed'); END;

-- T6/T7: audit log is append-only
CREATE TRIGGER trg_audit_no_update BEFORE UPDATE ON audit_log
BEGIN SELECT RAISE(ABORT, 'audit log is append-only'); END;
CREATE TRIGGER trg_audit_no_delete BEFORE DELETE ON audit_log
BEGIN SELECT RAISE(ABORT, 'audit log is append-only'); END;

-- T8: account class is immutable; system accounts cannot be deactivated or re-keyed
CREATE TRIGGER trg_acct_lock BEFORE UPDATE ON accounts
BEGIN
  SELECT RAISE(ABORT, 'account class is immutable') WHERE NEW.class != OLD.class;
  SELECT RAISE(ABORT, 'system accounts cannot be deactivated or re-keyed')
  WHERE OLD.is_system = 1 AND (NEW.is_active = 0 OR NEW.system_key IS NOT OLD.system_key);
END;
