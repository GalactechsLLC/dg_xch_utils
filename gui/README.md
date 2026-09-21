# dg_xch_gui

Native Rust desktop for node diagnostics, accounts, farming, and development plotting. It uses egui/wgpu rather than a browser, Electron, or a JavaScript runtime.

## Status

The desktop is functional but still under development. Standard-coin wallets, multiple unlocked accounts, background polling, live node details, and embedded PoS1 farming are available. PoS2 RAM plotting and fragment proof recovery use explicit resource budgets; PoS2 network farming is not connected yet. CATs, NFTs, offers, hardware signing, and complete transaction recovery are not exposed. Use test funds.

## Run

From the repository root:

```sh
cargo run -p dg_xch_gui --release
```

Use a current stable Rust toolchain. Linux builds need a C toolchain, OpenSSL headers, and X11/Wayland development libraries, including libxkbcommon. Install a working graphics driver. macOS uses Metal for rendering and Windows uses the native wgpu backend; neither needs WebKit or Node.js. The desktop does not depend on the full node's GMP backend. CI builds and launches it on Linux, macOS, and Windows; that is not a guarantee for every driver or desktop environment.

## Connect and configure

1. Start a [full node](../full-node/README.md) with `coin-index` enabled for wallet queries.
2. In Preferences, set its RPC hostname and port. The Rust full node normally serves RPC and peers on the same listener, port 8444; a Chia node may use 8555 for RPC.
3. Supply the trusted private CA certificate and a client certificate/private key signed by that CA. The hostname must match the server certificate; the local Rust node certificate includes localhost and loopback addresses. Use a properly issued certificate or a local tunnel for remote access, not an insecure verification bypass.
4. Select the network or custom chain-definition JSON. Obtain the genesis **block header hash** independently and enter it before unlocking wallets; it is not the genesis challenge.
5. Add or import accounts, record the mnemonic backup, and use a strong password. Multiple accounts can remain unlocked and update independently.
6. Configure plot directories and farmer payout details. Start a farmer using an account or an existing FastFarmer YAML file. Legacy YAML contains plaintext farming keys; encrypted account mode derives keys in memory.

Settings and themes use the platform's per-user configuration directory. Encrypted accounts and SQLite wallet files use its local application-data directory, under the `Galactechs` / `dg_xch` application identity. Preferences displays the exact paths. See [wallet storage and backups](../wallet/README.md). Lock every account and stop farming before changing networks or endpoints.

## Plotting and GPUs

Plot Workshop defaults to k18 and supports the pinned reference's even k18–k32 domain and size-dependent strength range. Set sufficient RAM and work budgets before choosing a larger plot; k32 needs substantially more than 64 GiB of RAM. New settings default to **Auto** GPU selection; saved explicit CUDA/Vulkan preferences remain unchanged. Auto first probes the configured NVIDIA CUDA device through the separately built trusted [CUDA executable](../plotter/cuda/README.md). Supply an absolute executable path; the desktop never searches `PATH` or downloads a helper. Probing has a five-second deadline and bounded output. The current helper loads its CUDA module and checks one GPU hash against the CPU; it does not run a plotting workload or performance benchmark.

If that probe succeeds, Auto uses native Rust CUDA. Otherwise it selects the lowest-ordinal non-NVIDIA hardware Vulkan adapter, or the lowest-ordinal NVIDIA Vulkan adapter if no other vendor is available. This vendor policy preserves the more extensive native CUDA pipeline; it is **not a measured claim that CUDA is faster on every device**. The selected backend, name, ordinal, and any CUDA preflight failure are displayed with the job. Once selected, a backend failure stops the job; it does not retry on another GPU or CPU.

Choose explicit CUDA or Vulkan in Preferences to pin a device. Their ordinals are independent: CUDA device 0 is not necessarily Vulkan device 0, especially in mixed-vendor systems. The configured CUDA ordinal also applies to Auto's CUDA probe; the Vulkan ordinal applies only to explicit Vulkan selection. Disable GPU in Plot Workshop for the Rust CPU path, including systems without supported GPU compute. ARM CPU portability does not establish CUDA-oxide support on ARM, and macOS Metal UI rendering is separate from PoS2 compute.

Vulkan remains an in-process WGSL compute shader with Rust host matching; CUDA kernels and host orchestration remain Rust. Software adapters are rejected for compute. Neither backend is a production-size network farmer. Read the [plotter limits](../plotter/README.md) before running either backend.

## Validation

```sh
cargo test -p dg_xch_gui -p dg_xch_wallet -p dg_xch_farmer
cargo run -p dg_xch_gui -- --smoke-test
```

The smoke test uses temporary application data, disables node polling, renders all six pages in both themes, and exits. It needs a display server; on headless Linux use `xvfb-run -a target/debug/dg_xch_gui --smoke-test`. It does not send funds, farm, or run GPU compute workloads.

If Xvfb cannot present through your Vulkan driver, use `WGPU_BACKEND=gl LIBGL_ALWAYS_SOFTWARE=1 xvfb-run -a target/debug/dg_xch_gui --smoke-test`. This selects software rendering for the UI only; PoS2 GPU compute still rejects software adapters.

[Repository overview](../readme.md)
