-- Spec 06 §3.1: a migrated open invoice/bill may already be partially
-- collected/paid under the PRIOR system, with no payment_allocations row to
-- back it (that payment happened outside this ledger's history). Without
-- this column, recompute_invoice_status/recompute_bill_status would derive
-- amount_paid_kobo PURELY from payment_allocations + deposit_applications
-- and silently erase the pre-migration collected amount the moment any new
-- in-app payment touches the document. imported_paid_kobo is a genuine
-- historical fact captured once at import time — like total_kobo, it is
-- stored, not derived, and never changes after creation.
ALTER TABLE invoices ADD COLUMN imported_paid_kobo INTEGER NOT NULL DEFAULT 0;
ALTER TABLE bills    ADD COLUMN imported_paid_kobo INTEGER NOT NULL DEFAULT 0;
