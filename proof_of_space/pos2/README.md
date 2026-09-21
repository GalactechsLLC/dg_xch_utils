# dg_xch_pos2

Native Rust proof-of-space v2 hashing, fragment encoding, quality chaining, validation and in-memory plotting primitives. This library does not depend on Chia's PoS2 implementation at runtime.

## Status

The implementation targets `chia-pos2 0.6.0`: even k18 through k32, strength 2 through `k - section_bits - 1`, and separate mainnet/testnet hashing. `CompactPlot` uses compact 16-byte entries and retains final fragments rather than full witnesses. CPU plotting uses two RAM tables. Fully GPU-resident CUDA and Vulkan k28 strength-2 plotting keep both tables and sorting on the device; lower-memory paths alternate CPU sorting with a GPU-resident input table. `NativePlot` remains a development/reference path with complete witnesses. Large plots require explicit resource budgets and enough physical RAM or VRAM for the selected path.

The shared `device` module supplies transformations to native CUDA kernels and the optional Vulkan host pipeline. CUDA device code is Rust; Vulkan uses WGSL shaders and supplies the AMD path. Both resident k28 strength-2 backends perform generation, target matching, pair hashing, filtering and fragment encoding on the GPU. Their full-resident paths also perform radix sorting and delta/stub packing there, without transferring intermediate tables to the host. Lower-memory paths share `compact::resident` orchestration, budget accounting and CPU radix sorting. The plotter package supplies the common FSE compressor and file writer, consuming shared `compact::PackedChunk` values. Other configurations use GPU hashing with host matching and fragment assembly. Vulkan software adapters are rejected and device errors do not select a CPU fallback.

The k28 strength-2 CPU path combines eight-lane hardware AES batches, bounded parallel matching and safe Rust radix sorting. Non-resident CUDA and Vulkan plotting at those parameters use at most eight host matching workers and 262,144-input batches, sharing one serialized accelerator engine while host work overlaps hashing. This worker limit does not apply to resident matching. GPU selection is not a promise of higher throughput; neither accelerated path silently replaces GPU hashing with CPU hashing.

## Usage

This is a library package, not a daemon. The [plotter CLI](../../plotter/README.md) exposes plotting, file inspection, GPU device selection and development proving.

```rust,no_run
use dg_xch_pos2::{params::ProofParams, compact::CompactPlot, plotting::PlotLimits};
use std::sync::atomic::AtomicBool;

# fn main() -> Result<(), std::io::Error> {
let params = ProofParams::new([42; 32].into(), 18, 2, false)?;
let cancelled = AtomicBool::new(false);
let plot = CompactPlot::build(params, PlotLimits::default(), &cancelled)?;
println!("table entries: {:?}", plot.table_counts);
# Ok(())
# }
```

Enable `vulkan` for adapter enumeration and `vulkan::Hasher::for_params`. Pass that engine to `CompactPlot::build_with_engine` or `solver::solve_with_engine`. Both CPU and GPU hash engines process bounded batches; long-strength hashes are split without changing their results. Hardware Vulkan on Linux/Windows provides the AMD path; no native macOS Vulkan availability is assumed. CUDA supplies a Rust implementation of the same hash interface in the separate plotter helper. The independent `resident` feature exposes `compact::resident::{Backend, build, memory_required}` without Vulkan dependencies; `vulkan` enables it automatically, and the CUDA helper enables it directly.

`CompactPlot::build_with_engine` first offers the engine's optional `build_compact` hook. CUDA and Vulkan use it for little-endian k28 strength-2 resident plotting when memory and device limits permit. Full-resident Vulkan additionally requires subgroup support, 14 storage-buffer bindings, 32 KiB of workgroup storage and 256-thread workgroups; the earlier resident-matching path needs nine storage-buffer bindings. Both Vulkan paths require a binding of at least `1 GiB + 4 bytes` and divide tables into at most five 1 GiB shards. CUDA stores each table in one allocation. Both use a dense matching index and bounded 64 MiB transfers. Unsupported configurations retain a lower-memory resident or GPU-hash path; failures after execution begins propagate to the caller.

`vulkan_full::build_device` returns an optional GPU-resident plot after checking managed budgets and device limits. Its `packed_chunks` reader computes boundaries, deltas and packed stubs on the GPU and prefetches bounded readbacks while the caller compresses/writes prior chunks. `download` instead returns a host `CompactPlot`. File callers should use `dg_xch_plotter::vulkan::create` to preserve GPU residency through packing. The standard k28 full-resident path fits a 12 GiB managed-memory budget; this is not a whole-process bound or a guarantee of available VRAM. Vulkan has no portable free-VRAM check in this implementation.

`solver` recovers a complete proof from a validated quality chain by scanning x values and retaining only candidates consistent with the stored fragment prefixes. It does not allocate the complete plotting tables. Every returned proof is independently validated. `PlotReader` in the plotter package handles compressed disk or in-memory files. Quality discovery can therefore happen without generating a proof for every candidate.

Callers must set work, entry and memory budgets appropriate for their process and supply cancellation flags. `CompactPlot::memory_required` reports the CPU managed-buffer minimum. `compact::resident::memory_required(max_entries)` reports the shared CUDA/Vulkan resident budget, adding `1 GiB + 4 bytes` for the index, three 64 MiB buffers and 1 MiB of allowance to the CPU minimum and accounting for managed host and device memory together. CPU sorting scratch is released before the GPU input table is installed; a usual k28 run needs about 5.2 GiB of managed device buffers. The hash-only k28 strength-2 hybrid instead adds 320 MiB for larger batches and host workers. Budgets exclude complete process RSS and driver allocations. Hash engines implement `Send`; the hybrid path serializes access to each engine. Plot validity alone does not establish network eligibility, difficulty, activation height or pool membership.

## Validation

From the repository root:

```sh
cargo test -p dg_xch_pos2
cargo test -p dg_xch_pos2 --features vulkan --lib
cargo test -p dg_xch_pos2 --test vulkan_shader
```

Shader parsing/validation does not require a GPU. Hardware tests are ignored by default; see the [plotter README](../../plotter/README.md) for explicit low-k CPU/GPU comparisons. Hardware execution and production-scale validation must not be inferred from a successful compile.

Run the bounded k28 GPU-resident checks sequentially on an explicitly selected hardware adapter:

```sh
for module in vulkan_radix vulkan_full vulkan_packing; do
  DGX_VULKAN_TEST_DEVICE=0 cargo test --locked --release \
    -p dg_xch_pos2 --features vulkan --lib "$module::" -- \
    --ignored --nocapture --test-threads=1
done
```

These checks cover stable GPU sorting, hierarchical prefix carries through 289 chunks without allocating a full plot, generation/matching parity, fragment download, packed delta/stub bytes, invalid inputs and cancellation. They passed on an AMD RX 6800 XT using RADV and an NVIDIA RTX A4000 using Vulkan. The AMD full k28 strength-2 pipeline also passed byte-for-byte comparison against a fresh pinned Chia plot and a complete reference-reader scan. Its three-run median was 6.38 seconds including durable writes; see the plotter README for hardware, limits and phase timings. This does not certify every adapter or provide GPU sanitizer coverage.

The plotter also contains an ignored, release-only k28 strength-2 benchmark. Its [reproduction instructions](../../plotter/README.md#reproducing-k28-benchmarks) cover compile-only preparation, matching plot identities, explicit GPU selection, sequential execution and durable output. The comparison target is the pinned `chia-pos2 0.6.0` CPU reference, with optimized C++ and FSE builds, not a proprietary GPU client. Phase timings are enabled with `DGX_POS2_PROFILE=1`; they should not be interpreted as disjoint CPU/GPU execution time when host work overlaps device calls.
