# dg_xch_gui

Native egui/wgpu desktop for Chia wallets, node status, farming, and plotting. No browser or JavaScript runtime.

## Install and launch

From the repository root on Linux or macOS:

```sh
cargo install --path cli --locked
dgx init
dgx gui
```

On Windows, install with `cargo install --path cli --locked --no-default-features --features desktop,vulkan` instead. Windows desktop builds connect to a separate node.

See the [root guide](../readme.md) for build prerequisites and the visual walkthrough. Start a local node with `dgx full-node` in another terminal.

## Connect and use

**Settings** defaults to Chia mainnet and the local node on port 8444. For another node, supply its RPC endpoint, trusted private CA, and client certificate/key. Chia reference nodes commonly use port 8555. Mainnet's trusted genesis header is built in.

- **Wallets:** create or import accounts and unlock several at once. Balances update in the background.
- **Node:** height, cumulative weight, sync status, mempool capacity, and local diagnostics.
- **Farm:** start an account-backed farmer or load existing YAML. Pool settings are read from the pool before you edit them.
- **Plots:** choose an output directory and CPU/GPU backend. Filenames are generated automatically; plotting can run during node sync.
- **Tools:** address conversion, CAT2/NFT1 creation, and XCH/CAT2 offers.

The GUI owns its embedded farmer, not the separately launched node. Closing it stops farming and wallet polling. Lock accounts and stop farming before changing endpoints.

![Connection settings](screenshots/preferences-garden-selection.png)

## Developer integration

`runner::run` runs on the main thread through `dgx`. `AppPaths::discover` loads the initialized profile; `DGX_CONFIG_DIR` overrides discovery. Linux defaults are `~/.dgx/config` and `~/.dgx/data`; other platforms use their native application directories.

The `theme` module holds shared colors and typography. Garden light is the default; Forest dark is optional. Sora fonts are bundled locally. Preserve visible action feedback and use stable widget IDs for account-specific inputs.

For a display-backed smoke check:

```sh
dgx gui --smoke-test
```

On headless Linux, use `xvfb-run -a dgx gui --smoke-test`. This renders temporary UI state without sending funds or starting farming.

CUDA requires a separately built helper configured by absolute path. Vulkan supplies AMD compute; Metal UI rendering on macOS does not imply PoS2 GPU compute support. See [plotter limits](../plotter/README.md) and [wallet support and backups](../wallet/README.md) before using real funds.
