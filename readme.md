# DruidGarden XCH Utils

Rust components for Chia-compatible nodes and experimental custom chains: native desktop, wallets, farming, plotting, peer discovery, storage, and consensus libraries.

## Current state

This is active development, not a finished chain-launch distribution.

- The native desktop uses egui/wgpu, without Electron, a browser runtime, or JavaScript.
- Standard wallets have encrypted account keys and durable SQLite state. Multiple accounts can sync in the background. Use test funds; funded-chain and recovery testing are still needed.
- The integrated farmer comes from `dg_fast_farmer`, without its old UI. PoS1 and PoS2 discovery, signing and network submission are connected, including CPU, native CUDA and Vulkan proof recovery. End-to-end deployment testing remains a separate gate.
- Native PoS2 RAM plots can be checked against the pinned external Chia reference. CPU, CUDA and AMD-capable Vulkan share compact plotting and fragment-recovery paths. Large k sizes require explicit RAM/work budgets; network farming readiness is separate from plotting. Vulkan uses a WGSL shader with Rust host code.
- The introducer provides peer discovery. The CPU timelord schedules real signage, infusion and end-of-slot proofs; compact-proof mode remains separate. ASIC backend integration and production throughput validation remain future work.
- Chia mainnet is the default for library and service configuration. Runtime `ChainSelection` also supports the pinned no-prefarm DGX preset and explicit custom definitions. `config/chains/dgx.json` selects the version-2 DGX chain with PoS2 active from genesis; it is a different chain from the legacy version-1 definition.

## Build and start

Use a current stable Rust toolchain and a native C/C++ build toolchain for dependencies. Initialize any submodules:

```sh
git submodule update --init --recursive
cargo build -p dg_xch_gui --release
cargo run -p dg_xch_gui --release
```

Linux desktop builds need OpenSSL, libxkbcommon, and Wayland/X11 development packages plus a working graphics driver. macOS needs the Xcode command-line tools. Windows desktop builds use the MSVC toolchain. The full node and VDF tools currently target Linux/macOS because GMP does not support this Windows MSVC dependency path. CUDA uses a separate pinned nightly toolchain; it is not required to build the desktop or CPU libraries.

Start a wallet-capable full node:

```sh
mkdir -p ./local-node
cargo run -p dg_xch_cli --release --features coin-index -- full-node \
  --listen 127.0.0.1:8444 --db sqlite://./local-node/chain.db \
  --ssl-dir ./local-node/ssl --network mainnet --genesis-sync
```

This selects Chia, not a newly created chain; supply reachable peers or an introducer to synchronize it. Use `dg chain init --output ./new-chain --network dgx` for the custom production preset, or `--development` instead of `--network` for a disposable small-farm chain. Initialization never fabricates blocks. See the [full-node README](full-node/README.md) for private-CA RPC, discovery, trust policy, and storage. The Rust node serves RPC and peers on the same listener. In the desktop, configure that port, verified TLS paths, the same chain definition, and a trusted genesis block-header hash.

## Applications and services

| Package | Purpose and instructions |
| --- | --- |
| [dg_xch_gui](gui/README.md) | Native desktop, multiple accounts, node details, farming and plotting controls |
| [dg_xch_wallet](wallet/README.md) | Encrypted accounts, SQLite persistence, synchronization, signing and backups |
| [dg_xch_cli](cli/README.md) | The `dg` command and full-node entry point |
| [dg_full_node](full-node/README.md) | Full-node application, authenticated RPC and wallet queries |
| [dg_xch_farmer](farmer/README.md) | Integrated farmer/harvester and legacy configuration |
| [dg_xch_plotter](plotter/README.md) | Native CPU/Vulkan PoS2 RAM plotting, file reading and fragment proof recovery |
| [dg_xch_plotter_cuda](plotter/cuda/README.md) | Separately built NVIDIA CUDA backend |
| [dg_xch_timelord](timelord/README.md) | CPU regular timelord, verified VDF workers and separate compact-proof service |
| [dg_xch_introducer](introducer/README.md) | Bounded peer discovery and registration |
| [dg_xch_simulator](simulator/README.md) | Deterministic test fixtures and development servers |
| [dg_xch_dev_tools](tools/README.md) | Database, corpus, node and weight-proof diagnostics |

## Shared libraries

| Package | Responsibility |
| --- | --- |
| [core](core/README.md) | Blockchain types, consensus constants, CLVM, protocol messages and TLS helpers |
| [node](node/README.md) | Node engine, mempool, synchronization and slot state |
| [stores](stores/README.md) | Chain storage backends and interfaces |
| [p2p](p2p/README.md) | Peer addresses, connection supervision and discovery |
| [clients](clients/README.md) | RPC, peer and pool clients |
| [servers](servers/README.md) | Shared transport and bounded protocol framing; deliberately not merged into a service |
| [proof_of_space](proof_of_space/README.md) | Compatibility facade and proof-version dispatch |
| [pos1](proof_of_space/pos1/README.md) | PoS1 verification, reading and plotting primitives |
| [pos2](proof_of_space/pos2/README.md) | Native PoS2 verification, table generation and GPU integration |
| [pos_common](proof_of_space/pos_common/README.md) | Shared entropy codecs |
| [vdf](vdf/README.md) | Class-group VDF primitives |
| [weight-proof](weight-proof/README.md) | Weight-proof verification and serving |
| [keys](keys/README.md) | Mnemonics, BLS derivation and addresses |
| [puzzles](puzzles/README.md) | CLVM puzzle construction |
| [serialize](serialize/README.md) | Wire encoding and bounded decoding |
| [macros](macros/README.md) | Serialization derive macro |
| [parser_macro](parser_macro/README.md) | Compile-time CLVM parsing |
| [logging](logging/README.md) | Application logging |

## Local stack and validation

The [Docker Compose harness](docker/README.md) provisions separate nodes, an introducer, independent farmers and a regular CPU timelord with disposable test identities. It includes sequential matching-plot preparation and an acceptance checker for real genesis production, cross-node agreement, zero prefarm and required farmer participation. GPU services are opt-in profiles, with a separate native CUDA image. Container health alone is not chain readiness; run the acceptance checks before treating a deployment as working.

Start with focused checks, then run broader tests. Leave GPU and resource-intensive runs until the end:

```sh
cargo test -p dg_xch_wallet -p dg_xch_gui -p dg_xch_farmer
cargo test -p dg_xch_introducer -p dg_xch_timelord
cargo test -p dg_xch_pos2 -p dg_xch_plotter --features vulkan
cargo test --workspace --locked
```

Some tests need fixtures or local listeners; ignored hardware/reference tests require explicit setup in the relevant package README. See [fuzzing](fuzz/README.md) for bounded parser/VM checks. CI separately builds and smoke-tests the desktop on Linux, macOS, and Windows. A passing build or dependency advisory scan is not a full security assessment.

Keep real mnemonics, CA keys, wallet databases and production node data outside the checkout and outside test containers. Published crates may lag behind this working tree.

The dependency-audit configuration retains one reviewed exception, `RUSTSEC-2023-0071`: RSA is used for local certificate generation/signing, not an exposed RSA decryption oracle. This is a remaining dependency risk, not a fixed advisory; re-evaluate the exception if that usage changes. Unmaintained-dependency warnings are not suppressed.
