# Performance follow-up — September 5, 2026, 20:44 PDT

## Executive assessment

The node is progressing and its recent SQLite/checkpoint metrics look healthy. **The latest optimization bundle has not demonstrated an end-to-end improvement against the immediately preceding run.** Across approximately matched heights 0.2–2.2 million, throughput is **4.1% lower**, instrumented worker CPU/block is **2.5% higher**, and body CPU/job is **18.8% higher**. VDF CPU/job is 1.8% lower and archive CPU/job is 28.4% lower, but the archive saving is tiny in absolute terms.

The current approximately 6,800 blocks/min is not, by itself, evidence that the node degraded over time: the overnight run was also around 6,450 blocks/min at comparable heights. VDF cost/job rises markedly in this later era in both runs. The most actionable new concern is the **body/CLVM CPU increase at matched heights**, followed by validation pipeline waits and the large resident queue. Do not increase threads or the writer cache based on the current graphs alone.

This review used read-only service/host queries and source inspection. It did not change application code, run benchmarks, attach a profiler, inspect database contents, restart services or deploy anything. Only review artifacts were written.

## Evidence and scope

- Live source: [Grafana Sync Overview](https://grafana.galactechs.com/d/dgxch-sync-overview), version **8**, datasource `P1809F7CD0C75ACF3`, job `local_full_node`, instance `192.168.0.109:8444`.
- [Raw queries, dashboard definition, summaries and host observations](performance-followup-2026-09-05-2044-PDT.json.gz). This preserves daytime history through **20:44 PDT**, overnight history from Sep 4 **23:15** through Sep 5 **10:53**, instant histogram buckets, one-minute-sampled five-minute rates/p95, and a 15-second restart-boundary query. No credentials are included.
- Archive SHA-256: `72057eeea8aed250e04b3f77e53d3cb6bb7638a374445648d1b83ac2e1e3d50e`.
- Historical context: [original review](performance-optimization-review.md), [14:11 pre-restart snapshot](performance-prerestart-2026-09-05-1411-PDT.md), and [implementation guide](performance-implementation.md).
- The local workstation is **not the monitored node**. See the host section; deployed revision, build flags, node hardware, physical I/O and host pressure remain unverified.

### Restart and availability

The finer boundary query shows the last pre-restart scrape at **14:14:00**, peak **2,255,231**. `up=0` is observed from **14:14:15 through 14:15:00**. At **14:15:15**, `up=1` and peak/confirmed counters are zero. Progress is visible again at **14:16:45**, peak **1,247**. These are scrape observations, not exact process lifecycle timestamps.

All 389 one-minute `up` evaluations from **14:16–20:44** are 1. There is no further observed peak/confirmed-counter reset in that period. This cannot rule out outages between scrapes. The workstation's separate reboot at 20:38 did not interrupt those monitored-node samples.

## Matched-height comparison: immediately before versus after reset

Counter rates use endpoint differences over reset-free intervals. Matching chooses nearest one-minute peak-height samples: the ranges are close, not byte-identical replay intervals. Coin mutations/block provide another workload check; they do not fully characterize CLVM complexity, reference use, VDF witness composition or transactions. Five-minute rate plots around startup are excluded from the matched comparison.

| Range | Before: time / heights | After: time / heights | Before blocks/min | After blocks/min | Change |
|---|---|---|---:|---:|---:|
| About 0.2–1.6M | 11:24–13:00 / 204,543 → 1,600,895 | 14:25–16:07 / 201,951 → 1,601,119 | 14,545.3 | 13,717.3 | −5.7% |
| About 1.6–2.2M | 13:00–14:10 / 1,600,895 → 2,203,135 | 16:07–17:18 / 1,601,119 → 2,199,903 | 8,603.4 | 8,433.6 | −2.0% |
| Combined | 11:24–14:10 / 204,543 → 2,203,135 | 14:25–17:18 / 201,951 → 2,199,903 | **12,039.7** | **11,548.9** | **−4.1%** |

| Combined-range measurement | Before | After |
|---|---:|---:|
| Duration | 166 min | 173 min |
| Worker CPU seconds/block | 0.14801 | **0.15172** |
| Input coin mutations/block | 124.50 | 124.48 |
| VDF CPU ms/completed job | 23.874 | 23.440 |
| Body CPU ms/completed job | 89.073 | **105.832** |
| Signature CPU ms/completed job | 2.115 | 2.111 |
| Archive CPU ms/completed job | 0.0795 | 0.0570 |
| Confirmation elapsed seconds/second | 0.485 | 0.453 |
| Writer-hold elapsed seconds/second | 0.435 | 0.414 |
| Body-precompute join wait seconds/second | 0.186 | **0.249** |
| Mean catch-up commit latency | 110.29 ms | 110.07 ms |
| Readahead successful-consumption ratio | 99.71% | **93.70%** |
| Mean RSS | 12.702 GiB | **14.264 GiB** |
| Mean charged queue residency | 6.247 GiB | **7.603 GiB** |
| Worker budget / writer-cache budget | 62 / 4 GiB | 62 / 4 GiB |

The body increase is also present normalized by blocks: **29.876 → 35.451 CPU ms/block**, versus VDF **109.230 → 107.424 ms/block** and archive **0.159 → 0.114 ms/block**. Thus the absolute archive reduction cannot offset the body increase. Reduced phase occupancy per second is not necessarily faster work: doing fewer blocks/sec also reduces occupancy. The essentially unchanged mean commit latency illustrates that distinction.

These observations warrant investigation, not a causal verdict against a particular patch. The actual deployed binary and profile are unverified, there is only one run of each configuration, queue occupancy differs, and machine/peer/cache conditions were not controlled. Correctness tests passing does not establish performance acceptance.

## Current bottleneck and workload progression

All following windows end at 20:44 PDT, at peak **4,592,223**. Advertised peak is **9,252,332**, leaving **4,660,109** blocks of lag; the node is still in catch-up mode.

| Trailing interval | Blocks/min | Worker CPU sec/block | Confirmation sec/sec | Writer hold sec/sec | Precompute wait sec/sec | Mean RSS GiB |
|---|---:|---:|---:|---:|---:|---:|
| 5 min | 6,400.0 | 0.22931 | 0.093 | 0.078 | 0.482 | 15.157 |
| 15 min | 6,417.1 | 0.23129 | 0.113 | 0.097 | 0.489 | 15.317 |
| 30 min | **6,779.7** | 0.21644 | **0.124** | **0.106** | **0.459** | 15.401 |
| 60 min | 9,585.1 | 0.17842 | 0.176 | 0.153 | 0.485 | 15.514 |
| 120 min | 10,763.7 | 0.16984 | 0.219 | 0.193 | 0.493 | 15.205 |

In the latest 30 minutes, instrumented workers consume **24.457 CPU seconds/second**: VDF **18.828 (77.0%)**, body **4.681 (19.1%)**, signatures **0.931 (3.8%)**, with archive and coin preparation negligible. This is not host CPU utilization. The 62 configured workers correspond in this source to 61 validation workers plus one archive/coin-preparation worker; host core count, throttling and other consumers are not established.

VDF averages **36.089 CPU ms/job**, **37.883 wall ms/job**, and **207.916 ms submission-to-start wait/job** in that window. Body averages **118.184 CPU ms/job** and **3.981 ms queue wait/job**. Precompute join wait accumulates at **0.459 sec/sec**, while drain join wait is **0.342 sec/sec**. These point toward the compute/pipeline path rather than a dominant SQLite writer stall. They do not prove global CPU saturation, mutex contention, or a specific function hotspot.

Source interpretation matters: `full-node/src/node/sync/processing.rs:139` waits on the next-window body-precompute task, and `full-node/src/node/sync/processing.rs:361` measures waiting for the staged VDF/signature drain. Their scopes can overlap other work. `node/src/sync/mod.rs:2136` measures body jobs that include generator execution and aggregate-signature verification, not CLVM alone. One-minute active/queued gauges can alias bursty batches; completed-job time counters are better interval evidence.

### The later-era slowdown is not unique to this run

| Approximate heights | Overnight run | Current run | Important caveat |
|---|---|---|---|
| 2.34–4.22M | 03:26–07:16, 8,155.0 blocks/min | 17:30–20:00, 12,511.6 blocks/min | +53.4% throughput, but overnight writer cache was 256 MiB rather than 4 GiB; different code/machine-update context |
| 4.39–4.59M | 07:45–08:16, 6,449.5 blocks/min | 20:14–20:44, 6,779.7 blocks/min | +5.1% throughput; similarly heavier VDF work in both runs |

At the latter range, VDF CPU/job is **35.746 ms overnight versus 36.089 ms now**, whereas body CPU/job is **85.887 versus 118.184 ms**. Total CPU/block is **0.20301 versus 0.21644** and coin mutations/block are **51.03 versus 50.62**. This supports workload-era effects for the higher VDF cost, alongside a persistent body-performance concern. It does not establish why the VDF proofs in that era cost more: witness/iteration distributions are not exported.

## Storage, memory, caches and supply

**SQLite currently looks healthy.** Latest 30-minute mean catch-up commit latency is **34.59 ms**; the latest five-minute histogram p95 estimate is **47.33 ms**. Its sampled values over the half hour have median **91.875 ms** and maximum **121.43 ms**; the median of rolling p95 estimates is not a pooled half-hour event p95. There are 848 PASSIVE checkpoint calls in the half hour, no TRUNCATE calls, zero checkpoint errors/busy events, and two incomplete passes. Sampled outstanding frames and no-progress age are zero throughout that half hour. Across the post-reset history there are three busy events, no errors and no sampled nonzero no-progress age. A stable **1.646 GiB WAL file** is not evidence of a stuck checkpoint: file size and outstanding uncheckpointed frames are different quantities. No page-cache spills were recorded in the selected matched/latest windows.

**Memory remains expensive, but this is not evidence of a leak.** At capture: RSS **15.156 GiB**, charged queue **8.002 GiB**, queue length **155,328**, jemalloc allocated **9.723 GiB**, active **10.026 GiB**, resident **10.744 GiB**. Jemalloc retained virtual memory is **29.159 GiB**, which must not be added to RSS as physical memory. The post-reset sampled RSS maximum is approximately **16.53 GiB**, and the latest value is below it. Queue occupancy and allocator/cache retention confound time trends. Node-host available RAM, swap and cgroup pressure are missing, so neither memory pressure nor its absence can be inferred from workstation RAM.

**Cache sharding is still a low-priority hypothesis.** Latest-half-hour verification cache hit ratio is **0.063%**, discriminant **76.25%**. Their metadata-lock waits accumulate at **0.00811** and **0.000134 sec/sec**, respectively. This is small relative to the compute work, and does not establish profitable sharding or a larger verification cache. Misses and evictions do not prove avoidable reuse loss.

**A full queue does not rule out head-of-line supply waits.** Sixteen peers and 64 inflight readahead windows are maintained. The latest 30 minutes consume readahead successfully 93.53% of the time; `node/src/sync/prefetch.rs:306` shows a miss can mean a bounds mismatch/replan, failure, empty response or fallback, rather than a conventional byte-cache miss. Downloaded blocks average **1,614.9/min**, below **6,779.7/min confirmed**, while charged queue bytes stay full. That combination can reflect consuming an accumulated queue and changes in block size/queue contents; it is not proof that bandwidth limits the current validation step. Add/inspect contiguous-ready-head and fallback-reason evidence before changing prefetch depth or peer count.

## Code-level follow-up priorities

1. **Isolate the CLVM checkpoint change first.** `core/src/clvm/arena.rs:262` now calls `atom_intern.retain` on every restoration. `core/src/clvm/runtime.rs:297` invokes restoration for self-contained opcode results; imported long atoms can survive across many such operations. Rust documents `HashMap::retain` as scanning capacity, including empty buckets. Repeatedly scanning a large surviving map can cost more than the previous clear-and-repopulate strategy, despite saving some allocations. This is a concrete regression hypothesis, not a measured attribution. [Rust HashMap documentation](https://doc.rust-lang.org/std/collections/struct.HashMap.html#method.retain).
2. **Use a one-change A/B, not another optimization bundle.** On a separate test build, compare the current implementation with only the checkpoint-intern preservation change disabled; keep runtime reuse and borrowed puzzle/solution trees fixed. Then isolate runtime reuse if necessary: `core/src/clvm/runtime.rs:117` clears the retained hash map between spends, so retained high-water capacity can trade allocation savings for repeated clearing/scan work. Profile restore/intern/import/conditions parsing and aggregate-signature work separately. Preserve consensus accounting and existing differential/leak tests. No rollback was performed by this review.
3. **Benchmark representative generator shapes.** Existing `core/benches/clvm.rs:70` includes block 834752 and the 532-spend reference-generator block 4671894. Include many-spend and low-reuse/large-atom cases as well as scalar arithmetic. Record cost/sec, CPU, allocations, peak capacity and variance under an identical release profile. This review deliberately did not run a workload-generating benchmark against a live performance observation.
4. **Then examine compute overlap, not blindly more threads.** The source overlaps the next body window with staged validation, and already sorts VDF jobs by witness type (`node/src/header.rs:198`). Inspect body stragglers, generator-reference reads, validation batch widths and job-time distributions. Preserve outer parallelism/shared budgets; do not add nested pools or assume 62 threads equal 62 available physical cores. A bounded lookahead or scheduling change requires demonstrated readiness/straggler evidence.
5. **Retain small gains provisionally, with realistic expectations.** VDF/job and archive/job move in the desired direction in the immediately adjacent matched run, but neither isolates a patch. Archive savings are only about **0.045 CPU ms/block** there. There is no separate live PoSpace phase or weight-proof timing in this capture, so P1/P2/P3 and weight-proof changes cannot be independently graded from these graphs. Do not claim blanket branchless or batch-processing success without isolated measurements.
6. **Test memory-budget reduction separately after CPU attribution.** An 8 GiB charged queue still dominates memory. Compare 8/4/2 GiB using the existing prefetch-memory option while fixing writer cache, workers and matched workload; record next-required-window availability and throughput. Do not shrink a live queue or change defaults as part of diagnosis.

## Dashboard and measurement gaps

The deployed dashboard is still version 8. The working-tree checkpoint scheduling row and panels **150–155** are absent remotely, although the node exports their backing metrics. Existing dashboard CPU counters, mean queue waits and rolling p95 are useful, but are not host utilization, tail queue latency or exact event percentiles.

Recommended additions, not deployed here:

- Deployment annotations and a build-info/start-time metric identifying revision, dirty state, profile, features and effective worker/queue/cache budgets.
- CPU/job and CPU/block by phase, with completed-work counts and generator cost/spend/reference workload; keep invalid or zero-work ratios undefined rather than displaying reassuring zeros.
- Body/precompute and VDF job latency distributions, ready-head/contiguous queue depth, prefetch fallback reasons and checkpoint progress panels.
- Node-host CPU frequency/thermal throttling, NUMA/cgroup limits, process/thread CPU, disk latency/queue depth, available RAM/swap and memory/I/O pressure.

The queried Prometheus datasource has **no matching node-exporter CPU/memory/uname or process-start series for 192.168.0.109** under the queried names/instance filter. Metrics from the Kubernetes nodes or this workstation must not be substituted for that host.

## Local-machine findings and remaining visibility limit

Read-only host inspection establishes that this workspace is on **luna-pc**, addresses **192.168.0.179 / 192.168.1.139**, an AMD Ryzen 9 5950X with **16 physical cores / 32 logical CPUs**, approximately **62 GiB RAM**, **53 GiB available** and **zero swap in use** at observation. It rebooted at **20:38:33 PDT**. No `dg` process or listener on 8444/8555/9100 was found. Short `vmstat` interval samples were 94–97% idle, without swap traffic; these describe the workstation only. Its root filesystem is 40% used and `/home` is 84% used.

The local `target/release/dg` predates these changes (Sep 3); no local `target/release-perf/dg` was present. Workspace HEAD remains `5dd5475` with uncommitted performance and pre-existing SQLite/telemetry work. Neither proves the deployed node's revision or build flags.

SSH host/user authorization was requested because the monitored machine is different; no SSH access was attempted during this review. To complete host-level attribution, obtain read-only access to the actual node and inspect its process start/command line and binary identity, service/container limits, NUMA/CPU topology, short process/thread and disk samples, pressure/swap/frequency data, and startup/error logs. Avoid dumping environment secrets, scanning the live database, issuing checkpoint pragmas, or running storage stress tests. Until then, claims about disk saturation, thermal throttling, swapping or exact deployed patch effects remain unproven.

## Decision

Keep the current measurements as the post-reset baseline, but **do not mark the optimization bundle performance-accepted yet**. Prioritize a controlled CLVM checkpoint/body A/B and deployed-build verification. The current storage path does not justify an emergency tuning change, and the later-era throughput drop alone does not justify a rollback. Host inspection and isolated measurements should decide which changes to retain.
