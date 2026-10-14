-- Additive upgrade: preserve the existing peer catalog, counters, and leases.
ALTER TABLE dg_discovery.budgets DROP CONSTRAINT budgets_interval_ms_check;
ALTER TABLE dg_discovery.budgets ADD CHECK(interval_ms BETWEEN 1 AND 60000);
ALTER TABLE dg_discovery.budgets
 ADD COLUMN crawler_port integer CHECK(crawler_port BETWEEN 1024 AND 65535),
 ADD COLUMN minimum_height bigint CHECK(minimum_height BETWEEN 0 AND 4294967295),
 ADD COLUMN peak_wait_ms bigint CHECK(peak_wait_ms BETWEEN 100 AND 10000),
 ADD CHECK((crawler_port IS NULL)=(minimum_height IS NULL) AND (crawler_port IS NULL)=(peak_wait_ms IS NULL));
ALTER TABLE dg_discovery.jobs
 ADD COLUMN reliability_weight double precision[] NOT NULL DEFAULT ARRAY[0.,0.,0.,0.,0.]::float8[] CHECK(cardinality(reliability_weight)=5),
 ADD COLUMN reliability_count double precision[] NOT NULL DEFAULT ARRAY[0.,0.,0.,0.,0.]::float8[] CHECK(cardinality(reliability_count)=5),
 ADD COLUMN reliability_value double precision[] NOT NULL DEFAULT ARRAY[0.,0.,0.,0.,0.]::float8[] CHECK(cardinality(reliability_value)=5),
 ADD COLUMN reliability_tries bigint NOT NULL DEFAULT 0 CHECK(reliability_tries>=0),
 ADD COLUMN reliability_successes bigint NOT NULL DEFAULT 0 CHECK(reliability_successes>=0),
 ADD COLUMN reliability_at bigint,
 ADD COLUMN reliable boolean NOT NULL DEFAULT false,
 ADD COLUMN ignore_until bigint NOT NULL DEFAULT 0,
 ADD COLUMN ban_until bigint NOT NULL DEFAULT 0;
COMMENT ON SCHEMA dg_discovery IS 'dgx-discovery-typed-v2';
