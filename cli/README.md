# dg_xch_cli

`dgx` is the launcher for the desktop, full node, farmer, plotter, pool, introducer, and CPU timelord. It also provides RPC and key utilities.

## Install and start

From the repository root:

```sh
cargo install --path cli --locked
dgx init
dgx full-node
```

In another terminal, run `dgx gui`. Chia mainnet is the default. Setup asks where to store configuration, data, and plots; press Enter to accept the defaults.

Linux defaults to `~/.dgx/config` and `~/.dgx/data`. macOS and Windows use their per-user application directories. Select another profile with `dgx --config-dir /path/to/config init`, and use that same option on later commands. Reinitializing preserves existing settings and keys.

## Build options

| Installation | Command |
| --- | --- |
| Linux/macOS, all standard services | `cargo install --path cli --locked` |
| Headless Linux/macOS | `cargo install --path cli --locked --no-default-features --features hint,timelord,vulkan` |
| Windows desktop and remote-node client | `cargo install --path cli --locked --no-default-features --features desktop,vulkan` |

The default `hint` feature includes wallet coin indexes. Optional `postgres` and `mmap` features add node storage backends. CUDA needs a [separate helper](../plotter/cuda/README.md). See the [root installation guide](../readme.md#install) for system prerequisites.

## Commands and integration

```sh
dgx --help
dgx full-node --help
dgx farmer --help
dgx plotter -- --help
dgx pool --help
```

Service configuration examples live in their package READMEs. RPC calls need the node's trusted CA and client identity; public peer certificates are not private RPC credentials. Keep seed phrases and private keys out of command arguments and shell history.

For embedding, `dg_xch_cli_lib::run_cli` dispatches commands. Service adapters live in `src/services`; their reusable implementations stay in the respective libraries. The GUI runs on the OS main thread.

[Full node](../full-node/README.md) · [Desktop](../gui/README.md) · [Developer tools](../tools/README.md)
