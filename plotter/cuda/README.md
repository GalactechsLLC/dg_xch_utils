# dg_xch_plotter_cuda

Experimental NVIDIA backend for [dg_xch_plotter](../README.md). GPU kernels and host orchestration are Rust. The package uses cuda-oxide at revision `b9847e9515ed3a23096f22567d3eaf0a6e3e440c` and a separate workspace so ordinary node builds do not require CUDA or a nightly compiler.

## Status

For desktop use, build this helper separately and configure its absolute executable path in Settings after `dgx init`. `dgx gui` does not install or download CUDA tools. Direct helper commands below are development/hardware-validation interfaces, not evidence of mainnet activation.

The compact pipeline supports the same k sizes and strengths as the parent plotter without retaining full witnesses. Its fastest k28 strength-2 path keeps generation, intermediate tables, radix sorting, matching, filtering and final fragment sorting on the GPU. File plotting also computes chunk boundaries, deltas and packed stubs on CUDA. Only small status/metadata buffers and packed output cross PCIe; intermediate tables do not. CPU code performs FSE entropy compression, file writing and durable publication through the shared canonical writer. The in-memory `CompactPlot` interface instead downloads the final sorted 64-bit fragments. GPU kernels, including radix sorting, are Rust; this path does not use CUB or a C++ plotter.

This helper enables the `resident` feature without adding a Vulkan dependency. AMD cards use the parent package's Vulkan backend, which has a corresponding GPU-resident sorting and packing path using WGSL. CUDA retains its native Rust kernels and independent device selection.

GPU-resident plotting requires little-endian k28 strength-2 parameters and sufficient managed memory and free VRAM. It reuses two compact-entry buffers for sorting and matching, a `1 GiB + 4 bytes` matching index and bounded radix metadata. At the standard capacity, managed device allocations need approximately 10.2 GiB, plus driver overhead; a 12 GiB managed-memory budget covers the pipeline. Unneeded buffers are released before final output packing. Packing adds a 208 MiB managed allowance, including a 64 MiB device output buffer and two 64 MiB pinned host buffers; it overlaps the next packed readback with CPU compression and writing of the current batch. The helper checks available VRAM with an additional 64 MiB reserve before allocating. This preflight is not a reservation: later allocation or execution failures propagate without restarting the plot.

When the fully device-resident path does not fit, the earlier resident-matching path can use about 5.2 GiB of device buffers while sorting tables on the CPU. Its combined host/device minimum remains available through `compact::resident::memory_required(max_entries)`. If neither resident path fits, supported workloads retain bounded CUDA hashing with host matching, not CPU hashing. `DGX_POS2_PROFILE=1` identifies the selected path and reports phase timings.

Coalesced radix scatter and replicated shared-memory AES lookup tables are enabled by default. For diagnostic comparisons, `DGX_CUDA_RADIX_COALESCED=0` selects direct scatter and `DGX_CUDA_REPLICATED_AES=0` selects the original AES lookup layout. These are implementation choices, not plot-format changes; neither changes the resulting file.

Other parameters and insufficient resident budgets retain bounded GPU hashing with parallel Rust host matching. At k28 strength 2, this hybrid uses at most eight host matching workers, 262,144-input hash batches and an additional 320 MiB beyond the CPU managed-buffer minimum. Kernels use a shared-memory AES table and prepacked keys. The engine binds its CUDA context on each calling thread; explicit CUDA selection never substitutes CPU hashing. `--full-gpu-tables` retains the original GPU generation/matching/emission/sorting implementation for small plots at strengths 2–8 that fit its entry and VRAM bounds.

## Build and run

Install an NVIDIA driver, CUDA toolkit and the pinned [cuda-oxide](https://github.com/NVlabs/cuda-oxide/tree/b9847e9515ed3a23096f22567d3eaf0a6e3e440c) compiler with its required LLVM/Clang prerequisites. This directory pins `nightly-2026-08-28` with `rustc-dev`, `rust-src` and `llvm-tools`. The process must see the NVIDIA device nodes; sandbox restrictions can otherwise hide a working host GPU.

```sh
cd plotter/cuda
CUDA_TOOLKIT_PATH=/path/to/cuda cargo oxide build --arch sm_86 -- --release

target/release/dg_xch_plotter_cuda --probe-device --device 0

target/release/dg_xch_plotter_cuda \
  --output /path/to/development.plot \
  --farmer-key "$FARMER_PUBLIC_KEY" --pool-key "$POOL_PUBLIC_KEY" \
  --k 18 --strength 2 --device 0 \
  --memory-mib 512 --max-entries 2097152 --max-work 100000000

target/release/dg_xch_plotter_cuda \
  --prove-plot /path/to/development.plot \
  --challenge "$CHALLENGE_HEX" --device 0
```

Choose the actual architecture of your card; `sm_86` is appropriate for the RTX A4000. Public keys are 48-byte hexadecimal values. Use `--contract "$POOL_CONTRACT_HASH"` instead of `--pool-key` for a portable memo. The CLI accepts `--testnet`, `--index` and `--meta-group`; the hash domain must be supplied consistently and is not stored in the file.

`--probe-device` loads the selected CUDA module, checks one small GPU hash against the CPU result, and returns a versioned device-name response. This catches helpers compiled for an incompatible GPU architecture before Auto selects them; it is not a throughput benchmark or a full plotting test. Rebuild older helpers to obtain this check. Native CUDA remains separate from Vulkan and is preferred by the desktop's Auto vendor policy when this probe succeeds. Explicit backend selection remains available. CUDA-oxide ARM build/runtime support has not been established here; the parent CPU path does not require CUDA.

`--prove-plot` reads challenge fragments from the file and recovers missing x values with the bounded CUDA hash engine. Add `--quality "$QUALITY_HEX"` to recover only a previously selected quality; a missing quality fails instead of proving unrelated chains. The live farmer uses this interface with a subprocess deadline and independent output verification. Returned proofs undergo independent CPU verification. The helper does not regenerate the complete file or verify unread chunks. Errors do not trigger a CPU fallback. Cancellation is cooperative between dispatches, not GPU preemption. Memory budgets are managed-buffer bounds, not whole-process or driver memory guarantees. See the parent README for k28/k30/k32 RAM requirements and explicit plotting budgets.

## Validation

Run plotting hardware tests explicitly:

```sh
DGX_CUDA_TEST_DEVICE=0 CUDA_TOOLKIT_PATH=/path/to/cuda \
  cargo oxide test --arch sm_86 -- \
  --release k28_resident_cuda_generation_matching_and_limits_match_cpu -- --ignored --nocapture --test-threads=1

CUDA_TOOLKIT_PATH=/path/to/cuda cargo oxide test --arch sm_86 -- \
  --release gpu_compact_plotting_and_fragment_proving_match_cpu -- --ignored --nocapture

CUDA_TOOLKIT_PATH=/path/to/cuda cargo oxide test --arch sm_86 -- \
  --release gpu_tables_match_cpu_witnesses -- --ignored --nocapture

CUDA_TOOLKIT_PATH=/path/to/cuda cargo oxide test --arch sm_86 -- \
  --release gpu_reconstructed_plot_proves_cpu_created_file -- --ignored --nocapture

DGX_POS2_REFERENCE_BIN=/path/to/reference-writer CUDA_TOOLKIT_PATH=/path/to/cuda \
  cargo oxide test --arch sm_86 -- \
  --release gpu_file_matches_chia_byte_for_byte -- --ignored --nocapture
```

The reference-writer setup is described in the [parent README](../README.md). The bounded k28 resident test checks generation, all three matching tables, fragment encoding, cross-block output and work/output limits against CPU results in both hash domains; it requires an explicit device ordinal. The `cuda_radix_matches_stable_cpu_sort_and_rejects_invalid_inputs` and `k28_cuda_packing_matches_canonical_file_and_rejects_invalid_fragments` tests cover stable device sorting, hierarchical-prefix boundaries, packed canonical bytes, invalid inputs and cancellation. Packing checks include cancellation and injected writer failure while a readback is pending. Run these ignored tests individually with `DGX_CUDA_TEST_DEVICE` set before a full k28 benchmark. Bounded resident-matching and packing tests passed NVIDIA Compute Sanitizer memcheck on the A4000 with zero reported errors; this is not full-pipeline sanitizer coverage. Other tests cover compact-fragment parity, bounded high-round hashing, proof recovery, legacy table/witness equivalence and canonical file bytes. Full network farming and full-scale sanitizer coverage remain separate acceptance gates.

Three sequential fully GPU-resident k28 strength-2 mainnet runs took 8.964327, 9.283825 and 9.229541 seconds on an RTX A4000 (`sm_86`, driver 610.57.04), with a Ryzen 9 5950X and 32 host threads. The median was 9.23 seconds, including file and directory synchronization. Each run started at 70°C and peaked at 85°C, with cooling between runs and no overlapping workloads. All three 988,326,103-byte files matched a freshly generated pinned Chia reference byte-for-byte; the reference reader also scanned all 4,096 chunks and 268,406,568 fragments of the final output.

The median run spent 0.223 seconds in setup, 6.580 seconds building device tables, and 2.427 seconds packing, compressing, writing and synchronizing the file. Within table building, GPU sorting took 0.809 seconds and matching took 5.514 seconds. Packing downloaded 1,140,758,160 bytes, approximately 1.06 GiB including its boundary/status readbacks. Compared with approximately 28 GiB of table-transfer payload in the earlier CPU-sort CUDA path, the new path removes about 96% of PCIe payload, apart from small control transfers. Intermediate tables never leave the GPU. FSE entropy compression and durable file I/O remain on the CPU.

The earlier CPU-sort CUDA path had a 25.67-second median in a previous session on the same hardware and fixture, so the new 9.23-second median is about 2.78 times as fast as that historical baseline. That earlier session measured a 50.39-second Chia CPU reference median; the reference was regenerated for byte comparison here, not rebenchmarked. These results do not measure Chia GPU plotting, establish a controlled CUDA-versus-Vulkan ranking, or characterize other strengths.

For sequential timing, compile with `cargo oxide test --arch sm_86 -- --release --no-run`, then run the printed test executable with `tests::k28_plotting --ignored --exact --nocapture --test-threads=1`. Set `DGX_POS2_BENCHMARK_INPUT` to the same mainnet-domain seed plot used for the CPU/reference comparison, `DGX_POS2_BENCHMARK_OUTPUT` to a new output path, and `DGX_POS2_BENCHMARK_DEVICE` to the CUDA ordinal. Set `RAYON_NUM_THREADS` consistently and optionally enable `DGX_POS2_PROFILE=1`. Compile before timing, monitor temperatures, allow cooling when needed, and do not overlap benchmarks. See the parent README for comparison methodology.
