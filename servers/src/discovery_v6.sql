-- Existing installations continue crawling both address families by default.
ALTER TABLE dg_discovery.budgets
 ADD COLUMN crawler_enable_ipv6 boolean NOT NULL DEFAULT true;
COMMENT ON SCHEMA dg_discovery IS 'dgx-discovery-typed-v6';
