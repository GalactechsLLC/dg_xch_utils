# dg_xch_gui

Native Rust desktop for node diagnostics, accounts, farming, and development plotting. It uses egui/wgpu rather than a browser, Electron, or a JavaScript runtime.

## Design and integration

Setup flows use visible cards, distinct primary actions, inline safety notices, and contextual help for technical controls. Plot identity and wallet selection sit side by side on wide windows and stack on smaller screens. Diagnostic tables wrap long values instead of clipping them. The screenshot profile uses the real Linux home directory and a live local node; it contains no wallet secrets.

New profiles default to Garden light, with the website's 150-degree white (`#fefefe`) to green (`#87db94`) background gradient. Forest dark is optional. Existing saved theme preferences are not overwritten. The README screenshots show Garden light.

The palette follows `druid.garden`'s `variables.scss`: forest background `#0b0f0d`, surface `#121916`, primary green `#3f8c61`, and warm light surfaces. Sora is bundled locally under its SIL Open Font License in `assets`; no font requests or web runtime are used. `theme` owns visual tokens and typography. Pages are Overview, Wallets, Node, Farm, Plots, and Settings. Always-visible cards replace disclosure widgets, diagnostics render as labeled values with a full-data copy action, and settings use two columns on wide windows with a single-column fallback.

## Status

The desktop is functional but still under development. Standard-coin wallets, multiple unlocked accounts, background polling, live node details, and embedded PoS1 farming are available. PoS2 RAM plotting and fragment proof recovery use explicit resource budgets; PoS2 network farming is connected and follows activation rules; sustained mainnet operation is a separate validation task. CATs, NFTs, offers, hardware signing, and complete transaction recovery are not exposed. Use test funds.

## Run

From the repository root:

```sh
cargo build -p dg_xch_cli --release
./target/release/dgx init
./target/release/dgx gui
```

Use a current stable Rust toolchain. Linux builds need a C toolchain, OpenSSL headers, and X11/Wayland development libraries, including libxkbcommon. Install a working graphics driver. macOS uses Metal for rendering and Windows uses the native wgpu backend; neither needs WebKit or Node.js. The desktop does not depend on the full node's GMP backend. CI builds and launches it on Linux, macOS, and Windows; that is not a guarantee for every driver or desktop environment.

## Connect and configure

![Native connection preferences](screenshots/preferences-garden-networks.png)

On Windows, build `dg_xch_cli` with `--no-default-features --features desktop,vulkan`, run `dgx init`, then `dgx gui`. Configure an existing compatible remote node in Settings; this does not provide a local Windows full node. CI checks the portable initializer and native desktop separately on all three desktop platforms.

1. Run `dgx init`, then start a [full node](../full-node/README.md) with `hint` enabled for wallet queries. Initialization seeds TLS paths and the local port; existing preferences are retained.
2. In Settings, set its RPC hostname and port. The Rust full node normally serves RPC and peers on the same listener, port 8444; a Chia node may use 8555 for RPC.
3. Supply the trusted private CA certificate and a client certificate/private key signed by that CA. The hostname must match the server certificate; the local Rust node certificate includes localhost and loopback addresses. Use a properly issued certificate or a local tunnel for remote access, not an insecure verification bypass.
4. Select Chia mainnet or Chia testnet11 from Network; their genesis block header hashes are pinned in the core library and read-only. Select Custom to reveal the chain-definition JSON path and editable trusted genesis block header hash. The network ID comes from that JSON on save. For custom chains, obtain the header hash independently; it is not the genesis challenge.
5. Add or import accounts, record the mnemonic backup, and use a strong password. Multiple accounts can remain unlocked and update independently.
6. Configure plot directories and farmer payout details. Start a farmer using an account or an existing FastFarmer YAML file. Legacy YAML contains plaintext farming keys; encrypted account mode derives keys in memory.

On Linux, new profiles use `~/.dgx/config` for settings and `~/.dgx/data` for encrypted accounts and SQLite wallet state; plots default to `~/.dgx/data/plots`. macOS and Windows retain native per-user directories under the `Galactechs` / `dg_xch` application identity. Previously initialized profiles are still discovered when no new default profile exists. Explicit `--config-dir` and `DGX_CONFIG_DIR` take precedence; no existing data is moved. Settings displays the exact paths. See [wallet storage and backups](../wallet/README.md). Lock every account and stop farming before changing networks or endpoints.

`AppPaths::discover` reads the initialized `dgx.json` profile through shared server helpers, honoring `DGX_CONFIG_DIR`. `dgx gui` requires initialization; `dgx gui --smoke-test` is isolated and exempt. The old desktop executable has been removed; `runner::run` is the library entry point called by `dgx` on the main thread. Plot output defaults to the first configured plot directory. The workshop can derive public farmer/pool keys from an encrypted account without opening a wallet session or connecting to a node. No secret keys are written into plots.

The GUI owns an embedded farmer, but not the separately launched node. Closing the GUI stops its farmer and wallet polling. The [root visual guide](../readme.md) documents the user workflow; `screenshots` contains actual captures connected to a running local, unsynchronized node, not fabricated chain state.

## Plotting and GPUs

The plotting form accepts an existing output directory, not a filename. CPU, Vulkan, and CUDA jobs stage incomplete files privately within that directory, then publish `plot-k<size>-YYYY-MM-DD-HH-MM-<plot-ID>.plot` using the UTC start time and actual plot ID. Publishing never overwrites an existing file; publication errors retain the completed file at the recovery path shown in the job status. Proof checking has a separate existing-file field.

Plots defaults to k28, strength 2, and a 12 GiB memory budget and supports the pinned reference's even k18–k32 domain and size-dependent strength range. Set sufficient RAM and work budgets before choosing a larger plot; k32 needs substantially more than 64 GiB of RAM. New settings default to **Auto** GPU selection; saved explicit CUDA/Vulkan preferences remain unchanged. Auto first probes the configured NVIDIA CUDA device through the separately built trusted [CUDA executable](../plotter/cuda/README.md). Supply an absolute executable path; the desktop never searches `PATH` or downloads a helper. Probing has a five-second deadline and bounded output. The current helper loads its CUDA module and checks one GPU hash against the CPU; it does not run a plotting workload or performance benchmark.

If that probe succeeds, Auto uses native Rust CUDA. Otherwise it selects the lowest-ordinal non-NVIDIA hardware Vulkan adapter, or the lowest-ordinal NVIDIA Vulkan adapter if no other vendor is available. This vendor policy preserves the more extensive native CUDA pipeline; it is **not a measured claim that CUDA is faster on every device**. The selected backend, name, ordinal, and any CUDA preflight failure are displayed with the job. Once selected, a backend failure stops the job; it does not retry on another GPU or CPU.

Choose explicit CUDA or Vulkan in Settings to pin a device. Their ordinals are independent: CUDA device 0 is not necessarily Vulkan device 0, especially in mixed-vendor systems. The configured CUDA ordinal also applies to Auto's CUDA probe; the Vulkan ordinal applies only to explicit Vulkan selection. Disable GPU in Plots for the Rust CPU path, including systems without supported GPU compute. ARM CPU portability does not establish CUDA-oxide support on ARM, and macOS Metal UI rendering is separate from PoS2 compute.

Vulkan uses in-process WGSL compute with Rust orchestration; CUDA kernels and orchestration are Rust. Both have resident k28 strength-2 pipelines; other parameters use bounded hybrid paths. Software adapters are rejected for compute. Network farming and accepted blocks remain distinct from successful plotting. Read the [plotter limits](../plotter/README.md) before running either backend.

## Validation

```sh
cargo test -p dg_xch_gui -p dg_xch_wallet -p dg_xch_farmer
cargo run -p dg_xch_cli -- gui --smoke-test
```

The smoke test uses temporary application data, disables node polling, renders all six pages in both themes, and exits. It needs a display server; on headless Linux use `xvfb-run -a target/debug/dgx gui --smoke-test`. It does not send funds, farm, or run GPU compute workloads.

If Xvfb cannot present through your Vulkan driver, use `WGPU_BACKEND=gl LIBGL_ALWAYS_SOFTWARE=1 xvfb-run -a target/debug/dgx gui --smoke-test`. This selects software rendering for the UI only; PoS2 GPU compute still rejects software adapters.

[Repository overview](../readme.md)
