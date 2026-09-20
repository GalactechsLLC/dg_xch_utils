# dg_xch_plotter

Native proof-of-space v2 plotting, inspection and development proving. The file writer and verifier are implemented in this repository; Chia's implementation is used only as an external test oracle.

## Status

The RAM-based pipeline creates format-1 plots and retains full witnesses. Low-k compatibility tests are available. This is not a production-size plotter or an efficient disk harvester: default limits deliberately reject k28, and rebuilding tables to answer a challenge is too expensive for normal farming. Format-2 Benes plots, checkpoint/resume, external sorting and compact-fragment recovery are not implemented.

Three execution paths are available:

| Backend | GPU work | Host work | Requirements |
| --- | --- | --- | --- |
| CPU | None | Entire pipeline in Rust | Stable Rust |
| Vulkan | AES hashing for generation, targets and candidate pairs | Rust matching, sorting, fragment encoding, validation and file writing | `vulkan` feature; hardware Vulkan compute adapter |
| [CUDA](cuda/README.md) | Rust kernels for generation, matching, emission, fragment encoding and sorting | Rust orchestration, validation and file writing | NVIDIA GPU and pinned cuda-oxide toolchain |

Vulkan supports the AMD/NVIDIA driver interface without vendor-specific kernels. Its narrow compute shader is WGSL, not Rust device code; there is no C++ kernel or JavaScript runtime. Neither GPU path silently falls back to CPU. Low-k reconstruction has been checked on an AMD RX 6800 XT and NVIDIA RTX A4000 against the same CPU-created canonical plot and matching proof results. This does not establish production-size support or certify other drivers and devices.

The desktop supports Auto, CUDA, and Vulkan preferences. Auto prefers a successfully probed trusted CUDA helper, otherwise non-NVIDIA hardware Vulkan, then NVIDIA Vulkan. This is a vendor heuristic, not a comparative performance result. CUDA and Vulkan device ordinals are never treated as interchangeable. The shared `backend` policy is hardware-independent and unit tested; command-line tools retain explicit CPU/Vulkan selection here and explicit CUDA in the separate helper. The Rust CPU path stays available without GPU toolchains, including for ARM builds; CUDA-oxide ARM support is not claimed.

## Build and run

Run commands from the repository root:

```sh
cargo build -p dg_xch_plotter --release
cargo run -p dg_xch_plotter --release -- --help
```

Supply your farmer's 48-byte public key and either a 48-byte pool public key or a 32-byte pool-contract puzzle hash, all hexadecimal. Do not supply a mnemonic or private key. The writer generates a fresh local plot key and refuses to overwrite an existing file.

```sh
cargo run -p dg_xch_plotter --release -- create \
  --output /path/to/development.plot \
  --farmer-key "$FARMER_PUBLIC_KEY" --pool-key "$POOL_PUBLIC_KEY" \
  --k 18 --strength 2 --experimental-size \
  --memory-mib 512 --max-entries 2097152 --max-work 100000000

cargo run -p dg_xch_plotter --release -- inspect /path/to/development.plot

cargo run -p dg_xch_plotter --release -- prove-plot /path/to/development.plot \
  --challenge "$CHALLENGE_HEX"
```

Use `--contract "$POOL_CONTRACT_HASH"` instead of `--pool-key` for a portable memo. A portable memo does not create a PlotNFT or pool registration on-chain. Use `--testnet` consistently when creating and proving testnet-domain plots: the hash domain is not stored in the file. An arbitrary challenge may produce no proofs.

`inspect` validates structure and identity, not the entire compressed payload. `prove-plot` rebuilds the tables, regenerates and byte-compares the complete canonical file, then independently verifies returned proofs. The file size must fit the configured verification budget.

## AMD and Vulkan

Install a working hardware Vulkan driver on Linux or Windows. The process must be able to access its GPU device; a software Vulkan implementation is deliberately rejected. Native macOS Vulkan is not assumed by this backend.

```sh
cargo build -p dg_xch_plotter --release --features vulkan
cargo run -p dg_xch_plotter --release --features vulkan -- devices

cargo run -p dg_xch_plotter --release --features vulkan -- create \
  --backend vulkan --device 0 \
  --output /path/to/development-amd.plot \
  --farmer-key "$FARMER_PUBLIC_KEY" --pool-key "$POOL_PUBLIC_KEY" \
  --k 18 --strength 2 --experimental-size

cargo run -p dg_xch_plotter --release --features vulkan -- prove-plot \
  /path/to/development-amd.plot --challenge "$CHALLENGE_HEX" \
  --backend vulkan --device 0
```

The device ordinal comes from `devices`; check it again after changing drivers or hardware. Launch separate processes with distinct ordinals for separate GPUs. The Vulkan development backend accepts strengths 2 through 8 and uses bounded batches with timeout and cancellation checks. Cancellation cannot preempt an already running GPU dispatch or CPU sort. Memory limits cover managed table and batch buffers, not driver allocations or total process RSS.

The farmer's `DevelopmentHarvester::open_with_engine` accepts the same build callback used by `create_with_engine` and `ReconstructedPlot::open_with_engine`. This exposes GPU reconstruction to development farming tools; it does not make it an efficient network signage-point solver.

## Validation

Ordinary checks do not require a GPU:

```sh
cargo test -p dg_xch_pos2 --test vulkan_shader
cargo test -p dg_xch_pos2 --features vulkan --lib
cargo test -p dg_xch_plotter --release
```

Run hardware tests explicitly, after normal checks, on an otherwise idle machine:

```sh
cargo test -p dg_xch_pos2 --release --features vulkan \
  vulkan_hashes_match_native_for_all_supported_rounds -- --ignored --nocapture
cargo test -p dg_xch_plotter --release --features vulkan \
  --test vulkan -- --ignored --nocapture
```

The hardware tests reject missing/software devices, compare native and GPU witnesses and reconstruct CPU plots byte-for-byte through the GPU backend. They default to ordinal 0; set `DGX_VULKAN_TEST_DEVICE` to select another ordinal reported by `devices`. They are not performance benchmarks or production-k tests.

The low-k Vulkan comparisons have passed on an AMD Radeon RX 6800 XT using RADV and an NVIDIA RTX A4000. This covers the test cases above, not every GPU, driver, strength, or production workload.

For direct Chia file comparisons, build `tests/reference_writer.cpp` externally against the source bundled with `chia-pos2 0.6.0` (upstream revision `b0da7aa7bec3974833d651173a6d9953c21eb808`). Use C++20, its `cpp` and `fse/fse` include paths and the reference FSE static library. Then run:

```sh
DGX_POS2_REFERENCE_BIN=/path/to/reference-writer \
  cargo test -p dg_xch_plotter --release \
  plot_files_match_chia_byte_for_byte_at_k18 -- --ignored --nocapture
```

k18 is the minimum supported by the pinned reference API. Production-k resource usage, efficient disk solving and end-to-end block production remain acceptance gates. See the [repository README](../readme.md) for the rest of the stack.
