# dg_xch_plotter_cuda

Experimental NVIDIA backend for [dg_xch_plotter](../README.md). GPU kernels and host orchestration are Rust. The package uses cuda-oxide at revision `b9847e9515ed3a23096f22567d3eaf0a6e3e440c` and a separate workspace so ordinary node builds do not require CUDA or a nightly compiler.

## Status

Generation, matching/counting, emission, Feistel fragment encoding and sorting execute on the GPU. The CPU performs prefix offsets, independent witness verification, challenge search and file writing. The implementation retains full tables and is a correctness baseline, not a production-k28 plotter or compact disk solver. AMD cards use the parent package's Vulkan backend, not this package.

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

`--probe-device` initializes the selected CUDA context and returns a versioned device-name response for the desktop's automatic backend selection. It does not run kernels or benchmark CUDA against Vulkan; a successful probe does not prove that a binary compiled for a different GPU architecture can plot. Rebuild older helpers to expose this command. Native CUDA remains separate from Vulkan and is preferred by the desktop's Auto vendor policy when this probe succeeds. Explicit backend selection remains available. CUDA-oxide ARM build/runtime support has not been established here; the parent CPU path does not require CUDA.

Proof reconstruction rebuilds every table and compares the whole file before returning independently verified proofs. It is not an efficient harvester. Errors do not trigger a CPU fallback. Cancellation is cooperative between dispatches, not GPU preemption. Memory budgets are managed-buffer bounds, not whole-process or driver memory guarantees.

## Validation

Run hardware tests explicitly after ordinary workspace checks:

```sh
CUDA_TOOLKIT_PATH=/path/to/cuda cargo oxide test --arch sm_86 -- \
  --release gpu_tables_match_cpu_witnesses -- --ignored --nocapture

CUDA_TOOLKIT_PATH=/path/to/cuda cargo oxide test --arch sm_86 -- \
  --release gpu_reconstructed_plot_proves_cpu_created_file -- --ignored --nocapture

DGX_POS2_REFERENCE_BIN=/path/to/reference-writer CUDA_TOOLKIT_PATH=/path/to/cuda \
  cargo oxide test --arch sm_86 -- \
  --release gpu_file_matches_chia_byte_for_byte -- --ignored --nocapture
```

The reference-writer setup is described in the [parent README](../README.md). These tests cover low-k table/witness equivalence, development proving and canonical file bytes. Production-size workloads, compute-sanitizer coverage and network farming remain separate work.
