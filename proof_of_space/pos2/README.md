# dg_xch_pos2

Native Rust proof-of-space v2 hashing, fragment encoding, quality chaining, validation and in-memory plotting primitives. This library does not depend on Chia's PoS2 implementation at runtime.

## Status

The implementation supports the validated even-k domain from 18 through 32 and separates mainnet/testnet hashing. `NativePlot` retains complete witnesses for development proving; it is not a compact disk harvester. Default resource limits deliberately reject production-k28 plotting.

The shared `device` module supplies transformations to native CUDA kernels and the optional Vulkan host pipeline. CUDA device code is Rust. Vulkan offloads AES hashing to a narrow WGSL compute shader; matching, sorting and fragment assembly stay in Rust. Vulkan software adapters are rejected and device errors do not select a CPU fallback.

## Usage

This is a library package, not a daemon. The [plotter CLI](../../plotter/README.md) exposes plotting, file inspection, GPU device selection and development proving.

```rust,no_run
use dg_xch_pos2::{params::ProofParams, plotting::{NativePlot, PlotLimits}};
use std::sync::atomic::AtomicBool;

# fn main() -> Result<(), std::io::Error> {
let params = ProofParams::new([42; 32].into(), 18, 2, false)?;
let cancelled = AtomicBool::new(false);
let plot = NativePlot::build(params, PlotLimits::default(), &cancelled)?;
println!("table entries: {:?}", plot.table_counts);
# Ok(())
# }
```

Enable the `vulkan` feature for `vulkan::adapters()` and `vulkan::build(params, limits, cancelled, ordinal)`. Hardware Vulkan on Linux/Windows provides the AMD path; no native macOS Vulkan availability is assumed. Strengths 2 through 8 are accepted by the bounded development GPU backend. `NativePlot::from_witnesses` independently verifies accelerator output before exposing it as a plot.

Callers must set work, entry and memory budgets appropriate for their process and supply cancellation flags. Budgets cover managed buffers, not complete process RSS. Plot validity alone does not establish network eligibility, difficulty, activation height or pool membership.

## Validation

From the repository root:

```sh
cargo test -p dg_xch_pos2
cargo test -p dg_xch_pos2 --features vulkan --lib
cargo test -p dg_xch_pos2 --test vulkan_shader
```

Shader parsing/validation does not require a GPU. Hardware tests are ignored by default; see the [plotter README](../../plotter/README.md) for explicit low-k CPU/GPU comparisons. Hardware execution and production-scale validation must not be inferred from a successful compile.
