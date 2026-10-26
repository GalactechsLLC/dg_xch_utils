-- Observatory controls and bounded historical aggregates; existing observations survive.
ALTER TABLE dg_discovery.budgets
 ADD COLUMN subnet_interval_ms bigint NOT NULL DEFAULT 0 CHECK(subnet_interval_ms BETWEEN 0 AND 60000),
 ADD COLUMN subnet_concurrency integer NOT NULL DEFAULT 256 CHECK(subnet_concurrency BETWEEN 1 AND 256),
 ADD COLUMN verification_concurrency integer NOT NULL DEFAULT 0 CHECK(verification_concurrency BETWEEN 0 AND 64),
 ADD COLUMN adaptive_multiplier integer NOT NULL DEFAULT 1 CHECK(adaptive_multiplier BETWEEN 1 AND 16),
 ADD COLUMN adaptive_baseline double precision,
 ADD COLUMN adaptive_enabled boolean NOT NULL DEFAULT false,
 ADD COLUMN history_days integer NOT NULL DEFAULT 90 CHECK(history_days BETWEEN 1 AND 365);
ALTER TABLE dg_discovery.jobs ADD COLUMN verifying boolean NOT NULL DEFAULT false;
ALTER TABLE dg_discovery.jobs ADD COLUMN subnet cidr GENERATED ALWAYS AS (network(set_masklen(ip,CASE WHEN family(ip)=4 THEN 24 ELSE 64 END))) STORED;
CREATE INDEX jobs_active_subnet ON dg_discovery.jobs(network,role,subnet,lease_until) WHERE lease_token IS NOT NULL;
CREATE TABLE dg_discovery.subnet_cooldowns (
 network text NOT NULL, role text NOT NULL, subnet cidr NOT NULL, next_at timestamptz NOT NULL,
 PRIMARY KEY(network,role,subnet)
);
CREATE TABLE dg_discovery.geography (
 network text NOT NULL, ip inet NOT NULL, country text, asn bigint, organization text,
 database_epoch bigint NOT NULL, updated_at bigint NOT NULL, PRIMARY KEY(network,ip)
);
CREATE TABLE dg_discovery.distribution_history (
 network text NOT NULL, bucket bigint NOT NULL, data jsonb NOT NULL, PRIMARY KEY(network,bucket)
);
CREATE TABLE dg_discovery.node_samples (
 network text NOT NULL, node text NOT NULL, bucket bigint NOT NULL, data jsonb NOT NULL,
 PRIMARY KEY(network,node,bucket)
);
CREATE TABLE dg_discovery.chain_blocks (
 network text NOT NULL, height bigint NOT NULL, hash text NOT NULL, prev_hash text NOT NULL,
 timestamp bigint, data jsonb NOT NULL, PRIMARY KEY(network,height), UNIQUE(network,hash)
);
CREATE TABLE dg_discovery.chain_status (
 network text PRIMARY KEY, updated_at bigint NOT NULL, data jsonb NOT NULL
);
CREATE TABLE dg_discovery.probe_history (
 network text NOT NULL, bucket bigint NOT NULL, completed bigint NOT NULL, handshakes bigint NOT NULL,
 responses bigint NOT NULL, failures bigint NOT NULL, connect_ms double precision, request_ms double precision,
 PRIMARY KEY(network,bucket)
);
COMMENT ON SCHEMA dg_discovery IS 'dgx-discovery-typed-v9';
