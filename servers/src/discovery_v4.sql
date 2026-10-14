ALTER TABLE dg_discovery.budgets DROP CONSTRAINT budgets_peak_wait_ms_check;
ALTER TABLE dg_discovery.budgets ADD CHECK(peak_wait_ms BETWEEN 100 AND 120000);
COMMENT ON SCHEMA dg_discovery IS 'dgx-discovery-typed-v4';
