# dg_xch_introducer

Peer discovery for Chia full nodes. The introducer checks advertised endpoints before sharing them; it does not validate blocks.

## Install and run

From the repository root:

```sh
cargo install --path cli --locked
dgx init
dgx introducer --config /home/user/.dgx/config/introducer.json
```

Create that JSON file with your certificate paths:

```json
{
  "listen": "0.0.0.0:8445",
  "chain": "mainnet",
  "tls": {
    "certificate": "/path/to/public_introducer.crt",
    "private_key": "/path/to/public_introducer.key",
    "ca_certificate": "/path/to/node-server-ca-bundle.crt"
  },
  "peer_server_name": "localhost",
  "allow_private_addresses": false,
  "max_connections": 64,
  "max_peers": 4096,
  "peer_ttl_seconds": 3600
}
```

The public introducer identity is also used when probing nodes. The CA bundle must trust their **server** certificates, and `peer_server_name` must match them. These are distinct from private RPC credentials.

To have a mainnet node use your introducer:

```sh
dgx full-node --network mainnet --introducer introducer.example:8445
```

Replace the example hostname with your reachable service. Nodes register at startup and refresh every 15 minutes; their advertised listening port must be reachable.

## Behavior

Registrations are kept in memory and expire. Replies contain at most 100 peers, with one endpoint retained per source IP. Multiple nodes behind one NAT need another discovery arrangement. Private addresses are rejected by default.

The service bounds connections, probes, and messages, but a public deployment still needs network-level abuse protection. For embedding, use the configuration and service APIs in `src/lib.rs`.

[Full node](../full-node/README.md) · [Shared transport](../servers/README.md)
