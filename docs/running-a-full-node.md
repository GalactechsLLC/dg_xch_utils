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
`--prefetch-max-inflight` only when an explicit cap is needed. Catch-up validation automatically
combines the 32-block peer responses into a CPU-sized window (up to 256 blocks), while near-tip
confirmation stays at 32 blocks for low latency. The ordered store commit remains serial; use the
window phase metrics to distinguish that limit from validation capacity.

## Storage

Supported database URLs are:

- `sqlite://<path>` for the embedded default
- `postgres://<connection-string>` with the `postgres` feature
- `mmap://<directory>` with the `mmap` feature

The mmap directory must be writable by the node process.

### SQLite's transition to tip

Secondary coin indexes remain deferred during bulk sync to preserve ingestion speed.
At tip they build on an isolated connection with disk-spillable sorting, a 16 MiB page-cache
target and two auxiliary sort workers. This is not a total process-memory cap. Each SQLite
index statement still holds the writer; confirmations can pause until it finishes.

On small devices, keep both the database and sort scratch off tmpfs. Set `SQLITE_TMPDIR`
before starting the process to a private, writable disk-backed directory with sufficient
free space. Keep the existing DB on upgrade; do not clear it or force an index rebuild.

Storage stays in its low-latency profile at zero lag, with hysteresis before returning to
bulk mode. Large fully checkpointed WAL allocations are reclaimed opportunistically near
tip. Monitor the dashboard's native SQLite memory and index-maintenance panels alongside
confirmed height and host available memory; a maintenance liveness response is not proof
of current-tip readiness.

## Sync Modes

- The default mode validates a weight proof and then fully validates forward from its checkpoint.
- `--genesis-sync` validates the chain from height zero.
- `--sync-from <height>` selects an explicit validated starting height.

Recovery of historical generator references checks their hash commitments and binds them to
local chain records. References below the stored history require matching responses from two
distinct peers; recovery waits if only one peer is available. Corroboration does not protect
against colluding peers. Failed sync windows discard cached recovery references before retrying.

## Monitoring

Prometheus metrics are served at `https://<listen-address>/metrics`. See [monitoring.md](monitoring.md) for the metric names and scrape configuration.
