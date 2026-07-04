-- Spec 10 §3/§6: license key stored at company creation, unvalidated in v1.
-- Exists from the first shipped build so no future migration touches client data for this.
ALTER TABLE companies ADD COLUMN license_key TEXT;
