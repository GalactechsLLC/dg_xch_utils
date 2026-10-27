-- Custom chains identify the network with a name (up to 64 bytes), a separator,
-- and the full 64-character genesis digest. Keep that complete identity in storage.
ALTER TABLE dg_discovery.budgets DROP CONSTRAINT budgets_network_check;
ALTER TABLE dg_discovery.budgets ADD CONSTRAINT budgets_network_check CHECK(length(network) BETWEEN 1 AND 129);
ALTER TABLE dg_discovery.peers DROP CONSTRAINT peers_network_check;
ALTER TABLE dg_discovery.peers ADD CONSTRAINT peers_network_check CHECK(length(network) BETWEEN 1 AND 129);
COMMENT ON SCHEMA dg_discovery IS 'dgx-discovery-typed-v8';
