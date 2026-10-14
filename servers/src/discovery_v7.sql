-- Prior first_source values cannot establish the most recent advertising peer.
ALTER TABLE dg_discovery.peers ADD COLUMN last_gossiped_by varchar(100);
-- Old samples lack these observations; retain NULL rather than fabricate history.
ALTER TABLE dg_discovery.history
 ADD COLUMN total_nodes_5_days bigint CHECK(total_nodes_5_days>=0),
 ADD COLUMN ipv4_nodes_5_days bigint CHECK(ipv4_nodes_5_days>=0),
 ADD COLUMN ipv6_nodes_5_days bigint CHECK(ipv6_nodes_5_days>=0);
COMMENT ON SCHEMA dg_discovery IS 'dgx-discovery-typed-v7';
