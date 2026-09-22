# dg_full_node

Full-node application library: configuration, chain storage, peer transport, synchronization, RPC, and wallet-query services. The executable entry point is `dgx full-node` in `dg_xch_cli`.

## Status

The node validates and stores chain data and exposes authenticated diagnostics. Runtime chain selection defaults to Chia mainnet; custom definitions can remove the prefarm and activate PoS2 from genesis. Fresh-chain candidates follow normal header, body and VDF validation, with explicit bootstrap work sent only to trusted timelords on custom chains. The [Compose acceptance checks](../docker/README.md) exercise integration separately from container health. Linux and macOS are the current node build targets; the GMP dependency prevents Windows MSVC node builds.

## Run

From the repository root, choose an empty development data directory:

```sh
cargo build -p dg_xch_cli --release --features hint
./target/release/dgx --config-dir ./local-node/config init --non-interactive \
  --data-dir ./local-node/data --plots-dir ./local-node/plots
./target/release/dgx --config-dir ./local-node/config full-node \
  --listen 127.0.0.1:8444 \
  --db sqlite://./local-node/data/chain.db \
  --ssl-dir ./local-node/config/ssl \
  --chain-config ./config/chains/dgx.json \
  --genesis-sync
```

Use `--network mainnet` instead of `--chain-config` only when you intend to connect to Chia mainnet. Do not reuse a database between networks. `--print-chain-info` displays the custom chain identity and constants. `--help` lists the storage, worker, memory, and connection settings.

RPC and peer WebSockets share `--listen`; do not configure the old separate RPC port. Private-CA RPC is the default. The node creates its private CA under `--ssl-dir`; distribute only the CA certificate and an appropriately signed client identity to clients, never the CA private key. The local leaf certificate covers localhost/loopback. The [Compose test harness](../docker/README.md) provisions isolated test identities.

For peer discovery add `--introducer HOST:PORT`; advertise a reachable listener using `--advertise IP:PORT` when required. The advertised address must be a literal IP; bracket IPv6 addresses. See the [introducer](../introducer/README.md). Peer discovery never replaces consensus validation. Trusted timelord access is configured with `--trusted-peer` or `--trusted-cidr`; use narrow addresses and never a blanket public-network trust range.

## Storage and wallet queries

The SQLite backend is the default. Build with `coin-index` for standard wallet coin queries, or `hint` for hint-aware wallet queries. PostgreSQL and mmap are optional features. Back up with SQLite-aware tooling or stop the node before copying the database; a live WAL database is not safely backed up by copying only its main file.

The native desktop's Node page calls the authenticated `get_node_details` endpoint. It reports actual local counters and consensus settings; absent live-peer data is not a fabricated zero-value health signal.

## Validation

```sh
cargo test -p dg_full_node --features coin-index
cargo test -p dg_full_node --features hint --test puzzle_state
```

These tests are not proof of a complete new-chain launch or sustained production throughput.

[Repository overview](../readme.md) · [Node engine](../node/README.md) · [Stores](../stores/README.md)
