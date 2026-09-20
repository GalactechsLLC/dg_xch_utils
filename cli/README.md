# dg_xch_cli

The `dg` command provides full-node startup and RPC, wallet, key, and diagnostic commands.

## Status

This is the command-line application for the workspace, not the graphical desktop. Node builds currently target Linux and macOS because the VDF dependency uses GMP. The GUI has a separate Windows-capable build.

## Build and run

From the repository root:

```sh
cargo build -p dg_xch_cli --release --features coin-index
./target/release/dg --help
./target/release/dg full-node --help
```

Add `hint` for wallet hint queries, `postgres` for PostgreSQL, or `mmap` for the memory-mapped chain backend. Feature names are build-time choices, not runtime flags. Published crate versions may predate this checkout.

Follow the [full-node instructions](../full-node/README.md) to start a node. For each RPC command, use `dg <command> --help` and configure the intended network, endpoint, and TLS paths. Legacy mnemonic and private-key arguments remain available, but supplying secrets on the command line can expose them through shell history and process listings. Prefer the non-echoing interactive mnemonic prompt where available, or the native desktop's encrypted account workflow for long-lived wallets; CLI wallet APIs are not all equivalent to that durable session workflow.

`dg create-wallet cold` displays its recovery phrase directly in an interactive terminal, not through the logger, and refuses redirected output. It does not display the raw private key or save the phrase; record it securely offline and avoid terminal session recording.

## Validation

```sh
cargo test -p dg_xch_cli
```

Network commands require a compatible reachable service. Do not use production funds for development integration checks.

[Repository overview](../readme.md) · [Wallet](../wallet/README.md) · [Desktop](../gui/README.md)
