# Running a dg_xch Full Node

## Build

Install stable Rust, `cmake`, a C compiler, and the system zstd development package. Then build the node:

```bash
cargo build --release -p dg_xch_cli --bin dg --features sqlite,coin-index,hint
```

The binary is written to `target/release/dg`.

## Start

```bash
mkdir -p "$HOME/dg-xch-data"
./target/release/dg full-node \
  --listen 0.0.0.0:8444 \
  --db "sqlite://$HOME/dg-xch-data/chain.db" \
  --network mainnet
```

Use one or more `--peer host:port` options or an `--introducer host:port` to establish outbound connections. Use `--advertise ip:port` only when the listener is reachable from the public network.

## Unified Portfu TLS

Portfu owns the single `--listen` socket and serves the Chia peer WebSocket, RPC, health,
metrics, and operational WebSockets on it. Peer clients connect to `/ws` with a certificate rooted
at the Chia network CA. RPC and operational routes require a certificate rooted at the private CA
under `<ssl-dir>/ca`; it is generated on first start when missing. `/health` and `/metrics` remain
public. The deprecated `--rpc` flag does not create a second listener and, when supplied, must equal
`--listen`.

The configured `--listen` address is always honored; Portfu never rewrites it to loopback. The
default `--rpc-tls private-ca` mode is appropriate for a public listener. Development mode
`--rpc-tls local` accepts Chia-CA client certificates for protected routes and therefore does not
provide private administrative isolation.

## Peer Settings

The `dg full-node` command exposes every `P2pSettings` value:

- `--target-outbound` and `--target-peer-count`
- `--host-pool-capacity`, `--address-lower`, and `--address-upper`
- `--connect-timeout-secs`, `--handshake-timeout-secs`, and `--retry-timeout-secs`
- `--heartbeat-secs`, `--pong-deadline-secs`, and `--recent-peer-threshold-secs`
- `--jitter-floor`

Invalid combinations are rejected during startup. In particular, outbound peers cannot exceed total peers, address bounds must fit within the host pool, durations must be nonzero, and jitter must be between `0.0` and `1.0`.

The follow-sync fetch width now uses both standard V3 request slots for each
`--target-outbound` connection; raising the outbound target therefore raises useful network
concurrency instead of leaving extra connections idle. For a high-core-count machine, start with
`--target-outbound 32 --target-peer-count 80` (up to 64 concurrent range fetches). Increase
`--prefetch-memory-mb` if `/metrics` shows the queue repeatedly reaching its byte ceiling, and use
`--prefetch-max-inflight` only when an explicit cap is needed. More fetch concurrency cannot make
the serial chain-confirm boundary parallel, so CPU usage below 100% can still be normal when storage
or ordered validation is the limiting stage.

## Storage

Supported database URLs are:

- `sqlite://<path>` for the embedded default
- `postgres://<connection-string>` with the `postgres` feature
- `mmap://<directory>` with the `mmap` feature

The mmap directory must be writable by the node process.

## Sync Modes

- The default mode validates a weight proof and then fully validates forward from its checkpoint.
- `--genesis-sync` validates the chain from height zero.
- `--sync-from <height>` selects an explicit validated starting height.

## Monitoring

Prometheus metrics are served at `https://<listen-address>/metrics`. See [monitoring.md](monitoring.md) for the metric names and scrape configuration.
