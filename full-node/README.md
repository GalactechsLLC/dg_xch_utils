# dg_full_node

Full-node application library for Chia: storage, peer connections, synchronization, RPC, and wallet queries. Launch it with `dgx full-node`.

## Install and run

From the repository root:

```sh
cargo install --path cli --locked
dgx init
dgx full-node --network mainnet
```

Initialization supplies database and TLS paths. The node uses Chia's mainnet introducer by default. Run `dgx gui` separately to watch synchronization, or use `dgx full-node --help` for connection and resource settings.

Linux and macOS are supported build targets. The GMP dependency currently prevents Windows MSVC full-node builds; Windows users can connect the desktop to another node. Mainnet compatibility and sustained operation still need broader validation.

## Connections and storage

- RPC and peer WebSockets share port **8444** by default. A Chia reference node commonly uses **8555** for RPC.
- RPC uses the private CA and client identities created during setup. Share client credentials, not the CA private key. Remote hostnames must match the server certificate.
- SQLite is the default store. The default CLI installation includes `hint` and `coin-index` for wallets; `postgres` and `mmap` are optional.
- Stop the node before file-copy backups, or use SQLite-aware backup tooling. Never share one database between running nodes or networks.

Use `--introducer HOST:PORT` to select a discovery service and `--advertise IP:PORT` when the reachable address differs from the listener. Timelord access should be local or limited to explicitly trusted peers, never a broad public range.

## Integration

The [node engine](../node/README.md) owns consensus and live state; this package supplies configuration and Portfu routes. Authenticated `get_node_details` serves GUI diagnostics. `get_recent_signage_point_or_eos` reports actual receipt times for cached objects, which pools use for freshness checks.

[CLI](../cli/README.md) · [Stores](../stores/README.md) · [Introducer](../introducer/README.md)
