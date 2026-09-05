# SQLite confirmation performance experiments

## What changed

- Bulk VDF verification, header signatures, body precomputation and archive encoding share a persistent CPU pool instead of creating competing per-window worker groups. Consensus verification is not skipped. Diagnostic `drain-probe` builds should not be used for throughput comparisons.
- Verification and discriminant caches use bounded keyed lookup and share identical in-progress work while an entry remains resident. Expensive derivation runs outside the metadata lock. Capacities remain 1,000 proof results and 200 discriminants.
- Bulk archive serialization, hashing and Zstd level-3 compression happen before acquiring SQLite's writer. Encoded records include their status in the insert. Body inserts are capped at 16 rows / 16 MiB of compressed payload, except a single oversized body.
- Coin additions and spends use bounded multi-row SQL; hint writes are sorted, deduplicated and batched when hints are enabled. Optional coalescing combines changes within one confirmation transaction, including coins created and spent inside that transaction. Coin records remain available for rollback; they are not discarded as ephemeral.
- Validation windows have independent block and estimated resident-byte limits. Bulk confirmation transactions can be smaller than validation windows. Archive rows, coins and peak remain atomic within each part. Earlier parts may remain committed if a later part fails. Unprocessed rejected-tail archive rows need not be persisted and can be downloaded/staged again.
- Near-tip persistence and reorg handling retain their existing paths. WAL, synchronous mode, checkpoint policy, compression format and schema are unchanged. This is not a durability-reduction experiment.

## Controls

| CLI flag | Default | Meaning |
| --- | --- | --- |
| `--compute-workers N` | Available logical CPUs minus two, minimum one | Shared bulk CPU budget; fixed for the process lifetime. Does not cap Tokio, networking or every other process thread. |
| `--validation-window-blocks N` | Existing CPU-derived size: twice available logical CPUs, clamped to 32–256 | Maximum blocks staged/verified together during bulk sync; permitted 1–4096. |
| `--validation-window-mb N` | 128 | Estimated resident MiB per validation window, not serialized bytes and not a process RSS limit. One oversized block is admitted for progress. |
| `--confirm-transaction-blocks N` | Whole validation window | Maximum staged blocks per bulk confirmation part, permitted 1–4096. A larger value does not combine separate validation windows. Near-tip/reorg fallback can use different transaction boundaries. |
| `--sqlite-writer-cache-mb N` | 256 bulk / 64 near tip | Overrides only the bulk writer cache budget. SQLite allocates on demand; this is not a reservation. |
| `--coalesce-coin-writes` | Off | Combine coin/hint changes within each bulk extension transaction. Enable separately to isolate its effect. |

The pool default is approximately 62 workers if your 3970X exposes all 64 logical CPUs. One worker is now reserved for archive preparation, leaving 61 for validation; this does not increase the total budget. With a budget of one, all phases share that worker. Fullnode enables core's `bounded-bls` feature to run BLST work inside its calling worker rather than creating BLST's additional machine-sized pool. Other standalone core consumers can retain threaded BLST. Check the dashboard rather than assuming container affinity or CPU quotas expose every CPU. Physical cores, SMT threads and VDF throughput do not scale interchangeably.

## Follow-up: confirmation backlog

The longer supplied snapshot covers approximately heights 970,047–1,161,311 over 30 minutes: about 106 confirmed blocks/s, while the downloaded queue grows from 4,384 to 74,336 blocks. Archive preparation averages about 305 ms per call despite archive jobs consuming only about 0.04 CPU cores on average. These observations support targeting serialized work and scheduling, not increasing prefetch. They do not establish the speedup of this candidate.

This follow-up changes four paths:

- Wallet peak notifications return before resolving spends when there are no subscribers. Active subscribers still receive coin-state updates through the existing path.
- SQLite coin multi-get submits at most 256 names per statement, instead of one awaited statement per name. The requested-name relation is the outer side of a CROSS JOIN, keeping indexed primary-key lookups into the coin table. Explicit ordinals preserve input order and duplicates; missing coins remain omitted. Regression tests check results, statement counts and query plans. See SQLite's [join-order documentation](https://www.sqlite.org/optoverview.html#manual_control_of_query_plans_using_cross_join).
- Archive preparation has one reserved worker within the existing total compute budget, so next-window validation cannot occupy all its capacity. BLST's hidden pool is disabled for Fullnode builds without skipping BLS verification. Measure transaction-heavy single-block cases as well as multi-block windows: serializing BLST's internal aggregate work can trade single-block latency for better outer concurrency.
- SQLite connections use `temp_store=MEMORY` for eligible temporary tables and sorts. The bundled SQLite also passes this setting into its pager to keep statement sub-journals in memory. WAL and synchronous settings are unchanged; this is not `journal_mode=MEMORY` or a blanket ban on every temporary-file type. The observed `etilqs_*` files were not classified, so verify their disappearance or placement in the next run. See SQLite's [temporary-file documentation](https://www.sqlite.org/tempfiles.html).

The existing Grafana storage-operation charts now include `operation="coin_lookup"`: elapsed time, calls, SQL executions and requested names. Requested names include misses and duplicates, not just returned records. Look for lower `stage` and `post_confirm` occupancy, fewer lookup executions per confirmed block, and lower archive-preparation wall time/queue delay. Compare throughput over matching heights rather than targeting a higher CPU percentage.

### Next test command and temporary-file isolation

Keep every argument in your current command unchanged for the first implementation comparison. Rebuild locally using the release command below and import the updated dashboard. No settings or files on `192.168.0.109` are changed by these source changes.

The read-only live inspection found deleted SQLite `etilqs_*` temporary files under disk-backed `/var/tmp`, although the database is on tmpfs. Their exact type was not established. For a separately labelled RAM-only temporary-file experiment, prefix your existing launch command with:

```bash
SQLITE_TMPDIR=/mnt/chia-ram/chia_sync_test ./target/release/dg full-node \
  --listen 0.0.0.0:8444 \
  --introducer introducer.chia.net:8444 \
  --peer druid.garden:443 \
  --peer 98.103.207.164:8445 \
  --peer 98.103.207.164:8444 \
  --db sqlite:///mnt/chia-ram/chia_sync_test/chain.db \
  --network mainnet \
  --genesis-sync \
  --prefetch-memory-mb=8192 \
  --prefetch-max-inflight=64 \
  --target-outbound 16 \
  --target-peer-count 80
```

The directory must already exist and be writable by the node user. Set this in the launching shell before process startup; do not change SQLite's process-global temporary directory at runtime. SQLite checks `SQLITE_TMPDIR` before the usual Unix fallback directories. Verify any remaining `etilqs_*` file descriptors point into tmpfs, and watch total tmpfs usage and available RAM. Main database/WAL placement is still controlled by `--db`. In-memory temporary storage is not capped by the writer-cache setting: large sorts and index creation can increase process memory beyond the bounded confirmation queries. On RAID testing, record whether temporary files use RAM or RAID and keep that choice identical between candidates.

For clean attribution, compare old/new binaries with the same temporary-file environment first, then change only `SQLITE_TMPDIR`. Do not change workers, validation windows, transaction windows or coalescing at the same time. After the implementation comparison, use rounds B–G below to test windows and cache sizes one dimension at a time.

## Baselines and reproducibility

1. Preserve the existing release binary under a distinct filename **before rebuilding**. Record its revision, build flags and the current uncommitted diff separately; a commit hash alone does not identify a dirty-tree build.
2. Build the candidate with the same feature set and compiler settings: `cargo build --release -p dg_xch_cli --locked`. Do not benchmark a debug build or enable `drain-probe`.
3. Keep your peers, network, `--genesis-sync`, process affinity, CPU governor and monitoring interval unchanged. Import `docs/grafana/dg-xch-sync-overview.json` into Grafana. Use the same Prometheus job/instance filters.
4. Run baseline and candidate against **different fresh database directories**, one process at a time. Do not reuse a partly synced database for a genesis comparison. Keep database, WAL and SHM files together; never delete WAL files to reset a live test.
5. Compare elapsed time over identical confirmed-height intervals, not identical wall-clock slices starting at different heights. Early history, transaction-heavy history and sub-epoch transitions have different costs. Save height/timestamp checkpoints and Grafana exports for each run.
6. Screen settings over the same fixed height interval with at least 10–15 minutes of steady-state samples after warm-up where practical. Repeat finalists three times, alternating order, then perform full genesis-to-tip runs. Compare median time and spread, not the best run.
7. A safe stopped-node database snapshot or SQLite backup can accelerate later-height comparisons. Restore the same snapshot separately for every run; never copy only `chain.db` while its writer is active. Keep a final full-genesis validation as the acceptance test.

Keep your original 8192 MiB / 64-inflight command for the first old-vs-new comparison. Changing prefetch and implementation together would confound attribution. Preserve any existing metrics/listener options omitted from the example command.

## Experiment order on the RAM drive

Change one dimension at a time; this is a staged search, not a Cartesian product.

| Round | Hold constant | Sweep | Selection criterion |
| --- | --- | --- | --- |
| A: implementation | Original CLI, coalescing off | Old binary vs candidate defaults | Confirmed blocks/s and identical chain/state; establish whether shared scheduling helps. |
| B: CPU workers | Validation 128 blocks / 128 MiB; transaction 128; cache 256 MiB | 32, 48, 62 workers | Best sustained confirmation throughput with acceptable queue wait and responsive networking. Test 64 only if 62 still benefits from more workers. |
| C: validation window | Winning workers; transaction 64; cache 256 MiB | 64, 128, 256, 512 blocks | Throughput versus VDF/CPU queue wait, RSS, stage time and tail latency. Keep byte cap 128 MiB. |
| D: transaction window | Winning workers and validation size | 32, 64, 128, 256, bounded by validation size | Minimize writer hold and cache writes/spills per confirmed block without losing throughput to more commits. |
| E: byte window | Winning workers, validation and transaction block limits | 32, 64, 128, 256 MiB on transaction-heavy history | Avoid RSS spikes and excessive preparation/hold latency; early small-block history may never hit this cap. |
| F: writer cache | Winning window settings | 256, 1024, 4096 MiB | Fewer cache misses/spills and more confirmed blocks/s, not merely more RAM consumed. Try 8192 only if 4096 still misses/spills significantly. |
| G: coin coalescing | Winning settings | Flag absent vs `--coalesce-coin-writes` | Equal final coin state; fewer statements/page writes per block and better confirm throughput. |
| H: prefetch | Winning settings, inflight 64 | 8192, 2048, 1024 MiB | Smallest lookahead that keeps confirmation fed. Then test inflight 64, 32, 16 separately. |

Round C's fixed 64-block transaction isolates validation size from transaction size. In Round D, transactions cannot exceed the actual byte-limited validation window: inspect the blocks-per-part chart, not just the CLI value. Small transactions may improve latency but increase commits and small WAL writes; larger is not automatically better.

The 400 GB RAM drive and database memory share your 512 GB physical RAM if the drive is RAM-backed. Its capacity is not free spare memory. Track system available memory, swap, actual RAM-drive usage, process RSS and WAL growth together. Do not allocate the remaining nominal 112 GB to SQLite. Stop or shrink an experiment if memory pressure develops; do not wait for OOM or a full RAM drive.

## What to read in Grafana

- **Downloaded vs confirmed:** confirmed throughput is the primary objective. A smaller download/confirm gap with unchanged confirmation speed is a memory/pacing improvement, not faster validation.
- **CPU budget / occupancy / queue wait:** distinguish busy VDF workers from archive/body competition. CPU seconds/second approximates busy cores on Linux; elapsed job occupancy includes blocking. Archive has reserved capacity; validation phases still share workers without strict oldest-window priority.
- **VDF caches:** compare hit, miss, in-progress-hit and eviction rates. Lock-wait seconds measure metadata contention, not waiting on an in-progress derivation. Caches are bounded by entries; they are not unbounded history stores.
- **Storage operation duration:** compare mean writer wait, writer hold, archive preparation, archive write, coin and peak work. These are cumulative-timer means, not p95 histograms. Existing COMMIT p95 remains separately charted.
- **SQL rows/executions:** verify batching reduces dispatches and raises rows per execution. Rows count submissions to successful instrumented statements, not necessarily affected rows or finally committed rows. Peak SQL is timed but not included in these row/statement counts.
- **Cache writes/spills per block:** SQLite writer page-cache events, sampled after batch commits, are a write-amplification proxy. They include rolled-back work and do not equal OS write calls, RAID IOPS or physical bytes.
- **Prepared payload:** encoded record and compressed body bytes, including retries. This excludes indexes, WAL framing and checkpoint copies. Writer cache budget is configured capacity, not live cache allocation.
- **Sync phase occupancy:** use rates of cumulative timers for comparisons. Existing window-duration gauges are scrape samples. Concurrent/contained phases overlap and must not be summed into a total latency.

All new exported performance metric families are represented in the dashboard's CPU, transaction/SQL and cache/write-amplification sections. Confirmation parts are not a universal transaction counter: near-tip parts can contain multiple commits.

## Repeat finalists on the NVMe RAID-6 array

RAM-drive results isolate much of the CPU/SQL dispatch cost but cannot select the final disk transaction/cache settings. Repeat the baseline, best RAM-drive setting, and its neighboring transaction/cache sizes on the RAID using fresh directories and the same confirmed-height intervals.

Capture `iostat -x 1`, `pidstat -d -p <PID> 1`, and `vmstat 1` alongside Grafana if sysstat is installed. Observe the RAID device **and** member devices; do not sum logical-array and member bytes as though they were independent traffic. Record write IOPS, average request size, write bandwidth, await/queueing, CPU iowait, checkpoint activity and WAL high-water mark. Normalize write bytes and IOPS over the same confirmed block count. Use device statistics rather than RAM-drive process I/O counters to assess RAID behavior.

Keep the existing WAL/checkpoint/durability configuration fixed. Count a result as a storage win only if it improves sustained confirmation or lowers writes per confirmed block without unbounded WAL growth, reader starvation or worse recovery. A RAM-drive winner can lose on RAID when more frequent commits amplify small writes.

## Correctness and restart acceptance

For each finalist, verify the same peak hash at the same height and compare coin records, spent heights and hints with the baseline. Include transaction-heavy and sub-epoch-boundary intervals; empty early blocks alone do not exercise coin coalescing. Do not compare raw database-file hashes, since row/page layout can legitimately differ.

On disposable databases, test normal stop/restart and forced process termination during archive preparation, SQL writing and between confirmation parts. A transaction must leave archive/coins/peak consistent; restart must resume from the durable peak, revalidate missing work and reach the baseline state. Process-kill testing does not demonstrate power-loss durability. Run `PRAGMA quick_check` on a stopped test database and inspect chain continuity/peak/coin semantics separately; SQLite structural integrity alone cannot establish consensus correctness.

Also exercise reorg/failure-prefix regression tests and near-tip operation before promoting settings. Reject any performance result with changed validation outcomes, missing spends, incorrect rollback, persistent checkpoint backlog or unstable RSS. No throughput improvement is assumed until these workload comparisons have been run on your hardware.

## Your command: prefetch-only experiment

Yes: item 7's initial experiment is only an existing CLI argument change. Keep all other arguments exactly as supplied and replace:

```text
--prefetch-memory-mb=8192
```

with:

```text
--prefetch-memory-mb=1024
```

Leave `--prefetch-max-inflight=64` unchanged for that comparison. This lowers a resident-byte budget; it is not a strict blocks-ahead limit and outstanding requests may overshoot it. If confirmation becomes download-starved, try 2048 MiB before adjusting request concurrency. No new blocks-ahead policy has been introduced.
