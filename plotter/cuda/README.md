# dg_xch_plotter_cuda

Optional NVIDIA PoS2 backend. Kernels and host orchestration are Rust, using pinned cuda-oxide revision `b9847e9515ed3a23096f22567d3eaf0a6e3e440c`.

## Install and connect

Install the application from the repository root:

```sh
cargo install --path cli --locked
dgx init
```

The CUDA helper needs a separate compiler step; ordinary Cargo installation alone does not compile its GPU kernels. Install the NVIDIA driver, CUDA toolkit, and the pinned [cuda-oxide toolchain](https://github.com/NVlabs/cuda-oxide/tree/b9847e9515ed3a23096f22567d3eaf0a6e3e440c), including its LLVM/Clang prerequisites.

From `plotter/cuda`:

```sh
CUDA_TOOLKIT_PATH=/path/to/cuda cargo oxide build --arch sm_86 -- --release
```

This directory pins `nightly-2026-08-28`. Choose your card's architecture; `sm_86` is for the RTX A4000. Place the resulting `dg_xch_plotter_cuda` executable in your Cargo bin directory, then launch `dgx gui` and select its absolute path in Settings. The desktop does not download helpers or search for them automatically.

For the farmer, set `backend: cuda`, the CUDA device ordinal, and the same absolute `cuda_helper` path. No private farming keys are passed to the helper.

## Pipeline and limits

At k28 strength 2, the full-resident path performs generation, radix sorting, matching, filtering, and output packing on the GPU. Only packed output and small control buffers return to the host; FSE compression and durable file writing remain on the CPU.

The standard path needs roughly 10.2 GiB of managed VRAM plus driver overhead; allow a 12 GiB managed budget. Smaller budgets or other parameters use resident matching or GPU hashing with host work. Errors after execution starts do not retry on CPU.

The device probe checks a GPU hash against the CPU result before selection. It is not a benchmark. Proof recovery uses bounded work and independently verifies results. Cancellation waits for the current dispatch; it is not GPU preemption.

The helper is separate so ordinary CPU/ARM builds do not need CUDA or nightly Rust. CUDA-oxide support on ARM has not been established here.

## Validation

Sequential RTX A4000 k28 strength-2 runs had a 9.23-second median including durable writes. All three files matched the pinned Chia reference byte-for-byte. This does not compare with Chia's GPU plotter or establish every strength/device combination.

The ignored hardware tests cover hashing, table generation, radix sorting, packing, and proof recovery. Run them individually on an idle GPU and monitor temperatures. See the [parent package](../README.md#validation-and-benchmarks) for the shared validation scope.

[Plotter](../README.md) · [Farmer](../../farmer/README.md)
