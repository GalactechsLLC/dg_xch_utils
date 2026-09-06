# Workspace performance optimization review

Review date: 2026-09-05. Source baseline: `5dd547575940c8fa591c78ca3875a50d866dbdeb` **plus the working tree**.

Implementation follow-up: see [implemented changes and test instructions](performance-implementation.md). Source observations and line references in this review describe the pre-optimization baseline; the Grafana measurements are retained as historical evidence.

Status: source review, dashboard-definition review, and **live Grafana comparison complete through 2026-09-05 13:10 PDT**. No implementation, configuration, dashboard, or test code was changed for this review. Existing uncommitted SQLite/checkpoint, telemetry, and dashboard work was present and is included as context, not attributed to this review or assumed deployed.

## Executive assessment

For the observed sync workload, prioritize **VDF arithmetic (V2), CLVM runtime reuse (C1), and bounded pipeline/queue tuning (X1)**. VDF accounts for about 74% of instrumented worker CPU across approximately matched heights; CLVM/body work accounts for about 20%. PoSpace allocation removal (P1/P2) remains a promising small patch but lacks dedicated timing. Serial VDF verification inside parallel weight-proof batches (V1) remains a strong bootstrap optimization, not an explanation of the steady sync measurements. A shared-buffer serialization API has broader reach but lower immediate priority because archive CPU is small in these samples.

The new run processes approximately heights 200,000–1,600,000 at **14,545 blocks/min versus 10,634 previously (+36.8%)**, with nearly unchanged instrumented CPU seconds/block. This is an observational comparison, not an isolated code speedup: the SQLite writer cache budget changes from **256 MiB to 4 GiB**, queue residency increases markedly, and the machine was updated. Mean RSS rises from **1.94 to 10.97 GiB** over those windows. In the latest 30-minute sample, confirmation occupies 0.595 elapsed seconds/second, so increasing CPU threads alone is unlikely to resolve the whole critical path.

The workspace already employs many strong techniques: compact arena handles, inline small atoms, streaming condition parsing, shared-subtree tree hashes, fused VDF exponentiation, fixed-width limbs, bounded memoization, batch deduplication, work stealing, and reserved persistence capacity. Recommending these as new work would overstate the remaining opportunity. Blanket branch removal or adding more threads is not the next step.

All proposed optimization benefit estimates below are **qualitative hypotheses**, not measured patch speedups. Grafana establishes phase-level observations, not function-level attribution. No benchmarks or production profiling workloads were run during this documentation-only review; all live access was read-only.

## Grafana baseline and comparison windows

Reviewed [Sync Overview](grafana/dg-xch-sync-overview.json), UID `dgxch-sync-overview`, and [monitoring instructions](monitoring.md). The dashboard uses browser timezone and explicit datasource/job/instance selectors. The checked-in JSON contains queries, not historical samples, and may differ from the deployed dashboard.

Live source: [Grafana Sync Overview](https://grafana.galactechs.com/d/dgxch-sync-overview), dashboard version 8, Prometheus datasource UID `P1809F7CD0C75ACF3`, `job="local_full_node"`, `instance="192.168.0.109:8444"`. Access used the supplied viewer token without recording its contents. The local endpoints were unavailable; the remote dashboard and datasource queries succeeded. The deployed dashboard contains panels 101–144 but lacks the working tree's checkpoint panels 151–155, even though the new run exports those metrics. Dashboard version is not a node build revision.

[Saved comparison evidence](performance-baseline-2026-09-05.json) contains counter endpoints, gauge summaries, query windows, and sampled rate summaries used below. Raw read-only responses were also retained during the session under `/tmp/dg-performance-*.json`; these temporary files are not durable report artifacts. The saved JSON contains no credentials. Query requests used the Grafana datasource proxy `/api/datasources/proxy/uid/P1809F7CD0C75ACF3/api/v1/query_range`.

Assuming the session date and `America/Los_Angeles` timezone apply to the stated times:

| Interval | Local time (PDT, UTC−07:00) | UTC | Treatment |
|---|---|---|---|
| Previous code run | Sep 4, approximately 23:15 to Sep 5, approximately 11:00 | Sep 5, 06:15–18:00 | Previous-run observations; establish actual startup and height range |
| Machine update gap, observed | Sep 5, 10:53:30–11:09:00 | Sep 5, 17:53:30–18:09:00 | `up` falls to zero then recovers; exclude |
| New code run, first observed scrape | Sep 5, 11:09:00 onward | Sep 5, 18:09:00 onward | Peak restarts at 63; separate startup/cache warmup |

PDT remains the assumed interpretation of the user's local times; the observed UTC outage aligns with the approximate times supplied. At 10:53:15 PDT the last observed pre-gap peak is 5,388,063 and confirmed counter is 5,388,064. At 11:09:00 they are 63 and 64, respectively: this is a node progress/counter reset, and historical metrics remain available. At the requested previous-run start, 23:15, peak is already 31,327, so that timestamp is not an exact process startup. Boundary queries use 15-second evaluation steps; transitions are scrape observations, not exact shutdown/start times. Main comparison windows begin well after startup and do not bridge the gap.

### Measured comparison

Historical range: Sep 5 06:15–20:10 UTC, evaluated every 60 seconds. Counter work rates below use endpoint differences divided by window duration; gauges use arithmetic means/maxima of one-minute evaluation samples. Counter endpoints have no observed reset inside the selected windows. Dashboard five-minute `rate` queries were retrieved separately using native range selectors. Histogram p95 is an estimate from bucketed observations, not an exact latency percentile.

| Measurement | Previous, Sep 4 23:23–Sep 5 01:33 PDT | New, Sep 5 11:24–13:00 PDT |
|---|---:|---:|
| Observed height endpoints | 218,719 → 1,601,087 | 204,543 → 1,600,895 |
| Elapsed time / confirmed blocks | 130 min / 1,382,368 | 96 min / 1,396,352 |
| Confirmed blocks/min, interval average | 10,633.6 | 14,545.3 |
| Five-minute blocks/min, sampled median | 9,951.5 | 15,229.8 |
| Instrumented worker CPU, core equivalents | 26.10 | 35.95 |
| Instrumented worker CPU seconds/block | 0.14730 | 0.14829 |
| VDF / body / signature CPU, core equivalents | 19.32 / 5.21 / 1.55 | 26.51 / 7.25 / 2.14 |
| Mean VDF CPU ms/completed compute job | 23.82 | 23.92 |
| Mean body CPU ms/completed compute job | 84.48 | 86.84 |
| Confirmation / writer-hold elapsed seconds/second | 0.297 / 0.264 | 0.334 / 0.287 |
| Mean catch-up commit latency | 73.41 ms | 57.16 ms |
| Median of sampled five-minute commit p95 estimates | 98.78 ms | 96.72 ms |
| Input coin mutations, counter difference | 91,428,166 | 91,344,455 |
| Configured total worker budget | 62 | 62 |
| Configured SQLite writer cache budget | 256 MiB | 4 GiB |
| Process RSS mean / sampled maximum | 1.94 / 3.76 GiB | 10.97 / 15.78 GiB |
| Charged reorder-buffer bytes, mean / maximum | 0.106 / 0.518 GiB | 4.982 / 8.001 GiB |
| Reorder-buffer slots, mean | 12,435 | 276,682 |

These are **approximately matched height ranges**, with about 1% more confirmed blocks in the new window; they are not an exact same-block replay. Coin-mutation totals are close, while ordering and per-block distributions remain unverified. Observed throughput is 36.8% higher and worker CPU consumption per second is 37.7% higher; CPU seconds/block is 0.7% higher, not lower. This supports improved pipeline delivery/occupancy as a hypothesis rather than faster primitives. Larger cache/queue settings and the machine update prevent attributing the gain to code alone. The writer-cache gauge measures a budget, not allocated memory, and charged queue bytes exclude representation/allocator overhead.

### Current bottleneck and prioritization

- **VDF is the largest measured CPU consumer:** 73.8% of instrumented worker CPU in the new matched window, versus body 20.2% and signatures 6.0%. This favors V2 and function-level VDF profiling. The 62-worker setting corresponds in reviewed code to 61 validation workers plus one persistence worker. Total measured worker CPU averages about 58% of that total budget; this does not establish 58% host utilization because other threads and hardware limits are unmeasured. VDF queues average 181 sampled jobs and mean VDF submission-to-start delay is 132 ms; instantaneous bursts can be saturated even when interval CPU occupancy is lower.
- **Confirmation matters more toward the captured endpoint:** Sep 5 12:40–13:10 PDT, heights 1,374,847–1,675,007, averages 10,005 blocks/min. Confirmation occupies 0.595 elapsed seconds/second and writer hold 0.540; mean commit latency is 172 ms and the median five-minute p95 estimate is 551 ms. VDF remains 68.6% of worker CPU, but CPU-intensive work overlaps serialized confirmation. These scopes overlap and must not be added. Archive CPU is only 0.031 core equivalents in this window, so serialization improvements alone cannot explain most writer-hold time.
- **Memory is an immediate efficiency concern:** in that latest window the reorder buffer stays around 8 GiB of charged bytes, RSS averages 15.15 GiB and peaks at 15.78 GiB, with 16 outbound peers and readahead depth 64. This suggests testing a smaller byte budget before increasing parallelism or prefetch. A large queue does not guarantee the next needed block is present; check head availability before diagnosing all supply stalls. No host memory limit or swap telemetry was established, so these measurements do not prove memory pressure or a leak.
- **Cache metadata locks are low priority:** new matched-window verification hit rate is only 0.071%; discriminant hit rate is 76.08%. Verification and discriminant metadata-lock wait accumulate at 0.01986 and 0.000341 seconds/second, respectively, versus 35.95 CPU seconds/second. These are different kinds of occupancy, but their scale gives little support for prioritizing cache sharding. Discriminant misses are 265/s; measure prime-derivation CPU and challenge locality before deciding whether capacity or ordering would improve reuse. A miss can be a new challenge, not necessarily avoidable thrashing.
- **Later chain eras differ:** the previous run's 10:15–10:50 PDT window, heights 5,148,191–5,366,687, averages 6,243 blocks/min and precompute-wait occupancy 0.554 seconds/second. That is not a regression comparison with the new run at height 1.6 million; it motivates retaining C1 and representative later-era CLVM tests.

The new run exports checkpoint mode/progress telemetry absent from the previous run. Its latest window records 1,297 PASSIVE calls, no TRUNCATE calls, zero checkpoint errors, and sampled outstanding-frame maximum 94,201; sampled no-progress age remains zero. This does not establish a stuck reader. Missing old mode metrics cannot be compared as zeros. The deployed node revision, exact build flags, backend feature configuration, hardware changes, physical I/O, and configured queue budget still need deployment records; SQLite metrics and the observed gauges alone do not establish those details.

### Measurement reference for future comparisons

For each run, record deployment revision/dirty status, binary build flags, machine/CPU, worker budget, backend, coin-index/hint features, storage medium, start/end heights, transaction/spend workload, scrape interval, and peer state. The machine update is a confounder independent of the code update. Compare the same replayed height interval when possible; disjoint eras and a database reset do not constitute a controlled A/B test.

| Evidence | Existing panels/metrics | Interpretation |
|---|---|---|
| Confirmed throughput | Blocks per minute; `fullnode_blocks_confirmed_total` | Main end-to-end outcome; report interval total/time and distribution of five-minute rates |
| Supply versus consumption | Download/confirm rates, prefetch occupancy, reclaimed reservations | An empty pipeline plus idle workers can mean network starvation |
| CPU utilization and queueing | Panels 101–104: compute workers, active/queued jobs, CPU/elapsed occupancy, mean queue wait | Distinguish compute saturation, blocking, and unavailable parallel work |
| VDF reuse/coordination | Panels 105–106: hits, misses, in-progress hits, evictions, lock wait | Cache changes require evidence of missed reuse or meaningful metadata contention |
| Sync critical path | Window timings and panel 108 phase occupancy | Last-window gauges are not latency distributions; overlapping phase scopes cannot be summed |
| Storage limitation | Panels 110–118 and 151–155 | Writer hold/commit, preparation, WAL progress, checkpoint duration; SQLite page activity is not device IOPS |
| Coin workload | Panels 141–144 | Normalize coin work/mutations as well as blocks in transaction-heavy eras |
| Memory pressure | RSS and available allocator metrics | Runtime reuse can lower allocation churn while retaining more capacity |

Use one exact job/instance throughout. Useful queries, substituting real label values:

```promql
rate(fullnode_blocks_confirmed_total{job="JOB",instance="INSTANCE"}[5m]) * 60

sum(rate(fullnode_compute_cpu_seconds_total{job="JOB",instance="INSTANCE"}[5m]))
  / rate(fullnode_blocks_confirmed_total{job="JOB",instance="INSTANCE"}[5m])

rate(fullnode_compute_queue_seconds_total{job="JOB",instance="INSTANCE"}[5m])
  / rate(fullnode_compute_jobs_total{job="JOB",instance="INSTANCE"}[5m])

rate(fullnode_vdf_cache_hits_total{job="JOB",instance="INSTANCE"}[5m])
  / (rate(fullnode_vdf_cache_hits_total{job="JOB",instance="INSTANCE"}[5m])
     + rate(fullnode_vdf_cache_misses_total{job="JOB",instance="INSTANCE"}[5m]))

resets(fullnode_blocks_confirmed_total{job="JOB",instance="INSTANCE"}[5m])
```

Ignore undefined ratios when there is no work; do not replace absence with a fabricated zero. Compute CPU seconds/block covers instrumented worker threads, not necessarily all process CPU or child threads. In-progress cache hits are a subset of hits, not an additional denominator. Mean queue wait is not p95. Retain raw time-series exports alongside any comparison summary.

## Ranked implementation backlog

The live readout above refines priority: start with V2/C1/X1 for ordinary sync, keep V1 for weight-proof verification, and defer V3/S1 unless more detailed profiling changes their measured importance. The table retains stable candidate IDs. “Small” means a local helper/call-site change; “medium” requires multiple consumers or lifetime/state handling. Confidence describes source evidence of avoidable work, not certainty of a large speedup.

| ID | Candidate | Scope | Potential / confidence | Where it pays |
|---|---|---|---|---|
| C1 | Reuse CLVM runtime across spends in one block | Small–medium | High in many-spend blocks / high | Body CPU, allocation churn |
| V1 | Use fused serial VDF path inside weight-proof parallel batches | Small | High for saturated weight-proof verification / high | Bootstrap/weight-proof CPU and scheduling |
| P1 | Remove temporary bit slices in v1 proof verification | Small first slice; medium full scratch conversion | High local allocation reduction / high | PoSpace verification in sequential header work |
| P2 | Reuse plot-filter digest and test prefix bytes directly | Small | Modest end-to-end; clear local work reduction / high | Every eligible PoSpace check |
| V2 | Build even VDF window powers with squaring | Small | Moderate kernel opportunity / medium | Uncached VDF arithmetic |
| S1 | Serialize nested fields into a shared output buffer | Medium, staged | Potentially high across serialization-heavy paths / high | Archives, wire encoding, header hashes |
| C2 | Borrow puzzle/solution inputs instead of owning wrappers | Small | Modest to workload-dependent / high | Spend setup before arena import |
| P3 | Use existing batched AES `g` in v2 proof validation | Small–medium | High within eligible hash work / high | v2 proofs only; hardware dependent |
| V3 | Reduce VDF cache-hit key allocation and copies | Medium | Workload-dependent / high | High cache-hit/revalidation workloads |
| B1 | Evaluate optimized release build profile | Configuration experiment | Potentially broad / unmeasured | Cross-crate inlining and code layout |
| C3 | Reduce arena interning bookkeeping/duplicate storage | Medium | Potentially high memory benefit / medium | Unique large atoms and frequent rewinds |
| X1 | Tune shared CPU budget and instrumentation granularity | Small experiment; medium change | Conditional / medium | CPU saturation or many very short jobs |

### C1 — Reuse a runtime across spends, with fresh consensus accounting

Evidence: `core/src/consensus/block_generator.rs:1371` creates `ClvmRuntime::new(cost_left, clvm_flags)` **inside** `conditions_from_generator_output`'s spend loop. `core/src/clvm/arena.rs:227` reserves a 1 MiB byte heap for every new arena, plus atom/pair pools. `core/src/clvm/runtime.rs:113` already clears stacks and resets the arena while retaining their capacities.

Proposed change: construct one runtime for this block's spend loop, add a narrow way to supply each spend's current `cost_left`, and continue using `run_in_arena`. Consume conditions completely before the next reset. This eliminates repeated arena construction and allows the pool capacities to be reused. For N spends, the explicit initial byte-heap reservation changes from N requests to one; that is **not** a claim that N MiB of physical RAM is touched or simultaneously resident.

Do not reuse a stale maximum cost. Reset all logical/ghost counters and stacks for every spend, including after errors. Keep dialect flags correct. Start with block-local lifetime: a global or indefinite thread-local arena can retain an adversarial high-water allocation on every worker. Per-block reuse also needs peak-memory measurement because its largest capacity survives until that block finishes.

Validation: identical condition digests, total/per-spend cost and errors; multiple consecutive spends, failure followed by success, maximum-cost boundaries, deep/shared output, and ghost allocation limits. Existing `core/tests/clvm_representation_invariants.rs`, `node/tests/clvm_conditions_digest.rs`, `node/tests/clvm_adversarial_limits.rs`, and memory/leak gates are directly relevant. Benchmark many-small-spend and few-large-spend blocks, not only arithmetic loops.

### V1 — Stop nested VDF thread creation in weight-proof batches

Evidence: `weight-proof/src/lib.rs:1402` parallelizes sampled segments with Rayon. Its `check_vdf` at line 635 calls `validate_vdf_info`, whose parallel verifier reaches `vdf/src/proof.rs:254`: a scoped OS thread is spawned for one exponentiation in each segment. By contrast, `node/src/header.rs:213` already uses `verify_vdf_serial` in the full-node batch drain.

Proposed first change: route the already-parallel sampled-segment path to `validate_vdf_info_serial`, which uses `fast_pow_form_pair_with`. If the helper also serves latency-sensitive serial callers, pass an explicit scheduling choice or split the helper rather than switching all callers indiscriminately. The fused implementation shares squarings and avoids inner spawn/join. Source comments estimate roughly 411 versus 673 group operations for representative exponents; these are algorithmic counts, **not measured wall-time speedups**.

Second, separately assess moving sampled-segment tasks onto the established compute budget. Their direct `par_iter` uses the global Rayon pool unless entered from another pool; this call does not itself select the application's reserved validation pool. Preserve short-circuiting and the global VDF-count limit. The ordinary sync dashboard does not currently attribute all weight-proof work to `fullnode_compute_*`, so validate with process CPU/thread counts and weight-proof completion time as well.

Validation: serial/parallel verdict parity for recursive, malformed, normalized and non-identity inputs; VDF-count boundary; simultaneous body sync; worker counts 1, 2 and machine width. Benchmark cache-cold proofs. Keep the two-chain path available for single-proof latency where it actually wins.

### P1 — Avoid allocating bit readers just to compare/read integers

Evidence: `proof_of_space/src/verifier.rs:225` starts the v1 proof path with heap-backed `fx`, decoded x values, and metadata. `get_proof_f1_and_meta` in `proof_of_space/src/plots/fx_generator.rs:146` repeatedly constructs bit readers from ChaCha output. `get_quality_string` at `proof_of_space/src/verifier.rs:297` builds sliced/reordered proof buffers over six levels. `compare_proof_bits` at `proof_of_space/src/verifier.rs:408` creates two `range` objects per compared x value; `BitReader::range` at `proof_of_space/src/utils/bit_reader.rs:304` materializes a new reader.

Small first patch: compare the k-bit integers directly using the existing non-allocating `slice_to_int` for validated ranges, preserving the current reverse-x comparison order and equality behavior. Replace `fx` with a fixed 64-element array and reserve exactly 64 decoded x values. These are separately reviewable changes.

Next: decode into fixed scratch arrays and extract ChaCha result bits directly from bytes, with exact cross-block handling. Reuse metadata storage rather than dropping its nested buffers through `clear`. Consider ordering indices into x values instead of rebuilding the entire proof bitstream for quality selection, but classify that as a separate algorithmic change.

Validation: every supported k, cross-byte/word/ChaCha-block boundaries, equal groups, proof permutation ordering, quality index extremes, malformed lengths, and known quality strings. Keep the generic safe bit-reader path as a differential oracle initially. Measure valid and early-invalid proofs. A K32-only specialization needs a general fallback. The current compression benchmark measures plot I/O/decompression, not isolated consensus verification.

### P2 — Compute the plot-filter input once and inspect only its prefix

Evidence: `core/src/blockchain/proof_of_space.rs:505` expands all 256 digest bits into `[bool; 256]`, then examines only the prefix. `calculate_pos_challenge` at line 547 hashes `calculate_plot_filter_input`, and `proof_of_space/src/lib.rs:94` subsequently calls `passes_plot_filter`, computing that same input digest again when the prefix is nonzero.

Proposed change: compute the filter input once in verification, derive the challenge from it, and pass it to a byte-prefix helper. Check complete prefix bytes for zero plus a mask on the remaining high bits; avoid expanding all bits. Preserve existing public wrappers if needed. Keep rejection ordering and zero-prefix behavior.

This removes one SHA-256 computation for nonzero-prefix checks in this call path plus the explicit 256-bit expansion. Its share of full proof verification may be small. Test all valid prefix lengths and current helper behavior outside the consensus range before deciding whether to restrict it. Do not silently change negative-prefix behavior as part of optimization.

### V2 — Use the specialized square operation when building power tables

Evidence: `vdf/src/form.rs:640` computes powers 2 through 15 by 14 successive `wmultiply` calls. The same module already provides `wsquare`. The fused path builds two tables per invocation.

Proposed experiment: for even power e, compute `base^e` by squaring the previously computed `base^(e/2)`; for odd e, multiply the previous power by the base. Per table this substitutes **seven squarings for seven general multiplies**, retaining seven multiplies. This does not reduce the number of operations; the gain depends on the measured square/multiply cost ratio and representation behavior. First verify every table entry against the current implementation and canonical reduced results.

Measure uncached proof verification end to end before tuning window width. A fixed `[WForm; 15]` table could also remove two allocations in the fused path, but measure stack size and spills; this is a lower-priority, independent experiment. Do not turn zero-digit windows into unconditional identity multiplications merely to remove a branch.

### S1 — Add an append-to-buffer serialization path

Evidence: `serialize/src/lib.rs:63` requires `to_bytes -> Vec<u8>`. Generic vector, option and tuple implementations allocate children and extend their parent. `macros/src/lib.rs:53` generates the same pattern for each field. Sized-byte serialization also returns a new vector. `stores/src/sqlite/block.rs:269` compresses a fully serialized block during archive preparation.

Proposed staged change: add a compatible append method with a default fallback, specialize scalars/sized bytes/containers, then update the derive macro and selected hot handwritten serializers. Keep existing `to_bytes` as the public convenience wrapper. This removes per-field temporary buffers only where overrides propagate through the call graph; a fallback alone produces no improvement. A streaming hash/writer API can follow if measurements justify it.

Scope is medium despite centralized edits: handwritten version-sensitive serializers require separate review. Preserve exact ordering, optional tags, integer encoding, length prefixes, and error propagation for every protocol version. Do not blindly preallocate from hostile wire lengths. An append failure can leave partial output; define and test that contract and ensure callers discard failed output.

Validation: byte-for-byte differential encoding of representative full blocks, headers and protocol messages across versions, roundtrip and malformed-input suites. Measure allocations and archive CPU seconds/block. The serializer currently has no standalone bench entry in its manifest; add focused coverage when implementing. Batched SQLite SQL and compression outside the writer lock already exist and should not be proposed again as new wins.

### C2 — Borrow puzzle and solution nodes during spend setup

Evidence: `core/src/consensus/block_generator.rs:1343` converts puzzle reveal and solution through `Program::new_ref(...).to_owned()` before importing them into the arena. The generator output remains alive through the spend loop.

Proposed change: pass borrowed nodes into `run_in_arena` and hash the borrowed reveal when needed. Confirm lifetimes through condition parsing. Ownership conversion is **not always a deep copy**: `core/src/clvm/sexp.rs:411` and line 603 clone Arcs for owned buffers/pairs but copy borrowed atom storage or recursively own borrowed pairs. Benefits depend on representation: fewer reference-count operations in the cheap case, fewer allocations in the borrowed case. Keep this separate from C1 so each contribution is measurable.

### P3 — Connect v2 validation to the existing AES batch kernel

Evidence: `proof_of_space/src/pos2/validator.rs:34` computes two scalar `hashing.g` values for each table-1 pair. `pos2/aes_hash.rs:160` already exposes `g_x_batch` with an eight-lane x86 AES kernel; v2 verification's recursive scalar path does not use it.

Proposed change: add a `ProofHashing` batch wrapper that preserves the testnet x transformation and exact round count, and feed precomputed match information into table-1 validation. Start with each eight-x fragment to retain useful early rejection; compare with all 128 x values batched. Keep later pairing/filter and chain checks unchanged. This is instruction-level parallelism on a worker, not a new thread pool.

Validation: scalar/batch parity on mainnet and testnet, all allowed parameters, valid and invalid fragments, native AES and fallback. The current batch acceleration is x86-specific; the fallback iterates scalar calls. Do not predict the same improvement on ARM. Establish actual v2 proof frequency before prioritizing this against v1 sync work.

### V3 — Reduce cache-hit bookkeeping before considering more caching

Evidence: `vdf/src/validation.rs:66` constructs output+witness bytes; `vdf/src/proof.rs:29` constructs another length-delimited owned cache key. `vdf/src/memo.rs:42` removes/reinserts entries, maintains a `BTreeMap` recency index, and clones owned keys while holding the metadata mutex. The discriminant cache similarly allocates seed keys on lookup.

Proposed first experiment: support borrowed lookup with ownership only on misses, and share owned key storage between lookup/recency structures. Preserve full equality across challenge, input, proof, discriminant size, iteration count and recursion; adapter target/witness checks must still run before returning a cached success. Do not replace full keys with an unchecked short fingerprint.

Only consider sharding or cache-capacity changes if panels 105–106 show contention/eviction costs large enough to matter. Cold expensive proofs can dwarf key work. Existing `OnceLock` entries coalesce concurrent requests while resident, but an in-progress entry can be evicted and recomputed; measure that case before designing a separate in-flight map. Any change must retain a hard memory bound, including hostile unique/invalid requests.

Validation: concurrent identical requests, capacity pressure while builds are in progress, cache-hit rejection parity, and lock wait versus actual proof CPU. A cache-hot repeated fixture is not a valid benchmark of VDF arithmetic.

### B1 — Measure release build settings before source micro-optimization

Evidence: root `Cargo.toml` has no release-profile override; `.cargo/config.toml` enables frame pointers. The Dockerfile builds in release mode. `core/Cargo.toml` enables portable blst, and the full node enables `bounded-bls` to disable its internal threads.

Try ThinLTO and fewer codegen units as isolated build experiments using identical workloads and features. Confirm the actual deployed binary's settings; absence of manifest overrides does not rule out environment/CLI overrides. Keep frame pointers for useful profiling initially. Native-CPU targeting is a machine-specific experiment with portability implications, not an appropriate universal binary setting.

PGO is a later option for branch/layout and inlining decisions using representative CLVM, VDF, PoSpace and near-tip workloads. It requires a training/build workflow beyond a tiny patch. Rust's [profile documentation](https://doc.rust-lang.org/cargo/reference/profiles.html) and [PGO guide](https://doc.rust-lang.org/rustc/profile-guided-optimization.html) describe the supported mechanisms; no published compiler speedup is assumed to transfer to this node.

### C3 — Investigate the cost of atom interning and rewinds

Evidence: `core/src/clvm/arena.rs:311` stores each new interned atom both in the arena byte heap and in a boxed map key. Atoms of 32 bytes or more are eligible. `restore` at line 263 clears the entire intern map, including entries referring to surviving storage. Repeated rewinds can discard useful lookup state and repeatedly free boxed keys.

Measure intern hits/misses, copied key bytes, map size at restore, and time spent clearing before changing representation. Candidate designs include an insertion log for removing only rewound entries, or a hash-to-span index with full byte comparison. These are more invasive than raising a threshold. Preserve ghost allocation accounting, pointer validity, and collision resistance under hostile atoms. Existing pair non-interning is deliberate; do not introduce pair hash-consing without new evidence.

### X1 — Tune capacity and task overhead only against measured occupancy

Evidence: `core/src/compute.rs:61` reserves one worker for archive/coin preparation when the budget exceeds one. The default budget subtracts two from available parallelism. Each `compute::map` item also performs shared atomic updates and thread-CPU clock reads (`compute.rs:176`). Body precomputation already runs across blocks and overlaps adjacent windows (`node/src/sync/mod.rs:2124`, `full-node/src/node/sync/processing.rs:136`).

On small machines, compare validation occupancy to its actual budget (total minus the persistence reservation), not to total workers alone. Experiment with configured budgets before redesigning pools. The reserved worker protects persistence progress; removing it may improve VDF occupancy but worsen end-to-end throughput.

For very short cached jobs, inspect atomic contention/clock overhead and benchmark modest chunks or per-worker counters. Avoid coarsening expensive variable-length VDF work and losing load balance. Keep metric definitions stable if job granularity changes. Rayon offers job-local state through [map_init](https://docs.rs/rayon/latest/rayon/iter/trait.ParallelIterator.html#method.map_init); this is not a guarantee of exactly one initialization per OS worker.

Live-data refinement: the new run's sampled reorder-buffer charge reaches approximately 8 GiB while its mean instrumented CPU use remains below the total budget. `node/src/sync/queue.rs:81` already accepts a byte budget; test smaller existing budget settings, for example 2 and 4 GiB against the observed approximately 8 GiB plateau, while holding the 4 GiB writer-cache setting and worker count fixed. Confirm the actual configured queue budget first. Require comparable throughput, head-of-queue availability and lower peak RSS. Do not reduce both queue and writer cache in the same experiment and then attribute the outcome to either. This is an efficiency experiment, not a demonstrated queue-size fix. The 0.595 confirmation occupancy in the latest window also calls for storage/coin-path profiling before increasing compute capacity.

## Branchless design and parallelism: specific decisions

| Technique | Apply here | Avoid here |
|---|---|---|
| Work elimination | C1/C2 allocation removal, P2 digest reuse, V1 fused exponentiation | Optimizing a branch around redundant work while leaving the work in place |
| Masks/word operations | P2 prefix bits; inspect emitted code for `runtime.rs:128` left/right handle selection | Arithmetic rewrites that accidentally change zero/overflow/shift behavior |
| Instruction-level parallelism | P3 existing multi-lane AES, potentially independent v1 ChaCha evaluations after profiling | Task scheduling per tiny hash or per CLVM opcode |
| Coarse parallel work | Existing block/proof batches; V1 avoid nested OS threads | Adding spend-level pools inside already-parallel body jobs without memory/cost-order analysis |
| Cache locality | Arena capacity reuse, shared serialization buffer, fixed small proof scratch | Large fixed stack buffers or caches without working-set measurements |
| PGO / code layout | Train on real mixed-era workloads, inspect hot dispatch assembly | Blanket `inline(always)` or assumed branch probabilities |

`ClvmRuntime::traverse_path` already selects two compact handles with an `if`; LLVM may lower this to a conditional move. Measure branch misses and inspect optimized assembly before writing a manual mask. Error exits and early invalid-proof rejection save substantial work and should remain cheap. Removing branches can increase instruction count, dependent work, and register pressure.

Production VDF arithmetic already uses carry-aware limb operations. `vdf/src/limbs.rs:552` explicitly retains the portable kernel because earlier A72 assembly did not establish a useful win; its comment's historical numbers were not revalidated here, and the referenced old report is absent from current docs. `discriminant.rs:114` uses packed small-prime screening and GMP probable-prime testing. The native Montgomery/BPSW helpers are not the selected production primality path. Optimizing those helpers or enabling dormant assembly is not automatically a production speedup. Do not alter prime candidate order, primality acceptance strength, canonical forms, or consensus cost/limits for performance.

## Coverage and lower-priority observations

This is a workspace-wide static pass with deeper call-path inspection of the requested areas, not a line-by-line proof that every function was audited.

| Area | Coverage / disposition |
|---|---|
| Core primitives, serialization and macros | Sized bytes, coin IDs, plot filters, serialization trait/derive, consensus primitive seam; prioritize S1/P2. `Coin::coin_id` already streams SHA-256 with stack amount encoding. `Hash for Coin` recomputes its ID, but no priority claim is made without a hot Coin-keyed container. |
| CLVM | Runtime, arena, ownership, tree-hash cache, generator/condition paths, operator allocation scan, benchmark and differential-test coverage. Prioritize C1/C2; parser-to-arena redesign is larger follow-up work. |
| ProofOfSpace | v1 consensus verifier/F1/bit reader, v2 validator/AES/core, plot reader/validation entry points. Keep farming/plotting I/O measurements separate from full-node proof checks. |
| VDF | Adapter, memoization, proof scheduling, window exponentiation, forms, discriminant selection, limb/Montgomery paths and existing parity tests. Prioritize V1/V2 according to workload. |
| Node/full-node/weight-proof | Header drains, native primitive dispatch, body precompute, cross-window pipeline, shared worker budgets, metrics, sampled-segment validation. PoSpace lacks a dedicated compute phase; whole header/weight-proof profiling is needed to attribute it. |
| Stores | SQLite archive/coin/checkpoint/telemetry surface plus backend layout. Existing local checkpoint work must settle before causal storage comparisons. PostgreSQL/mmap behavior requires separate backend measurements. |
| BLS/keys | Binding and aggregate-verification call paths/manifests inspected. Consensus has its own deduplicating aggregate path (`block_generator.rs:484`); optimizing the generic binding alone need not improve block validation. Preserve augmented messages, subgroup/infinity checks, and bounded threading. |
| P2P/clients/servers | Transport and peer bookkeeping surface screened. Arc clones/async locks alone do not establish a hotspot; shared serialization is the cross-cutting candidate. Check supply-side metrics before changing networking. |
| CLI/tools/simulator/puzzles/parser macros/logging/GUI | Workspace/build surface screened; no demonstrated dominant production compute path requiring a small change was established. GUI and test tooling are not ranked as sync CPU wins. |

Plot validation also defaults `thread_count` to zero but clamps it to one in `proof_of_space/src/verifier.rs:56`. If zero is intended to mean automatic parallelism, make that behavior explicit in a separate plotting-tool change and measure disk saturation. It is not a full-node sync optimization. Do not conflate plot-reader/decompressor parallelism with consensus proof verification.

## Validation and rollout plan for later work

1. **Complete deployment provenance:** the Grafana baseline and observed reset/gap are now captured above. Obtain the actual deployed revisions/build flags, hardware/update details and exact memory-budget settings. Record source and deployed binary separately. Keep the update gap/reset out of future averages.
2. **Profile one representative replay per bottleneck:** CPU samples, allocations/peak RSS, cycles, instructions, branch misses, context switches, and cache misses where supported. Existing optional full-node profiling endpoints and frame pointers can help; profiling endpoints were not invoked during this review.
3. **Implement one candidate per reviewable patch:** use V2/C1/X1 first for the observed ordinary-sync workload; V1 remains specific to weight-proof verification, and P1/P2 need header profiling for attribution. Measure valid and invalid inputs and small-machine behavior.
4. **Run consensus gates before speed comparisons:** preserve byte output, cost, condition digest, acceptance/rejection and bounded resource behavior. Use existing golden/differential suites; add regression cases specific to the changed mechanism rather than implementation-mirroring tests.
5. **Benchmark release builds repeatedly:** identical machine, flags and corpus; controlled thread counts and fresh processes for cold-cache VDF cases. Existing `cargo bench -p dg_xch_core --bench clvm` covers two block-generator fixtures; extend with many-spend fixtures during implementation. The PoSpace compression bench contains an absolute plot-file path and is not a portable consensus benchmark. VDF currently has correctness tests but no declared Criterion target; add a focused cold/warm verification harness before arithmetic tuning.
6. **Accept only observable improvements:** predeclare a useful threshold (for example ≥5% end-to-end improvement beyond run-to-run noise, or a substantial allocation reduction without throughput regression). This is an acceptance target, not a forecast. Report median/spread plus worst-case latency/RSS and CPU seconds per matched workload.
7. **Recheck live metrics after deployment:** annotate the exact deployment boundary, discard startup windows, compare like heights and features, and preserve a rollback point. Do not combine changes and attribute the result to branchlessness alone.

For prioritization, use Amdahl's law: if an operation occupies fraction f of the measured critical path and becomes s times faster, the ideal serial speedup is `1 / ((1 - f) + f / s)`. An illustrative 2× local win at f=0.10 is only about 1.053× overall. Overlapped CPU/storage pipelines need critical-path measurements rather than adding phase occupancy. The observed VDF share is a worker-CPU share, not f for the end-to-end critical path. The live baseline therefore establishes priorities but still does not assign a defensible node-wide speedup percentage to an unimplemented optimization.
