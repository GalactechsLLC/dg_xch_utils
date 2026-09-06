# Performance implementation and test guide

This change implements the localized optimizations from [the review](performance-optimization-review.md). The [Grafana baseline](performance-baseline-2026-09-05.json) predates these changes; its observed throughput increase is not a result of this implementation.

Additional live evidence: [pre-restart snapshot at 14:11 PDT on September 5](performance-prerestart-2026-09-05-1411-PDT.md), including the raw two-hour metric history for the next comparison.

Post-reset assessment: [20:44 PDT performance follow-up](performance-followup-2026-09-05-2044-PDT.md). Approximately matched-height throughput is 4.1% lower, with higher body CPU despite smaller VDF/archive CPU per job; performance acceptance remains pending isolated measurement and deployed-build verification.

## Implemented changes

| Review ID | Implementation |
|---|---|
| C1 / C2 | Reuse a CLVM runtime within the generator-output spend loop, update the remaining cost before each puzzle, and borrow puzzle/solution trees. Runtime reset still clears logical allocation counters and stacks between spends. |
| C3 | Preserve interned atoms that precede an arena checkpoint; remove entries pointing into rewound atom storage. Ghost accounting remains unchanged. |
| V1 | Use fused serial VDF checks for weight-proof segments and schedule the outer segment batch on the shared validation pool, retaining reserved persistence capacity. Ordinary latency-oriented VDF APIs remain available. |
| V2 | Build even powers in each VDF window table by squaring an earlier power, replacing seven general multiplications per table. |
| V3 | Share each full memo key between the lookup and recency indexes through `Arc`; cache hits reuse the resident key rather than cloning its bytes. Capacity and full-key equality remain unchanged. |
| P1 | Read quality-comparison integers without temporary bit readers, extract F1 bits directly from a reusable ChaCha byte buffer, use a fixed 64-value `fx` array, reserve decoded proof capacity, and flatten proof bytes directly into one output buffer. |
| P2 | Reuse the plot-filter digest for challenge verification and filter checking; test prefix bytes and remaining high bits without expanding a 256-element boolean array. |
| P3 | Use the existing AES batch kernel for the eight `g` inputs in each v2 proof fragment. Preserve testnet transformation, round count, and subsequent pairing/chain checks. |
| S1 | Add `ChiaSerialize::append_bytes`, implement it for primitives/containers and hot byte/program types, and generate shared-buffer serialization in `ChiaSerial`. Version-sensitive PoSpace encoding also appends directly. |
| B1 | Add an opt-in `release-perf` profile using ThinLTO, one codegen unit and profiling symbols. |
| X1 | Integrate weight-proof concurrency with the existing CPU budget. Queue/cache size experiments use existing CLI settings; defaults and the running node were not modified. |

The append API appends to the supplied buffer and does not roll back earlier bytes if a later field fails. Callers must discard or truncate failed output. Existing implementations that only implement `to_bytes` continue to work through the default fallback. Handwritten serializers without an override can still allocate temporary buffers. Opaque serialized CLVM retains its original bytes, including back-references.

The runtime is reused only within one block's spend loop, so its largest arena allocation is released when that call finishes. It does not retain attacker-controlled high-water capacities indefinitely in thread-local storage. Arena interning still uses full byte keys; this change does not introduce unchecked fingerprints or change consensus limits.

Cache sharding/capacity increases, a replacement interning representation, full proof-metadata redesign, PGO training, native-only CPU instructions and per-job telemetry batching remain profiling-dependent experiments. The review's live measurements did not establish a benefit for those broader changes. Cache request-key construction still allocates; this implementation removes the extra owned key copy in recency bookkeeping. Weight-proof work shares validation capacity but is not individually counted by the existing `compute::map` phase counters.

## Build for the node comparison

First compare the source changes with the same release profile, CLI arguments, features, machine and storage as the baseline:

```sh
cargo build --release -p dg_xch_cli --bin dg --features sqlite,coin-index,hint
```

Run `target/release/dg full-node` with your existing node arguments. Then test the build-profile change separately:

```sh
cargo build --profile release-perf -p dg_xch_cli --bin dg --features sqlite,coin-index,hint
```

That binary is `target/release-perf/dg`. The existing Dockerfile continues to use the regular release profile. The optimized profile is portable: it does not set `target-cpu=native`, remove frame pointers, change panic semantics, or enable inner blst threading. ThinLTO can increase build time and memory.

For the queue experiment, hold worker count, writer-cache budget, storage and replay interval fixed. Compare your existing `--prefetch-memory-mb` setting with `4096` and `2048` in separate runs. This option also feeds prefetch configuration; observe fetch/head availability as well as reorder-buffer memory. Keep `--sqlite-writer-cache-mb` unchanged during that comparison. Lower memory is useful only if throughput and tail latency remain acceptable.

Annotate deployment/reset times and compare the same height interval. Watch blocks/min, CPU seconds/block, confirmation/writer-hold occupancy, VDF/body CPU, queue residency and RSS. Exclude the startup interval and update gaps. Do not interpret a warmed verification-cache microbenchmark as a VDF arithmetic speedup.

## Portable benchmarks

These use committed fixtures or deterministic inputs and need no plot disk or live services:

```sh
cargo bench -p dg_xch_core --bench clvm
cargo bench -p dg_xch_core --bench serialization
cargo bench -p dg_xch_pos --bench verification
cargo bench -p dg_xch_vdf --bench verification
```

The serialization benchmark encodes a real 32-block wire fixture into fresh and reused buffers. PoSpace measures valid and tampered K41 proofs. The VDF benchmark calls uncached single/fused form exponentiation with 264-bit exponents; it isolates arithmetic and does not include discriminant search, proof parsing, or verification-cache hits. The existing CLVM benchmark covers block-generator fixtures and runtime execution. Use the same profile when comparing results.

## Correctness coverage

New regression cases cover runtime reuse after cost failure, checkpoint survival and discarded-handle reuse, plot-filter parity for every `i8` prefix, F1 bit extraction across ChaCha boundaries, scalar/batched AES parity, reference VDF window powers, real mainnet wire append parity across protocol versions, nested primitive/container encoding, and append fallback/error semantics. Existing golden CLVM, real PoSpace/VDF fixtures and node adversarial/memory tests provide additional checks.

### Validation results

All 417 selected tests passed in the debug test profile, with no failures or ignored tests:

| Coverage | Passed |
|---|---:|
| Core, PoSpace, VDF and weight-proof library tests | 287 |
| Core serialization, real wire fixtures, CLVM differential/invariant/vector, generator and compute-pool integration tests | 83 |
| PoSpace forward propagation, v2 reference proofs and real block proof | 15 |
| VDF primality differential and proof integration tests | 19 |
| Node golden condition digests, adversarial limits and allocation leak gate | 13 |

The seven-test node leak gate completed in approximately 8.6 minutes in the debug profile. It covers reference generators, failures, concurrent validation, cost-maxed many-spend generators and mempool admission; its allocation thresholds were not changed.

The serialization and macro crate test/doc-test commands also completed successfully, although those crates contain no standalone tests. Their behavior is exercised by the core regression cases. The CLI passed `cargo check -p dg_xch_cli --bin dg --features sqlite,coin-index,hint --offline`. All six cases in the three new benchmark targets passed smoke execution; these are correctness checks, not measured speedups. Changed Rust files were formatted and `git diff --check` passed.

This is targeted validation, not the entire workspace test suite. Neither release profile was built or benchmarked as part of these checks. Live throughput and memory acceptance remain for the controlled node comparison above; no deployment or service restart was performed.
