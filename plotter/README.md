# dg_xch_plotter

Native proof-of-space v2 RAM plotting, file inspection and fragment-based proving. The file writer and solver are implemented in this repository; Chia's implementation is used only as an external test oracle.

## Status

For application use, install `dgx`, run `dgx init`, then use `dgx plotter <subcommand>`. `dgx plotter -- --help` lists the CLI and `dgx plotter create --help` lists creation options. Direct Cargo commands below are development and validation entry points. Plotting needs no node connection or wallet synchronization. The desktop can obtain public plotting keys from an encrypted account without opening a wallet session.

The default CPU pipeline creates format-1 fat plots using two compact 16-byte-entry tables in RAM, retaining only final fragments. It targets the pinned `chia-pos2 0.6.0` format: even k18 through k32, both pool-public-key and portable-contract memos, all indices/meta groups, and separate mainnet/testnet hash domains. Strength ranges from 2 to `k - section_bits - 1`, where `section_bits` is 2 below k28 and `k - 26` otherwise. This is a versioned compatibility target, not a claim about future Chia formats or different strength numbering.

Plot creation needs no intermediate disk tables. The final file is published atomically without overwriting an existing plot. Library callers can use `create_in_memory` or `create_in_memory_with_engine` to keep the complete file in a bounded byte buffer, and `PlotReader::from_reader(Cursor::new(bytes), ...)` to read it. Checkpoint/resume, external sorting and format-2 Benes plots are not implemented; the pinned reference writer produces format 1.

Three execution paths are available:

| Backend | GPU work | Host work | Requirements |
| --- | --- | --- | --- |
| CPU | None | Entire pipeline in Rust | Stable Rust |
| Vulkan | Resident k28 strength-2 generation, radix sorting, matching, filtering, fragments and output packing when device limits permit; otherwise resident matching or AES hashing | FSE compression and file writing; lower-memory paths also sort or match on the host | `vulkan` feature; hardware Vulkan compute adapter |
| [CUDA](cuda/README.md) | Native Rust k28 strength-2 generation, radix sorting, matching, filtering, fragments and output packing when VRAM permits; otherwise resident matching or AES hashing | FSE compression and file writing; lower-memory paths also sort or match on the host | NVIDIA GPU and pinned cuda-oxide toolchain |

For k28 strength 2, the CPU path uses eight-lane hardware AES batches, bounded parallel matching and a safe Rust radix sort. The fully GPU-resident CUDA and Vulkan paths keep intermediate tables and sorting on the GPU and download packed output for the common writer. Their lower-memory resident-matching paths keep sorted inputs and the matching index on the device but return intermediate tables for CPU sorting; these paths share `dg_xch_pos2::compact::resident` orchestration and budget accounting. Other configurations retain the GPU-hash/host-matching pipeline.

The non-resident CUDA and Vulkan k28 strength-2 paths use at most eight host matching workers and 262,144-input hashing batches. Explicit GPU selection does not fall back to CPU hashing. Transfers, synchronization and host work still affect throughput, so backend availability is not a speed ranking.

Vulkan supports the AMD/NVIDIA driver interface without vendor-specific kernels. Its compute shaders are WGSL, not Rust device code; there is no C++ kernel or JavaScript runtime. Neither GPU path silently falls back to CPU. Low-k reconstruction has been checked on an AMD RX 6800 XT and NVIDIA RTX A4000 against the same CPU-created canonical plot and matching proof results. Larger-plot checks are listed below; none certify every driver and device.

The desktop supports Auto, CUDA, and Vulkan preferences. Auto prefers a successfully probed trusted CUDA helper, otherwise non-NVIDIA hardware Vulkan, then NVIDIA Vulkan. This is a vendor heuristic, not a comparative performance result. CUDA and Vulkan device ordinals are never treated as interchangeable. The shared `backend` policy is hardware-independent and unit tested; command-line tools retain explicit CPU/Vulkan selection here and explicit CUDA in the separate helper. The Rust CPU path stays available without GPU toolchains, including for ARM builds; CUDA-oxide ARM support is not claimed.

## Build and run

Run commands from the repository root:

```sh
cargo build -p dg_xch_cli --release
./target/release/dgx init
cargo run -p dg_xch_cli --release -- plotter --help
```

Supply your farmer's 48-byte public key and either a 48-byte pool public key or a 32-byte pool-contract puzzle hash, all hexadecimal. Do not supply a mnemonic or private key. The writer generates a fresh local plot key and refuses to overwrite an existing file.

```sh
cargo run -p dg_xch_cli --release -- plotter create \
  --output /path/to/development.plot \
  --farmer-key "$FARMER_PUBLIC_KEY" --pool-key "$POOL_PUBLIC_KEY" \
  --k 18 --strength 2 --experimental-size \
  --memory-mib 512 --max-entries 2097152 --max-work 100000000

cargo run -p dg_xch_cli --release -- plotter inspect /path/to/development.plot

cargo run -p dg_xch_cli --release -- plotter prove-plot /path/to/development.plot \
  --challenge "$CHALLENGE_HEX"
```

Use `--contract "$POOL_CONTRACT_HASH"` instead of `--pool-key` for a portable memo. A portable memo does not create a PlotNFT or pool registration on-chain. Use `--testnet` consistently when creating and proving testnet-domain plots: the hash domain is not stored in the file. An arbitrary challenge may produce no proofs.

`inspect` validates structure and identity, not the entire compressed payload. `prove-plot` reads only challenge-relevant chunks, recovers the missing x values from their fragments, and independently validates each complete proof. It does not rebuild full plotting tables or certify unread chunks. Recovery still hashes the plot's x-space and must finish within a deployment's farming deadline; a valid proof is not evidence of network submission. The older `ReconstructedPlot` API remains available for whole-file canonical comparison.

## RAM and work budgets

All k sizes use compact RAM tables, but larger plots require larger machines. A conservative two-table CPU plan is approximately 9.04 GiB for k28, 36.04 GiB for k30 and 144.04 GiB for k32, plus runtime/driver overhead. `CompactPlot::memory_required(k, max_entries)` returns the CPU managed-buffer minimum. A machine with 64 GiB of RAM cannot validate a fully in-memory k32 run. Large strength settings increase computation exponentially; accepting a parameter does not make it practical on every machine.

The resident-matching CUDA and Vulkan k28 strength-2 paths budget managed host and device buffers together: the CPU minimum plus a `1 GiB + 4 bytes` matching index, three 64 MiB output/readback/upload buffers and 1 MiB of additional allowance. `dg_xch_pos2::compact::resident::memory_required(max_entries)` exposes this shared budget. CPU sorting scratch is released before installing the GPU input table. A usual k28 table needs about 5.2 GiB of managed device buffers, plus driver overhead. The hash-only k28 strength-2 hybrid instead adds 320 MiB to the CPU minimum for larger batches and host workers. If the resident budget does not fit, the selected GPU backend can use the hybrid path when its budget fits; otherwise plotting fails.

CUDA can instead keep both tables and radix sorting on the device, using approximately 10.2 GiB of managed VRAM plus driver overhead at the standard capacity. A 12 GiB managed-memory budget covers this path. Its CLI downloads packed output, while callers requesting an in-memory `CompactPlot` receive final sorted fragments. See the [CUDA helper](cuda/README.md) for selection and validation details.

Vulkan also keeps both tables and sorting on the device when its full-resident preflight passes. This path requires subgroup support, 14 storage-buffer bindings, a binding of at least `1 GiB + 4 bytes`, 32 KiB of workgroup storage and 256-thread workgroups. It uses five shards per table to respect buffer-binding limits. The 12 GiB managed-memory budget covers the standard k28 capacity; enough physical VRAM must also be available. Vulkan checks managed budgets and adapter limits, not current free VRAM. Allocation or execution failures are returned without restarting on another backend. File creation in the CLI and desktop uses GPU delta/stub packing and two bounded readback buffers; `CompactPlot` callers instead download final sorted 64-bit fragments.

For a k28 strength-2 run on a machine with sufficient free RAM:

```sh
cargo run -p dg_xch_cli --release -- plotter create \
  --output /path/to/k28.plot \
  --farmer-key "$FARMER_PUBLIC_KEY" --contract "$POOL_CONTRACT_HASH" \
  --k 28 --strength 2 \
  --memory-mib 12288 --max-entries 310000000 --max-work 10000000000
```

The default 512 MiB limit intentionally does not authorize multi-gigabyte plotting. Limits fail closed rather than spilling tables to disk or silently switching backends. Spare managed RAM is used for a denser matching index; tighter budgets use a smaller index and more searching. Work is charged in 16-round AES evaluations; the solver also charges bounded matching work. `CompactPlot::minimum_work` exposes the generation/first-target lower bound, not an estimate of the entire run. Host hashing, matching and sorting use Rayon; set `RAYON_NUM_THREADS` to limit host parallelism. Budget additional memory for thread stacks, libraries, GPU drivers and the operating system.

## AMD and Vulkan

Install a working hardware Vulkan driver on Linux or Windows. The process must be able to access its GPU device; a software Vulkan implementation is deliberately rejected. Native macOS Vulkan is not assumed by this backend.

```sh
cargo build -p dg_xch_cli --release
cargo run -p dg_xch_cli --release -- plotter devices

cargo run -p dg_xch_cli --release -- plotter create \
  --backend vulkan --device 0 \
  --output /path/to/development-amd.plot \
  --farmer-key "$FARMER_PUBLIC_KEY" --pool-key "$POOL_PUBLIC_KEY" \
  --k 18 --strength 2 --experimental-size

cargo run -p dg_xch_cli --release -- plotter prove-plot \
  /path/to/development-amd.plot --challenge "$CHALLENGE_HEX" \
  --backend vulkan --device 0
```

The device ordinal comes from `devices`; check it again after changing drivers or hardware. Launch separate processes with distinct ordinals for separate GPUs. The fully GPU-resident k28 strength-2 path is preferred when the requirements above are met. Otherwise, the earlier resident-matching path needs at least nine storage-buffer bindings and a binding of at least `1 GiB + 4 bytes`. It divides GPU input into at most five 1 GiB shards and uses bounded 64 MiB output/readback/upload buffers. Unsupported parameters, device limits or resident memory budgets retain GPU hashing rather than switching to CPU. Errors after execution starts are returned, not hidden by a fallback.

The compact Vulkan and CUDA paths accept the full pinned strength range and split hashing into bounded dispatches with cancellation checks. Cancellation cannot preempt an already running GPU dispatch or CPU sort. Memory limits cover managed table and batch buffers, not driver allocations or total process RSS.

The farmer's `DiskHarvester` separates cheap quality discovery from proof recovery and accepts the same `HashEngine` interface as plotting. CPU and Vulkan are available in process; the CUDA helper exposes `--prove-plot`. The desktop uses fragment recovery rather than full-table reconstruction. Production signage submission remains separate from these plotting/proving APIs.

## Validation

Ordinary checks do not require a GPU:

```sh
cargo test -p dg_xch_pos2 --test vulkan_shader
cargo test -p dg_xch_pos2 --features vulkan --lib
cargo test -p dg_xch_plotter --release
cargo test -p dg_xch_plotter --release --test compact -- --include-ignored
```

Run hardware tests explicitly, after normal checks, on an otherwise idle machine:

```sh
cargo test -p dg_xch_pos2 --release --features vulkan \
  vulkan_hashes_match_native_for_all_supported_rounds -- --ignored --nocapture
cargo test -p dg_xch_plotter --release --features vulkan \
  --test vulkan -- --ignored --nocapture
```

The hardware tests reject missing/software devices, compare native and GPU witnesses and reconstruct CPU plots byte-for-byte through the GPU backend. They default to ordinal 0; set `DGX_VULKAN_TEST_DEVICE` to select another ordinal reported by `devices`. They are not performance benchmarks or production-k tests.

Low-k CPU/CUDA/Vulkan comparisons have passed on an AMD Radeon RX 6800 XT using RADV and an NVIDIA RTX A4000. An AMD k18 strength-9 plot also matched Chia byte-for-byte. CPU, AMD Vulkan and NVIDIA CUDA k28 strength-2 files matched a freshly generated pinned Chia reference file byte-for-byte. Recovered k28 proofs from CPU, Vulkan and CUDA were accepted by the pinned Chia validator and file reader. An AMD k30 strength-2 portable plot also passed creation, AMD proof recovery and an independent Chia scan of all 16,384 chunks. Its 4,222,945,483-byte file contained 1,073,955,558 fragments. Bounded AMD kernel checks through k32 and a synthetic k32 file-format boundary test passed; neither is a full k32 plot. Full k32 creation remains outstanding. This is not exhaustive certification of every GPU, driver or strength.

For direct Chia file comparisons, build `tests/reference_writer.cpp` externally against the source bundled with `chia-pos2 0.6.0` (upstream revision `b0da7aa7bec3974833d651173a6d9953c21eb808`). Use C++20, its `cpp` and `fse/fse` include paths and the reference FSE static library. Then run:

```sh
DGX_POS2_REFERENCE_BIN=/path/to/reference-writer \
  cargo test -p dg_xch_plotter --release \
  plot_files_match_chia_byte_for_byte_at_k18 -- --ignored --nocapture
```

k18 is the minimum supported by the pinned reference API. To validate a larger mainnet-domain plot without creating another copy:

```sh
DGX_POS2_TEST_PLOT=/path/to/k28.plot cargo test -p dg_xch_plotter --release \
  --test production -- --ignored --nocapture
```

Add `--features vulkan` and `DGX_VULKAN_TEST_DEVICE=0` to recover its proof on the selected Vulkan GPU, or set `DGX_CUDA_TEST_BIN` to the separately built CUDA helper. Set `DGX_POS2_REFERENCE_BIN` as well to validate the recovered proof and read its fragments with the pinned Chia implementation. Only plotting-related tests are needed for these checks; they do not establish end-to-end block production. See the [repository README](../readme.md) for the rest of the stack.

For k30 proof recovery, also set `DGX_POS2_TEST_MEMORY_MIB=1024`, `DGX_POS2_TEST_MAX_ENTRIES=8388608` and `DGX_POS2_TEST_MAX_WORK=10000000000`; the test's default limits are sized for smaller plots. These variables control proof recovery, not plot creation. The ignored `vulkan_bounded_k18_through_k32_kernels_match_cpu` test checks sampled GPU operations across all supported sizes, including k32 boundary values and split strength-9 dispatches. It does not create large plots or execute the exponential table-one work at maximum strength.

The external reference helper also accepts `<plot> scan false` (`true` for the testnet hash domain). This reads every chunk through Chia's decoder and checks offsets, fragment ordering and chunk membership without rebuilding plotting tables. It establishes file readability, not proof validity or byte-for-byte equality with a freshly generated reference plot.

## Reproducing k28 benchmarks

Run one job at a time on an otherwise idle machine. These comparisons target the CPU reference bundled with `chia-pos2 0.6.0`, not a proprietary GPU plotter or a different Chia release. The ignored benchmark uses k28 strength 2, a 12 GiB managed-memory budget, 310 million maximum entries and 100 billion work evaluations. Its default keys are deterministic test fixtures, not keys for production farming.

The fully GPU-resident Vulkan path took 6.453268, 6.383290 and 6.175400 seconds in three sequential k28 strength-2 mainnet runs on a Ryzen 9 5950X with 32 host threads and an RX 6800 XT using RADV. The median was 6.38 seconds, including durable file and directory synchronization. All 988,326,103-byte files matched a freshly generated pinned Chia reference byte-for-byte. Chia's reader also scanned every chunk and all 268,406,568 fragments of the final output. Peak AMD temperatures across these runs were 81°C edge and 96°C hotspot; workloads did not overlap.

The median Vulkan run spent 0.016 seconds in setup, 4.394 seconds building device tables, and 1.974 seconds packing, compressing, writing and synchronizing the file. GPU sorting accounted for 1.012 seconds and matching for 3.201 seconds. Packing readback was 1,140,758,388 bytes (about 1.06 GiB), with no intermediate table readbacks. The standard-capacity build budget was 11,075,423,768 managed bytes, including 11,041,869,336 bytes of device buffers. FSE compression and durable file I/O remain on the CPU.

The previous CPU-sort resident Vulkan path had a 22.70-second median on the same hardware and fixture, measured in an earlier session. The new median is about 3.56 times as fast as that historical baseline. The earlier session also measured a 45.39-second optimized Chia CPU reference median. Chia was regenerated for byte comparison here, not rebenchmarked. These results do not compare with Chia GPU plotting or characterize other strengths.

The fully GPU-resident CUDA path subsequently took 8.96, 9.28 and 9.23 seconds in three sequential runs on the same CPU with 32 host threads and an RTX A4000 (`sm_86`, driver 610.57.04). The 9.23-second median includes durable file and directory synchronization. All 988,326,103-byte k28 strength-2 mainnet files matched a freshly generated pinned Chia reference byte-for-byte. This is about 2.78 times as fast as the earlier CPU-sort CUDA path's 25.67-second median on the same hardware and fixture, measured in a previous session. That earlier session measured a 50.39-second Chia CPU reference median; Chia was not rebenchmarked for this CUDA change. Different GPUs and session conditions mean these AMD and NVIDIA results are not a controlled backend ranking, and none compares against Chia GPU plotting. See the [CUDA helper README](cuda/README.md#validation) for phase timings, transfer accounting and reproduction steps.

Compile before timing. Run commands from the repository root; set `BENCH_BIN` to the `src/lib.rs` test executable printed by Cargo, not the CLI executable. Replace `/mnt/nvmep1/tmp` with your existing scratch directory if needed:

```sh
cargo test --locked --release -p dg_xch_plotter --features vulkan --lib --no-run
BENCH_BIN=/path/to/the/printed/test-executable
BENCH_DIR="$(mktemp -d /mnt/nvmep1/tmp/dgx-k28-bench.XXXXXX)"
```

For a Linux/x86-64 reference build, set `REFERENCE` to the unpacked pinned crate source. Build both C++ and its FSE C sources with optimization; do not link a debug FSE archive. `-maes` enables hardware AES on this architecture; ARM needs the corresponding crypto-extension flags instead.

```sh
REFERENCE=/path/to/chia-pos2-0.6.0
REFERENCE_BUILD="$BENCH_DIR/reference-build"
mkdir "$REFERENCE_BUILD"
for source in entropy_common fse_compress fse_decompress fseU16 huf_compress huf_decompress hist; do
  cc -O3 -DNDEBUG -DFSE_MAX_MEMORY_USAGE=16 -I"$REFERENCE/fse/fse" \
    -c "$REFERENCE/fse/fse/$source.c" -o "$REFERENCE_BUILD/$source.o"
done
c++ -std=c++20 -O3 -DNDEBUG -maes -pthread \
  -I"$REFERENCE/cpp" -I"$REFERENCE/fse/fse" \
  plotter/tests/reference_writer.cpp "$REFERENCE_BUILD"/*.o \
  -o "$REFERENCE_BUILD/reference-writer"
```

The reference uses the machine's hardware thread count. Set `RAYON_NUM_THREADS` to the same count for the Rust comparison; the example uses 32. Hash-only hybrid matching is capped at eight host workers even when sorting can use the full pool; resident CUDA and Vulkan matching runs on the device instead. Finish each command before starting the next:

```sh
DGX_POS2_BENCHMARK_BACKEND=cpu DGX_POS2_PROFILE=1 RAYON_NUM_THREADS=32 \
  DGX_POS2_BENCHMARK_OUTPUT="$BENCH_DIR/cpu.plot" \
  "$BENCH_BIN" benchmark::k28_plotting --ignored --exact --nocapture --test-threads=1

DGX_POS2_BENCHMARK=1 "$REFERENCE_BUILD/reference-writer" \
  "$BENCH_DIR/cpu.plot" "$BENCH_DIR/reference.plot" false

DGX_POS2_BENCHMARK_BACKEND=vulkan DGX_POS2_BENCHMARK_DEVICE="$VULKAN_DEVICE" \
  DGX_POS2_PROFILE=1 RAYON_NUM_THREADS=32 \
  DGX_POS2_BENCHMARK_INPUT="$BENCH_DIR/cpu.plot" \
  DGX_POS2_BENCHMARK_OUTPUT="$BENCH_DIR/vulkan.plot" \
  "$BENCH_BIN" benchmark::k28_plotting --ignored --exact --nocapture --test-threads=1

cmp "$BENCH_DIR/cpu.plot" "$BENCH_DIR/reference.plot"
cmp "$BENCH_DIR/cpu.plot" "$BENCH_DIR/vulkan.plot"
sha256sum "$BENCH_DIR/"*.plot
```

Set `VULKAN_DEVICE` explicitly to the intended hardware ordinal from `devices` before the GPU command. `DGX_POS2_BENCHMARK_INPUT` copies a mainnet-domain k28 strength-2 plot's identity and memo, keeping all implementations on identical input; omit it only for the deterministic fixture. `DGX_POS2_BENCHMARK_OUTPUT` must name a new file. Backend `cpu-batched` selects the generic hash-engine pipeline for a diagnostic comparison, rather than the optimized CPU path. `DGX_POS2_PROFILE=1` emits phase timings. Resident CUDA and Vulkan emit shared `pos2_resident` phases; their internal GPU work is not measured by the generic hash-call counters. The [CUDA helper README](cuda/README.md#validation) describes its separate benchmark executable.

Reported build and write/sync times exclude compilation and the subsequent checksums. Both writers synchronize the file and its parent directory in benchmark mode. Hybrid host work overlaps GPU calls, so subtracting GPU-call wall time from the build time does not measure total CPU execution time. Repeat sequentially if you need variance estimates, and retain the compiler, driver, device and thread settings with the measurements. Remove only the benchmark directory you created and its contents after comparing the files.
