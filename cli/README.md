# dg_xch_cli

The `dgx` executable provides initialization, full-node startup, integrated application commands, and RPC, wallet, key, and diagnostic commands. The library entry point is `dg_xch_cli_lib::run_cli`.

## Status

Default features include the native desktop, full node with wallet indexes, CPU/Vulkan farming and plotting, introducer, and CPU timelord. Linux/macOS support the complete build. Windows uses `--no-default-features --features desktop,vulkan` to omit the GMP-dependent node/timelord. For a headless node deployment use `--no-default-features --features hint,timelord,vulkan`. A plain `--no-default-features` build retains initialization, RPC, CPU plotting/farming, and introducer commands.

## Build and run

New Linux profiles default to `~/.dgx/config` and `~/.dgx/data`, with plots under `~/.dgx/data/plots`. macOS and Windows retain native application directories. Explicit `--config-dir` or `DGX_CONFIG_DIR` overrides discovery. Existing initialized profiles at the previous default are still found when the new location is uninitialized; no data is automatically moved.

From the repository root:

```sh
cargo build -p dg_xch_cli --release --features hint
./target/release/dgx --help
./target/release/dgx full-node --help
./target/release/dgx chain --help
```

Add `hint` for wallet hint queries, `postgres` for PostgreSQL, or `mmap` for the memory-mapped chain backend. Feature names are build-time choices, not runtime flags. Published crate versions may predate this checkout.

## Application initialization and launchers

Run `dgx init` before starting the node or other services. Enter accepts each suggested configuration, data, and plot directory. Global `--config-dir` overrides `DGX_CONFIG_DIR`, which overrides platform defaults. Relative inputs become absolute; custom config locations must also be supplied on subsequent launches.

```sh
./target/release/dgx --config-dir /absolute/path/config init --non-interactive \
  --data-dir /absolute/path/data --plots-dir /absolute/path/plots
./target/release/dgx --config-dir /absolute/path/config full-node
./target/release/dgx --config-dir /absolute/path/config gui
```

`dgx.json` is the versioned storage profile and initialization marker; `desktop.json` holds GUI preferences and `ssl` contains TLS identities. Reinitialization retains settings and keys; conflicting explicit paths fail. Setup does not migrate existing data.

The node defaults to the initialized database/TLS paths and mainnet discovery through `introducer.chia.net:8444`. Explicit node flags take precedence. Initialized RPC commands default to the same local port and TLS root; pass endpoint flags for another server.

`gui`, `farmer`, `plotter`, `timelord`, and `introducer` run in the `dgx` executable. Their entry-point adapters live under `cli/src/services`, with reusable implementations in package libraries. The GUI runs on the OS main thread; other commands share the CLI's Tokio runtime. Timelord workers re-execute `dgx timelord worker` or `regular-worker` with bounded stdin/stdout and no app-profile requirement. No shell or companion application is involved. `simulator` remains a separate developer tool; CUDA's pinned-nightly device helper remains a separate backend build. These are foreground commands, not a service manager.

Help, offline commands, `chain init`, and `full-node --print-chain-info` work without application initialization. Chain-directory initialization below is separate; use a distinct application profile and database for each chain.

## Chain initialization

One build supports Chia and custom chains. Chia mainnet is the default; unknown network names fail instead of falling back to it.

```sh
dgx chain init --output ./chia-node
dgx chain init --output ./dgx-node --network dgx
dgx chain init --output ./dev-node --development
dgx chain inspect --chain-config ./dev-node/chain.json
dgx --config-dir ./dev-app init --non-interactive --data-dir ./dev-node/data --plots-dir ./dev-plots
dgx --config-dir ./dev-app full-node --chain-config ./dev-node/chain.json \
  --ssl-dir ./dev-node/ssl --db sqlite://./dev-node/data/chain.db
```

Initialization creates a chain manifest, storage directory and TLS material, not wallet keys or accepted blocks. Repeating it preserves existing certificates and matching identities. Mismatched chains and nonempty unrecognized directories are rejected, not overwritten. `--genesis-seed` is available with `--development` for a reproducible launch identity. A development manifest must be shared with all participating services. Actual first-block production requires the farmer and regular timelord; use the [Compose tools](../docker/README.md) for a complete disposable setup.

Follow the [full-node instructions](../full-node/README.md) to start a node. For each RPC command, use `dgx <command> --help` and configure the intended network, endpoint, and TLS paths. Legacy mnemonic and private-key arguments remain available, but supplying secrets on the command line can expose them through shell history and process listings. Prefer the non-echoing interactive mnemonic prompt where available, or the native desktop's encrypted account workflow for long-lived wallets; CLI wallet APIs are not all equivalent to that durable session workflow.

`dgx create-wallet cold` displays its recovery phrase directly in an interactive terminal, not through the logger, and refuses redirected output. It does not display the raw private key or save the phrase; record it securely offline and avoid terminal session recording.

## Validation

```sh
cargo test -p dg_xch_cli
```

Network commands require a compatible reachable service. Do not use production funds for development integration checks.

[Repository overview](../readme.md) · [Wallet](../wallet/README.md) · [Desktop](../gui/README.md)
