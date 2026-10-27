-- First-release schema. Discovery owns this namespace; no chain-store migrations run here.
CREATE SCHEMA dg_discovery;
CREATE TABLE dg_discovery.budgets (
 network text NOT NULL CHECK(length(network) BETWEEN 1 AND 64), role text NOT NULL CHECK(role IN ('crawler','introducer')),
 concurrency integer NOT NULL CHECK(concurrency BETWEEN 1 AND 256), interval_ms bigint NOT NULL CHECK(interval_ms BETWEEN 500 AND 60000),
 timeout_seconds integer NOT NULL CHECK(timeout_seconds BETWEEN 1 AND 60), revisit_seconds bigint NOT NULL CHECK(revisit_seconds>=30),
 cooldown_seconds integer NOT NULL CHECK(cooldown_seconds>=30), allow_private boolean NOT NULL,
 next_start timestamptz NOT NULL DEFAULT now(), maintenance_after timestamptz NOT NULL DEFAULT now(), PRIMARY KEY(network,role)
);
CREATE TABLE dg_discovery.peers (
 network text NOT NULL CHECK(length(network) BETWEEN 1 AND 64), ip inet NOT NULL CHECK(masklen(ip)=CASE WHEN family(ip)=4 THEN 32 ELSE 128 END),
 port integer NOT NULL CHECK(port BETWEEN 1024 AND 65535), family smallint GENERATED ALWAYS AS (family(ip)) STORED,
 publicly_routable boolean NOT NULL,
 endpoint text GENERATED ALWAYS AS ((CASE WHEN family(ip)=6 THEN '['||host(ip)||']' ELSE host(ip) END)||':'||port::text) STORED,
 first_seen bigint NOT NULL CHECK(first_seen>=0), last_seen bigint NOT NULL CHECK(last_seen>=first_seen),first_source varchar(100) NOT NULL,
 first_gossiped bigint,last_gossiped bigint,reported_gossip_timestamp numeric(20,0) CHECK(reported_gossip_timestamp BETWEEN 0 AND 18446744073709551615),
 first_success bigint,last_success bigint,metadata_at bigint,
 node_id bytea CHECK(octet_length(node_id)=32),software_version varchar(128),protocol_version varchar(32),node_type smallint CHECK(node_type BETWEEN 0 AND 255),
 advertised_port integer CHECK(advertised_port BETWEEN 0 AND 65535),capabilities jsonb NOT NULL DEFAULT '[]' CHECK(jsonb_typeof(capabilities)='array' AND jsonb_array_length(capabilities)<=32),
 connect_ms double precision CHECK(connect_ms>=0 AND connect_ms<'Infinity'::float8),
 peer_request_ms double precision CHECK(peer_request_ms>=0 AND peer_request_ms<'Infinity'::float8),last_peer_response bigint,
 peak_height bigint CHECK(peak_height BETWEEN 0 AND 4294967295),peak_observed_at bigint,verified_at bigint,verified_until bigint,
 PRIMARY KEY(network,ip,port)
) PARTITION BY LIST(network);
CREATE TABLE dg_discovery.jobs (
 network text NOT NULL,role text NOT NULL CHECK(role IN ('crawler','introducer')),ip inet NOT NULL,port integer NOT NULL,
 next_attempt bigint NOT NULL CHECK(next_attempt>=0),last_attempt bigint,attempts bigint NOT NULL DEFAULT 0 CHECK(attempts>=0),
 successes bigint NOT NULL DEFAULT 0 CHECK(successes>=0),consecutive_failures integer NOT NULL DEFAULT 0 CHECK(consecutive_failures>=0),
 last_error varchar(256),last_error_phase text CHECK(last_error_phase IN ('unknown','tcp_connect','tls','websocket','handshake','peer_request','verification')),last_error_at bigint,
 lease_token uuid,lease_until bigint,
 CHECK((lease_token IS NULL)=(lease_until IS NULL)),CHECK((last_error IS NULL)=(last_error_phase IS NULL) AND (last_error IS NULL)=(last_error_at IS NULL)),
 PRIMARY KEY(network,role,ip,port),FOREIGN KEY(network,ip,port) REFERENCES dg_discovery.peers(network,ip,port) ON DELETE CASCADE
) PARTITION BY LIST(network);
CREATE TABLE dg_discovery.cooldowns (
 network text NOT NULL,role text NOT NULL CHECK(role IN ('crawler','introducer')),ip inet NOT NULL,next_at bigint NOT NULL,
 PRIMARY KEY(network,role,ip)
) PARTITION BY LIST(network);
-- Each process writes its own counter row; completions do not contend on a global totals row.
CREATE TABLE dg_discovery.counters (
 network text NOT NULL,role text NOT NULL CHECK(role IN ('crawler','introducer')),worker_id uuid NOT NULL,
 attempts bigint NOT NULL DEFAULT 0,connections bigint NOT NULL DEFAULT 0,peer_responses bigint NOT NULL DEFAULT 0,failures bigint NOT NULL DEFAULT 0,
 gossip_received bigint NOT NULL DEFAULT 0,gossip_rejected bigint NOT NULL DEFAULT 0,pruned bigint NOT NULL DEFAULT 0,verification_expired bigint NOT NULL DEFAULT 0,
 connect_seconds_sum double precision NOT NULL DEFAULT 0,request_seconds_sum double precision NOT NULL DEFAULT 0,
 PRIMARY KEY(network,role,worker_id)
) PARTITION BY LIST(network);
CREATE TABLE dg_discovery.observations (
 network text NOT NULL,role text NOT NULL CHECK(role IN ('crawler','introducer')),attempt_id uuid NOT NULL,worker_id uuid NOT NULL,
 ip inet NOT NULL,port integer NOT NULL CHECK(port BETWEEN 1024 AND 65535),started_at bigint NOT NULL,finished_at bigint NOT NULL CHECK(finished_at>=started_at),
 successful boolean NOT NULL,handshake_completed boolean NOT NULL,peer_response boolean NOT NULL,
 connect_ms double precision,peer_request_ms double precision,node_id bytea CHECK(octet_length(node_id)=32),software_version varchar(128),
 error varchar(256),error_phase text CHECK(error_phase IN ('unknown','tcp_connect','tls','websocket','handshake','peer_request','verification')),
 received_peers integer NOT NULL CHECK(received_peers BETWEEN 0 AND 1000),PRIMARY KEY(network,attempt_id)
) PARTITION BY LIST(network);
CREATE TABLE dg_discovery.history (
 network text NOT NULL,bucket bigint NOT NULL,observed_at bigint NOT NULL,known bigint NOT NULL,recent bigint NOT NULL,identities bigint NOT NULL,
 PRIMARY KEY(network,bucket)
) PARTITION BY LIST(network);
-- Mainnet/testnet have independent physical partitions and indexes. Other named test/custom chains stay isolated by network keys.
CREATE TABLE dg_discovery.peers_mainnet PARTITION OF dg_discovery.peers FOR VALUES IN ('mainnet');
CREATE TABLE dg_discovery.peers_testnet11 PARTITION OF dg_discovery.peers FOR VALUES IN ('testnet11');
CREATE TABLE dg_discovery.peers_custom PARTITION OF dg_discovery.peers DEFAULT;
CREATE TABLE dg_discovery.jobs_mainnet PARTITION OF dg_discovery.jobs FOR VALUES IN ('mainnet');
CREATE TABLE dg_discovery.jobs_testnet11 PARTITION OF dg_discovery.jobs FOR VALUES IN ('testnet11');
CREATE TABLE dg_discovery.jobs_custom PARTITION OF dg_discovery.jobs DEFAULT;
CREATE TABLE dg_discovery.cooldowns_mainnet PARTITION OF dg_discovery.cooldowns FOR VALUES IN ('mainnet');
CREATE TABLE dg_discovery.cooldowns_testnet11 PARTITION OF dg_discovery.cooldowns FOR VALUES IN ('testnet11');
CREATE TABLE dg_discovery.cooldowns_custom PARTITION OF dg_discovery.cooldowns DEFAULT;
CREATE TABLE dg_discovery.counters_mainnet PARTITION OF dg_discovery.counters FOR VALUES IN ('mainnet');
CREATE TABLE dg_discovery.counters_testnet11 PARTITION OF dg_discovery.counters FOR VALUES IN ('testnet11');
CREATE TABLE dg_discovery.counters_custom PARTITION OF dg_discovery.counters DEFAULT;
CREATE TABLE dg_discovery.history_mainnet PARTITION OF dg_discovery.history FOR VALUES IN ('mainnet');
CREATE TABLE dg_discovery.history_testnet11 PARTITION OF dg_discovery.history FOR VALUES IN ('testnet11');
CREATE TABLE dg_discovery.history_custom PARTITION OF dg_discovery.history DEFAULT;
CREATE TABLE dg_discovery.observations_mainnet PARTITION OF dg_discovery.observations FOR VALUES IN ('mainnet');
CREATE TABLE dg_discovery.observations_testnet11 PARTITION OF dg_discovery.observations FOR VALUES IN ('testnet11');
CREATE TABLE dg_discovery.observations_custom PARTITION OF dg_discovery.observations DEFAULT;
CREATE INDEX observations_time ON dg_discovery.observations(network,role,finished_at DESC);
CREATE INDEX observations_endpoint ON dg_discovery.observations(network,ip,port,finished_at DESC);
CREATE INDEX jobs_due ON dg_discovery.jobs(network,role,next_attempt,ip,port);
CREATE INDEX jobs_leases ON dg_discovery.jobs(network,role,lease_until) WHERE lease_token IS NOT NULL;
CREATE INDEX jobs_errors ON dg_discovery.jobs(network,role,last_error_phase,ip,port) WHERE last_error IS NOT NULL;
CREATE INDEX jobs_attempt_desc ON dg_discovery.jobs(network,role,last_attempt DESC NULLS LAST,ip,port);
CREATE INDEX jobs_attempt_asc ON dg_discovery.jobs(network,role,last_attempt ASC NULLS LAST,ip,port);
CREATE INDEX jobs_failures ON dg_discovery.jobs(network,role,consecutive_failures DESC,ip,port);
CREATE INDEX cooldowns_expiry ON dg_discovery.cooldowns(network,role,next_at);
CREATE INDEX peers_connected_desc ON dg_discovery.peers(network,last_success DESC NULLS LAST,ip,port);
CREATE INDEX peers_connected_asc ON dg_discovery.peers(network,last_success ASC NULLS LAST,ip,port);
CREATE INDEX peers_version_desc ON dg_discovery.peers(network,software_version DESC NULLS LAST,ip,port);
CREATE INDEX peers_version_asc ON dg_discovery.peers(network,software_version ASC NULLS LAST,ip,port);
CREATE INDEX peers_ping_desc ON dg_discovery.peers(network,peer_request_ms DESC NULLS LAST,ip,port);
CREATE INDEX peers_ping_asc ON dg_discovery.peers(network,peer_request_ms ASC NULLS LAST,ip,port);
CREATE INDEX peers_seen ON dg_discovery.peers(network,first_seen DESC,ip,port);
CREATE INDEX peers_identity ON dg_discovery.peers(network,node_id,last_success) WHERE node_id IS NOT NULL;
CREATE INDEX peers_verified_time ON dg_discovery.peers(network,verified_until,ip,port) WHERE verified_until IS NOT NULL;
CREATE INDEX peers_verified_rotation ON dg_discovery.peers(network,ip,port) INCLUDE(verified_at,verified_until,publicly_routable) WHERE verified_until IS NOT NULL;
CREATE INDEX peers_retention ON dg_discovery.peers(network,last_seen,ip,port);
COMMENT ON SCHEMA dg_discovery IS 'dgx-discovery-first-release-typed-v1';
