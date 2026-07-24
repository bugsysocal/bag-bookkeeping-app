-- Spec 07 §2.3/§5: the Compliance banner ("threshold crossings") is
-- non-editable by the owner and advisor-clearable only. Clearing without
-- changing the underlying vat_exempt/cit_exempt flag means "reviewed, no
-- change needed for now" — recorded as the fiscal-year-start date it was
-- acknowledged for, so a new fiscal year always re-surfaces the check
-- (reminders follow the fiscal year, not the calendar year, per the
-- project's standing rule). NULL = never acknowledged.
ALTER TABLE companies ADD COLUMN vat_threshold_acked_fy_start TEXT;
ALTER TABLE companies ADD COLUMN cit_threshold_acked_fy_start TEXT;
