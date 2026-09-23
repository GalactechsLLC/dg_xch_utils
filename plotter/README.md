# dg_xch_plotter

Native Rust PoS2 plotting, file inspection, and proof recovery. Plotting uses RAM rather than temporary disk tables and does not need a synchronized node.

## Install and create a plot

From the repository root:

```sh
cargo install --path cli --locked
dgx init
dgx plotter -- --help
```

Supply your farmer public key and portable plot's pool-contract puzzle hash as hex. Neither value is a private key.

```sh
dgx plotter create \
  --output /home/user/.dgx/data/plots/k28.plot \
  --farmer-key "$FARMER_PUBLIC_KEY" --contract "$POOL_CONTRACT_HASH" \
  --k 28 --strength 2 \
  --memory-mib 12288 --max-entries 310000000 --max-work 10000000000

dgx plotter inspect /home/user/.dgx/data/plots/k28.plot
```

The output directory must exist; existing files are never overwritten. Use `--pool-key "$POOL_PUBLIC_KEY"` instead of `--contract` for a pool-public-key memo. A portable plot still needs an on-chain PlotNFT and pool setup.

These commands use the mainnet hash domain. **That does not make a plot eligible for mainnet farming:** this checkout's mainnet PoS2 activation constants still need updating and validation. Keep existing farms until network compatibility is established.

## Formats and resources

The compatibility target is `chia-pos2 0.6.0`, format-1 fat plots:

- Even k18–k32, with strength 2 through `k - section_bits - 1`.
- `section_bits` is 2 below k28 and `k - 26` otherwise.
- Pool-public-key and portable-contract memos, indices, and meta groups.
- CPU, native NVIDIA CUDA, and hardware Vulkan (including AMD).

CPU table budgets are roughly 9.04 GiB for k28, 36.04 GiB for k30, and 144.04 GiB for k32, **plus** process overhead. Higher strengths increase work sharply. The default 512 MiB limit is intentionally too small for large plots.

Checkpoint/resume, disk-backed sorting, and format-2 Benes plots are not implemented. Full k32 plotting has not been validated on available hardware.

## GPUs and proof checks

```sh
dgx plotter devices
```

For AMD or another hardware Vulkan adapter, add `--backend vulkan --device 0` to the creation command, using the ordinal from the device list. NVIDIA can use the [CUDA helper](cuda/README.md), selected in the GUI by absolute path.

Both full-resident k28 strength-2 pipelines keep intermediate tables, sorting, matching, and output packing on the GPU. A 12 GiB managed budget covers the standard pipeline; available VRAM and device limits still matter. Other settings use lower-memory or GPU-hashing/CPU-matching paths. FSE compression and file writing stay on the CPU.

GPU errors do not silently select CPU. Vulkan software adapters are rejected. CUDA and Vulkan ordinals are independent; macOS UI rendering is separate from compute support.

To recover proofs for a challenge:

```sh
dgx plotter prove-plot /home/user/.dgx/data/plots/k28.plot \
  --challenge "$CHALLENGE_HEX"
```

An arbitrary challenge may have no proofs. Inspection checks structure; proving reads relevant chunks and verifies recovered proofs, not the entire file.

## Library use

`create_in_memory` and `create_in_memory_with_engine` return bounded file bytes. `PlotReader::from_reader` accepts an in-memory cursor or another seekable reader. The shared writer consumes compact fragments from [dg_xch_pos2](../proof_of_space/pos2/README.md).

Set explicit memory, entry, work, and cancellation limits. Managed budgets exclude driver allocations and total process RSS.

## Validation and benchmarks

CPU, CUDA, and Vulkan k28 strength-2 output has matched the pinned Chia reference byte-for-byte. AMD k30 creation and reference-reader checks also passed; bounded k32 kernel tests are not a full k32 plot.

Recorded sequential k28 medians, including durable writes, were **6.38 s** on an RX 6800 XT (RADV) and **9.23 s** on an RTX A4000, using a Ryzen 9 5950X. These are different GPUs, not a controlled backend ranking or a comparison with Chia's GPU plotter.

For reproducible comparisons, use identical plot identities, thread counts, and output durability. Build before timing, run one workload at a time, and record driver versions and temperatures. `DGX_POS2_PROFILE=1` reports phase timings. The ignored benchmark and reference adapter live in `src/benchmark.rs` and `tests/reference_writer.cpp`.

[Farmer](../farmer/README.md) · [CUDA helper](cuda/README.md) · [Desktop](../gui/README.md)
