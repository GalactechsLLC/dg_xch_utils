-- Keep the last negotiated TLS version alongside endpoint handshake metadata.
ALTER TABLE dg_discovery.peers ADD COLUMN tls_version varchar(32);
COMMENT ON SCHEMA dg_discovery IS 'dgx-discovery-typed-v5';
