use dg_xch_node::SyncMetrics;
use dg_xch_stores::telemetry::StoreTelemetry;
use std::fmt::Write;
use std::sync::atomic::{AtomicU64, Ordering};

fn family(
    output: &mut String,
    name: &str,
    help: &str,
    kind: &str,
    label: &str,
    values: &[(&str, &AtomicU64)],
    divisor: f64,
) {
    let _ = writeln!(output, "# HELP {name} {help}\n# TYPE {name} {kind}");
    for (value, counter) in values {
        let number = counter.load(Ordering::Relaxed) as f64 / divisor;
        let _ = writeln!(output, "{name}{{{label}=\"{value}\"}} {number}");
    }
}

pub(super) fn render(output: &mut String, store: Option<&StoreTelemetry>, sync: &SyncMetrics) {
    let workers = dg_xch_core::compute::worker_count();
    let _ = writeln!(
        output,
        "# HELP fullnode_compute_workers Shared CPU worker budget.\n# TYPE fullnode_compute_workers gauge\nfullnode_compute_workers {workers}"
    );
    let compute: Vec<_> = dg_xch_core::compute::Phase::ALL
        .into_iter()
        .map(|phase| (phase.name(), dg_xch_core::compute::counters(phase)))
        .collect();
    for (name, help, kind, divisor, values) in [
        (
            "fullnode_compute_jobs_total",
            "Completed CPU jobs including failures.",
            "counter",
            1.0,
            compute
                .iter()
                .map(|(name, metrics)| (*name, &metrics.completed))
                .collect::<Vec<_>>(),
        ),
        (
            "fullnode_compute_active_jobs",
            "Currently executing CPU jobs.",
            "gauge",
            1.0,
            compute
                .iter()
                .map(|(name, metrics)| (*name, &metrics.active))
                .collect(),
        ),
        (
            "fullnode_compute_queued_jobs",
            "Submitted CPU jobs not yet started.",
            "gauge",
            1.0,
            compute
                .iter()
                .map(|(name, metrics)| (*name, &metrics.pending))
                .collect(),
        ),
        (
            "fullnode_compute_job_seconds_total",
            "Sum of CPU job wall time; concurrent jobs overlap.",
            "counter",
            1e9,
            compute
                .iter()
                .map(|(name, metrics)| (*name, &metrics.elapsed_nanos))
                .collect(),
        ),
        (
            "fullnode_compute_cpu_seconds_total",
            "Thread CPU time inside jobs; Linux only, zero elsewhere.",
            "counter",
            1e9,
            compute
                .iter()
                .map(|(name, metrics)| (*name, &metrics.cpu_nanos))
                .collect(),
        ),
        (
            "fullnode_compute_queue_seconds_total",
            "Sum of job submission-to-start waits.",
            "counter",
            1e9,
            compute
                .iter()
                .map(|(name, metrics)| (*name, &metrics.wait_nanos))
                .collect(),
        ),
    ] {
        family(output, name, help, kind, "phase", &values, divisor);
    }
    let caches = dg_xch_node::vdf_cache_metrics();
    for (name, help, divisor, values) in [
        (
            "fullnode_vdf_cache_hits_total",
            "Requests finding an existing cache entry.",
            1.0,
            caches
                .iter()
                .map(|(name, metrics)| (*name, &metrics.hits))
                .collect::<Vec<_>>(),
        ),
        (
            "fullnode_vdf_cache_misses_total",
            "Requests creating a cache entry.",
            1.0,
            caches
                .iter()
                .map(|(name, metrics)| (*name, &metrics.misses))
                .collect(),
        ),
        (
            "fullnode_vdf_cache_shared_total",
            "Hits finding an unfinished shared computation.",
            1.0,
            caches
                .iter()
                .map(|(name, metrics)| (*name, &metrics.shared))
                .collect(),
        ),
        (
            "fullnode_vdf_cache_evictions_total",
            "Cache entries evicted at capacity.",
            1.0,
            caches
                .iter()
                .map(|(name, metrics)| (*name, &metrics.evictions))
                .collect(),
        ),
        (
            "fullnode_vdf_cache_lock_seconds_total",
            "Time acquiring cache metadata locks.",
            1e9,
            caches
                .iter()
                .map(|(name, metrics)| (*name, &metrics.lock_wait_nanos))
                .collect(),
        ),
    ] {
        family(output, name, help, "counter", "cache", &values, divisor);
    }
    family(
        output,
        "fullnode_sync_phase_seconds_total",
        "Cumulative phase wall time; phases may overlap.",
        "counter",
        "phase",
        &[
            ("stage", &sync.stage_total_micros),
            ("confirm", &sync.confirm_total_micros),
            ("post_confirm", &sync.post_confirm_total_micros),
            ("drain_wait", &sync.drain_wait_total_micros),
            ("precompute_wait", &sync.pre_wait_total_micros),
        ],
        1e6,
    );
    let _ = writeln!(
        output,
        "# HELP fullnode_confirm_parts_total Successfully persisted confirmation parts.\n# TYPE fullnode_confirm_parts_total counter\nfullnode_confirm_parts_total {}",
        sync.confirm_parts.load(Ordering::Relaxed)
    );
    for (name, help, counter) in [
        (
            "fullnode_confirm_coin_mutations_total",
            "Input additions, spends and hints of accepted reported blocks; not affected SQL rows.",
            &sync.confirm_coin_mutations,
        ),
        (
            "fullnode_confirm_coin_bytes_total",
            "Estimated input coin payload of accepted reported blocks; excludes archive, indexes, WAL and allocator overhead.",
            &sync.confirm_coin_bytes,
        ),
        (
            "fullnode_confirm_oversized_coin_blocks_total",
            "Accepted reported blocks individually exceeding configured coin budgets.",
            &sync.confirm_oversized_coin_blocks,
        ),
    ] {
        let _ = writeln!(
            output,
            "# HELP {name} {help}\n# TYPE {name} counter\n{name} {}",
            counter.load(Ordering::Relaxed)
        );
    }
    if let Some(store) = store {
        family(
            output,
            "fullnode_sqlite_checkpoint_requests_total",
            "Background PASSIVE checkpoint scheduling decisions.",
            "counter",
            "reason",
            &[
                ("write_budget", &store.checkpoint_write_budget),
                ("interval", &store.checkpoint_interval),
                ("backlog", &store.checkpoint_backlog),
            ],
            1.0,
        );
        family(
            output,
            "fullnode_sqlite_checkpoint_events_total",
            "Incomplete checkpoint results and escalation attempts deferred by an active writer.",
            "counter",
            "event",
            &[
                ("incomplete", &store.checkpoint_incomplete),
                ("writer_active", &store.checkpoint_escalation_deferred),
            ],
            1.0,
        );
        for (name, help, divisor, passive, truncate) in [
            (
                "fullnode_sqlite_checkpoint_mode_calls_total",
                "Completed checkpoint attempts by mode, including errors.",
                1.0,
                &store.checkpoint_passive.calls,
                &store.checkpoint_truncate.calls,
            ),
            (
                "fullnode_sqlite_checkpoint_mode_seconds_total",
                "Checkpoint elapsed time by mode, including errors.",
                1e9,
                &store.checkpoint_passive.nanos,
                &store.checkpoint_truncate.nanos,
            ),
        ] {
            family(
                output,
                name,
                help,
                "counter",
                "mode",
                &[("passive", passive), ("truncate", truncate)],
                divisor,
            );
        }
        for (name, help, value) in [
            (
                "fullnode_sqlite_wal_outstanding_frames",
                "Uncheckpointed frames observed in the last valid checkpoint result; sampled, not live WAL size.",
                &store.wal_outstanding_frames,
            ),
            (
                "fullnode_sqlite_checkpoint_no_progress_seconds",
                "Seconds without observed checkpoint progress while frames remain outstanding; zero after completion.",
                &store.checkpoint_no_progress_seconds,
            ),
        ] {
            let _ = writeln!(
                output,
                "# HELP {name} {help}\n# TYPE {name} gauge\n{name} {}",
                value.load(Ordering::Relaxed)
            );
        }
        family(
            output,
            "fullnode_coin_path_events_total",
            "Coin path work and cache events; preparation counts include hints only when hint persistence is enabled.",
            "counter",
            "event",
            &[
                ("view_reuse", &store.coin_view_reused),
                ("ancestors_walked", &store.coin_view_ancestors),
                ("coins_folded", &store.coin_view_coins),
                ("prefetch_hit", &store.coin_prefetch_hits),
                ("lookup_fallback", &store.coin_prefetch_fallbacks),
                ("prepared_input_mutations", &store.coin_prepare_input_rows),
                ("prepared_output_mutations", &store.coin_prepare_output_rows),
            ],
            1.0,
        );
        let operations = [
            ("coin_prepare", &store.coin_prepare),
            ("coin_view", &store.coin_view),
            ("coin_prefetch", &store.coin_prefetch),
            ("writer_wait", &store.writer_wait),
            ("writer_hold", &store.writer_hold),
            ("archive_prepare", &store.archive_prepare),
            ("archive_write", &store.archive_write),
            ("coin_additions", &store.coin_additions),
            ("coin_removals", &store.coin_removals),
            ("coin_lookup", &store.coin_lookup),
            ("hints", &store.hints),
            ("peak_update", &store.peak_update),
        ];
        for (name, help, divisor, values) in [
            (
                "fullnode_store_operation_seconds_total",
                "Operation wall time including failed attempts; scopes can overlap.",
                1e9,
                operations
                    .iter()
                    .map(|(name, metrics)| (*name, &metrics.nanos))
                    .collect::<Vec<_>>(),
            ),
            (
                "fullnode_store_operation_calls_total",
                "Completed operation scopes including failures.",
                1.0,
                operations
                    .iter()
                    .map(|(name, metrics)| (*name, &metrics.calls))
                    .collect(),
            ),
            (
                "fullnode_store_operation_rows_total",
                "Rows submitted by successful SQL statements.",
                1.0,
                operations
                    .iter()
                    .map(|(name, metrics)| (*name, &metrics.rows))
                    .collect(),
            ),
            (
                "fullnode_store_operation_statements_total",
                "Successful SQL executions by operation.",
                1.0,
                operations
                    .iter()
                    .map(|(name, metrics)| (*name, &metrics.statements))
                    .collect(),
            ),
        ] {
            family(output, name, help, "counter", "operation", &values, divisor);
        }
        family(
            output,
            "fullnode_sqlite_writer_cache_events_total",
            "SQLite writer cache events sampled and reset after batch commits; includes rolled-back work.",
            "counter",
            "event",
            &[
                ("hit", &store.cache_hits),
                ("miss", &store.cache_misses),
                ("write", &store.cache_writes),
                ("spill", &store.cache_spills),
            ],
            1.0,
        );
        let _ = writeln!(
            output,
            "# HELP fullnode_sqlite_writer_cache_bytes Configured writer cache budget.\n# TYPE fullnode_sqlite_writer_cache_bytes gauge\nfullnode_sqlite_writer_cache_bytes {}",
            store.writer_cache_kib.load(Ordering::Relaxed) * 1024
        );
        let _ = writeln!(
            output,
            "# HELP fullnode_archive_prepared_bytes_total Encoded record and compressed body payload bytes prepared, including retries.\n# TYPE fullnode_archive_prepared_bytes_total counter\nfullnode_archive_prepared_bytes_total {}",
            store.prepared_bytes.load(Ordering::Relaxed)
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn performance_metrics_include_units_and_operation_labels() {
        let store = StoreTelemetry::default();
        store
            .coin_additions
            .nanos
            .store(2_000_000_000, Ordering::Relaxed);
        store.coin_additions.statement(100);
        store.coin_lookup.statement(256);
        store.coin_prefetch_hits.store(42, Ordering::Relaxed);
        store.checkpoint_write_budget.store(7, Ordering::Relaxed);
        store.checkpoint_passive.calls.store(9, Ordering::Relaxed);
        store
            .checkpoint_passive
            .nanos
            .store(3_000_000_000, Ordering::Relaxed);
        store.wal_outstanding_frames.store(11, Ordering::Relaxed);
        store
            .checkpoint_no_progress_seconds
            .store(12, Ordering::Relaxed);
        let mut output = String::new();
        render(&mut output, Some(&store), &SyncMetrics::default());
        assert!(
            output.contains(
                "fullnode_store_operation_seconds_total{operation=\"coin_additions\"} 2\n"
            )
        );
        assert!(
            output.contains(
                "fullnode_store_operation_rows_total{operation=\"coin_additions\"} 100\n"
            )
        );
        assert!(output.contains("# TYPE fullnode_compute_cpu_seconds_total counter"));
        assert!(output.contains("fullnode_coin_path_events_total{event=\"prefetch_hit\"} 42\n"));
        assert!(
            output.contains("fullnode_store_operation_calls_total{operation=\"coin_prepare\"}")
        );
        assert!(
            output.contains("fullnode_store_operation_rows_total{operation=\"coin_lookup\"} 256\n")
        );
        assert!(
            output.contains(
                "fullnode_store_operation_statements_total{operation=\"coin_lookup\"} 1\n"
            )
        );
        assert!(output.contains("fullnode_vdf_cache_misses_total{cache=\"discriminant\"}"));
        assert!(
            output
                .contains("fullnode_sqlite_checkpoint_requests_total{reason=\"write_budget\"} 7\n")
        );
        assert!(
            output.contains("fullnode_sqlite_checkpoint_mode_calls_total{mode=\"passive\"} 9\n")
        );
        assert!(
            output.contains("fullnode_sqlite_checkpoint_mode_seconds_total{mode=\"passive\"} 3\n")
        );
        assert!(output.contains("fullnode_sqlite_wal_outstanding_frames 11\n"));
        assert!(output.contains("fullnode_sqlite_checkpoint_no_progress_seconds 12\n"));
    }

    #[test]
    fn every_performance_metric_is_charted() {
        let dashboard: serde_json::Value = serde_json::from_str(include_str!(
            "../../../docs/grafana/dg-xch-sync-overview.json"
        ))
        .unwrap();
        let expressions: Vec<_> = dashboard["panels"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|panel| panel["targets"].as_array())
            .flatten()
            .filter_map(|target| target["expr"].as_str())
            .collect();
        let mut output = String::new();
        render(
            &mut output,
            Some(&StoreTelemetry::default()),
            &SyncMetrics::default(),
        );
        for metric in output
            .lines()
            .filter_map(|line| line.strip_prefix("# TYPE "))
        {
            let name = metric.split_whitespace().next().unwrap();
            assert!(
                expressions
                    .iter()
                    .any(|expression| expression.contains(name)),
                "missing chart: {name}"
            );
        }
    }
}
