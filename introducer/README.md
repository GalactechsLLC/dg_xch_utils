# dg_xch_introducer

A small peer-discovery service using the Chia TLS/WebSocket handshake and `RequestPeersIntroducer` protocol. It helps full nodes find each other; it does not validate blocks or act as a central farmer.

## Status

The service accepts registrations from full-node connections on its configured chain. It derives the advertised address from the connection's source IP and handshake port, then checks that endpoint with a TLS-verified full-node handshake before sharing it. It never probes an address supplied in a message body.

Registrations are held in memory, expire, and are rebuilt after a restart. Replies contain at most 100 peers. Connections, message sizes, probes, and registry capacity are bounded. Only one registered endpoint per source IP is retained; multiple public nodes behind one NAT need separate public addresses or another discovery arrangement.

The listener is public and does not treat the public Chia CA as an authentication authority. Its outbound checks use the configured CA bundle and server name. A publicly reachable deployment still needs network-level abuse controls and operational testing.

## Usage

Run from the repository root:

```sh
cargo run -p dg_xch_introducer -- --config /path/to/introducer.json
```

Use the [Docker development stack](../docker/README.md) to generate matching test certificates and configurations automatically. For a standalone instance, provide JSON like this:

```json
{
  "listen": "0.0.0.0:8445",
  "chain": {
    "network_id": "dgx",
    "genesis_seed": "dg_xch/dgx/no-prefarm/v1",
    "rewards": {
      "genesis_pool": 0,
      "genesis_farmer": 0,
      "initial_pool": 1750000000000,
      "initial_farmer": 250000000000,
      "halving_interval": 5045760,
      "max_halvings": 4
    }
  },
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

The identity is also presented when probing full nodes. The current full-node `/ws` route expects a public-network client certificate. `ca_certificate` instead contains the CAs that signed the **server** certificates of nodes you want to introduce. These are separate trust roles. `peer_server_name` must match those server certificates; the current local full-node certificates use `localhost`.

Use the exact same chain definition on all nodes. Point each node at the introducer:

```sh
cargo run -p dg_xch_cli --features coin-index --bin dg -- full-node \
  --listen 0.0.0.0:8444 --db sqlite:///path/to/chain.db \
  --chain-config config/chains/dgx.json --rpc-tls private-ca \
  --ssl-dir /path/to/node-ssl --introducer introducer.example:8445
```

Full nodes register at startup and refresh every 15 minutes, including when they already have peers. They also retry discovery while peer-starved. NAT deployments must expose their advertised listening port. Enable `allow_private_addresses` only for an isolated test or private network; the Docker example enables it on an internal bridge.

## Development

```sh
cargo test -p dg_xch_introducer
cargo test -p dg_xch_servers transport
```

The tests cover address policy, probe bounds, expiry, requester exclusion, and shared message framing. They are not a substitute for a public-network deployment test.
