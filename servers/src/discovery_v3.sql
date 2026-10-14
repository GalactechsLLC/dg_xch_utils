ALTER TABLE dg_discovery.budgets
 ADD COLUMN tcp_connect_timeout_seconds integer NOT NULL DEFAULT 2
 CHECK(tcp_connect_timeout_seconds BETWEEN 1 AND 60);
COMMENT ON SCHEMA dg_discovery IS 'dgx-discovery-typed-v3';
