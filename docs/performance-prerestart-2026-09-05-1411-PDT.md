# Pre-restart performance snapshot — September 5, 2026, 14:11 PDT

Captured from the live [Grafana Sync Overview](https://grafana.galactechs.com/d/dgxch-sync-overview) at **14:11:43 PDT (21:11:43 UTC)**, evaluating metrics through **14:11:00 PDT**. This preserves the currently running node before the planned restart with the workspace optimizations. The deployed binary revision remains unverified; these measurements must not be attributed to the newly implemented changes.

## Saved evidence

- [Complete raw snapshot and two-hour history](performance-prerestart-2026-09-05-1411-PDT.json.gz): gzip-compressed JSON containing query parameters, instant metrics (including histogram buckets), non-bucket metric history, five-minute rates and commit-p95 history.
- History: **12:11–14:11 PDT**, one-minute evaluation spacing. Dashboard version **8**, datasource UID `P1809F7CD0C75ACF3`, job `local_full_node`, instance `192.168.0.109:8444`.
- All seven Prometheus queries succeeded without warnings. All 121 sampled `up` values were 1; no sampled `_total` counter decreases were detected. This does not establish uninterrupted availability between samples.
- Read-only viewer access; no credentials are included. No service restart, deployment or configuration change was performed.
- Archive SHA-256: `d89e529f373d2789163e67d4b26600e6b594f97ee829f8fe929399f911c008b2`.

## Current state

| Metric | Value at 14:11 PDT |
|---|---:|
| Confirmed peak height | 2,217,087 |
| Advertised peak height | 9,251,117 |
| Tip lag | 7,034,030 blocks |
| Compute workers | 62 |
| Process RSS | 14.786 GiB |
| Charged queue residency | 8.000 GiB |
| Queue length | 185,824 |
| SQLite writer-cache budget | 4.000 GiB |
| SQLite WAL size | 1.688 GiB |
| Checkpoint errors, cumulative | 0 |

Queue accounting can exceed its nominal memory budget slightly; it is not the same measurement as process RSS. The writer-cache gauge is a configured budget rather than actual allocation.

## Throughput and resource windows

All windows end at 14:11 PDT. Throughput and CPU/occupancy figures below use cumulative counter endpoint differences divided by the window duration, not an average of dashboard gauges. CPU covers instrumented compute workers, not all process or machine CPU. Phase occupancy is elapsed seconds accumulated per wall second, not CPU utilization; overlapping phases must not be added as independent time.

| Window | Height endpoints | Blocks/min | Worker CPU sec/block | Confirmation sec/sec | Writer-hold sec/sec | Mean RSS GiB |
|---|---|---:|---:|---:|---:|---:|
| Last 5 min | 2,139,519 → 2,217,087 | **15,513.6** | 0.14395 | **0.268** | **0.223** | 14.870 |
| Last 15 min | 1,999,871 → 2,217,087 | 14,481.1 | 0.13958 | 0.378 | 0.331 | 14.957 |
| Last 30 min | 1,892,095 → 2,217,087 | 10,833.1 | 0.14288 | 0.580 | 0.529 | 15.057 |
| Last 60 min | 1,684,351 → 2,217,087 | 8,878.9 | 0.14663 | 0.672 | 0.618 | 15.100 |
| Last 120 min | 995,071 → 2,217,087 | 10,183.5 | 0.15421 | 0.575 | 0.523 | 14.887 |

The latest five minutes accumulated **37.219 worker CPU seconds/second**: VDF 28.232, body 6.790, signatures 2.153, archive 0.039 and coin preparation 0.004. VDF remains the largest instrumented CPU consumer.

The latest five-minute catch-up commit p95 estimate is **60.8 ms**, calculated with `histogram_quantile(0.95, sum by (le)(rate(fullnode_store_commit_seconds_bucket{job="local_full_node",instance="192.168.0.109:8444",phase="catch_up"}[5m])))`. The median of the one-minute-sampled five-minute p95 estimates over the last 30 minutes was 494.5 ms. That median is not a pooled 30-minute event p95.

There were **no checkpoint errors in the two-hour window**, eight additional busy events over two hours, one over the last 30 minutes, and none over the last 15 minutes.

## Interpretation and next comparison

The recent improvement is visible: the latest five-minute throughput is substantially higher than the longer-window averages, while confirmation/writer-hold occupancy and the latest commit-p95 estimate are lower. RSS remains around 15 GiB and the charged queue remains around 8 GiB. This is evidence of a strong recent interval, not proof of a sustained new performance level or a code-specific speedup.

The earlier [baseline](performance-baseline-2026-09-05.json) captured 10,005.3 blocks/min over 12:40–13:10 PDT at heights 1,374,847–1,675,007. The new windows process different blocks; do not treat their difference as an isolated optimization gain. Changes in block workload, storage/cache state and background machine load remain possible confounders. Local implementation builds and tests also ran during this capture's historical interval; their impact on the monitored host is unverified.

After restarting, record the actual restart time, deployed revision, release profile and CLI settings. Preserve this pre-restart artifact and collect a separate post-restart snapshot after warm-up. For a defensible speedup estimate, compare matched height ranges with the same worker/cache/queue settings, or replay the same workload, rather than comparing only adjacent wall-clock windows.
