# dg_xch_pos2

Native Rust PoS2 hashing, fragments, quality chains, verification, and RAM plotting. Chia's implementation is not a runtime dependency.

## Use in Rust

From a top-level workspace package:

```toml
[dependencies]
dg_xch_pos2 = { path = "../proof_of_space/pos2" }
```

Enable `vulkan` for hardware compute, or `resident` for backend-independent GPU orchestration.

```rust,no_run
use dg_xch_pos2::{compact::CompactPlot, params::ProofParams, plotting::PlotLimits};
use std::sync::atomic::AtomicBool;

fn main() -> Result<(), std::io::Error> {
    let params = ProofParams::new([42; 32].into(), 18, 2, false)?;
    let cancelled = AtomicBool::new(false);
    let plot = CompactPlot::build(params, PlotLimits::default(), &cancelled)?;
    println!("table entries: {:?}", plot.table_counts);
    Ok(())
}
```

This small mainnet-domain example is for development, not a farmable mainnet plot.

## Main interfaces

- `CompactPlot` keeps compact entries and final fragments instead of full witnesses.
- `build_with_engine` accepts a CPU or GPU hash engine; `NativePlot` retains full witnesses for reference work.
- `solver` recovers qualifying proofs from fragments without rebuilding full plotting tables.
- `compact::resident` shares backend orchestration and budget accounting.
- `vulkan_full::build_device` keeps k28 strength-2 tables on the GPU; its packed-chunk reader feeds the plotter's shared writer.

The pinned format supports even k18–k32 and strengths 2 through `k - section_bits - 1`, with separate mainnet/testnet hash domains. Vulkan uses WGSL kernels; the separate CUDA helper uses Rust kernels.

Always supply cancellation and realistic work/memory limits. `CompactPlot::memory_required` reports managed CPU buffers, not total process memory. Unsupported resident configurations can use GPU hashing with host matching; execution errors do not silently fall back to CPU.

For installation, file creation, hardware requirements, and validation results, see the [plotter](../../plotter/README.md). Network eligibility also depends on activation, difficulty, and plot filters.
