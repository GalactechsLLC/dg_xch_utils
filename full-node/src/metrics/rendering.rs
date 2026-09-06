use super::*;

// Render one histogram family in Prometheus text format: for each (label, snapshot) pair a full
// cumulative bucket ladder + the implicit +Inf, then _sum (seconds) and _count. An empty label
// string renders the bare series (no `{phase=...}`).
fn render_histogram(
    out: &mut String,
    name: &str,
    help: &str,
    series: &[(&str, &HistogramSnapshot)],
) {
    out.push_str(&format!("# HELP {name} {help}\n"));
    out.push_str(&format!("# TYPE {name} histogram\n"));
    for (phase, h) in series {
        let label = |le: &str| {
            if phase.is_empty() {
                format!("{{le=\"{le}\"}}")
            } else {
                format!("{{phase=\"{phase}\",le=\"{le}\"}}")
            }
        };
        for (i, ub) in DURATION_BUCKETS_SECS.iter().enumerate() {
            out.push_str(&format!(
                "{name}_bucket{} {}\n",
                label(&format!("{ub}")),
                h.buckets[i]
            ));
        }
        out.push_str(&format!("{name}_bucket{} {}\n", label("+Inf"), h.count));
        let suffix = if phase.is_empty() {
            String::new()
        } else {
            format!("{{phase=\"{phase}\"}}")
        };
        #[allow(clippy::cast_precision_loss)]
        let sum_secs = h.sum_micros as f64 / 1_000_000.0;
        out.push_str(&format!("{name}_sum{suffix} {sum_secs}\n"));
        out.push_str(&format!("{name}_count{suffix} {}\n", h.count));
    }
}
/// Render a snapshot to Prometheus text-exposition format (v0.0.4). Every series is a gauge/counter with a
/// HELP + TYPE line; the whole node's live sync state in one scrape.
#[must_use]
pub fn render_metrics(s: &MetricsSnapshot) -> String {
    let mut out = String::with_capacity(1024);
    let g = |out: &mut String, name: &str, help: &str, kind: &str, val: u64| {
        out.push_str(&format!("# HELP {name} {help}\n"));
        out.push_str(&format!("# TYPE {name} {kind}\n"));
        out.push_str(&format!("{name} {val}\n"));
    };
    g(
        &mut out,
        "fullnode_peak_height",
        "Confirmed local peak height.",
        "gauge",
        s.peak_height,
    );
    g(
        &mut out,
        "fullnode_claimed_peak_height",
        "Highest peer-announced peak height.",
        "gauge",
        s.claimed_peak,
    );
    g(
        &mut out,
        "fullnode_tip_lag",
        "Blocks behind the best peer-announced tip (claimed - confirmed, floored at 0; also 0 before any peer announces - read with the peer gauges).",
        "gauge",
        s.tip_lag,
    );
    // Absent (not 0) on an empty store: 0 must always mean "genesis-synced".
    if let Some(base) = s.sync_base_height {
        g(
            &mut out,
            "fullnode_sync_base_height",
            "Lowest confirmed main-chain height in the local store (0 = genesis-synced; an era-anchored node reports its backfilled floor). Left edge of the sync-progress bar.",
            "gauge",
            base,
        );
    }
    g(
        &mut out,
        "fullnode_blocks_downloaded_total",
        "Block bodies downloaded by the sync pipeline.",
        "counter",
        s.blocks_downloaded,
    );
    g(
        &mut out,
        "fullnode_blocks_confirmed_total",
        "Blocks validated and confirmed into the store.",
        "counter",
        s.blocks_confirmed,
    );
    g(
        &mut out,
        "fullnode_reservations_reclaimed_total",
        "Reservation windows reclaimed from stalled peers.",
        "counter",
        s.reclaimed,
    );
    g(
        &mut out,
        "fullnode_peak_reservation_window",
        "Peak in-flight reservation-window identifiers.",
        "gauge",
        s.peak_window,
    );
    g(
        &mut out,
        "fullnode_peak_inflight_blocks",
        "Peak blocks held by the headers-first body downloader; 0 when that pipeline is unused (including genesis follow sync).",
        "gauge",
        s.peak_inflight_blocks,
    );
    g(
        &mut out,
        "fullnode_process_resident_bytes",
        "Process resident set size in bytes.",
        "gauge",
        s.rss_bytes,
    );
    g(
        &mut out,
        "fullnode_outbound_peers",
        "Live outbound peer connections.",
        "gauge",
        s.outbound_peers,
    );
    g(
        &mut out,
        "fullnode_inbound_peers",
        "Live inbound peer connections.",
        "gauge",
        s.inbound_peers,
    );
    g(
        &mut out,
        "fullnode_inbound_connections",
        "Resident entries in the peer server's inbound connection map (true inbound session count; a monotonic climb is the retention witness).",
        "gauge",
        s.inbound_connections,
    );
    g(
        &mut out,
        "fullnode_window_vdf_micros",
        "Last sync window VDF-drain wall time in microseconds.",
        "gauge",
        s.window_vdf_micros,
    );
    g(
        &mut out,
        "fullnode_window_sig_micros",
        "Last sync window header-signature-drain wall time in microseconds.",
        "gauge",
        s.window_sig_micros,
    );
    g(
        &mut out,
        "fullnode_window_body_provided",
        "Bodies of the last sync window handed in precomputed by the driver's cross-window pipeline (the rest ran inline in window.body).",
        "gauge",
        s.window_body_provided,
    );
    g(
        &mut out,
        "fullnode_window_pre_wait_micros",
        "Microseconds the driver waited joining the previous window's body precompute before starting the last window (0 = precompute finished in time or none ran).",
        "gauge",
        s.window_pre_wait_micros,
    );
    g(
        &mut out,
        "fullnode_window_drain_wait_micros",
        "Microseconds the confirm waited on the previous window's spawned vdf/sig drain (the stage-ahead pipeline's backpressure signal).",
        "gauge",
        s.window_drain_wait_micros,
    );
    g(
        &mut out,
        "fullnode_window_body_micros",
        "Last sync window parallel body-precompute wall time in microseconds.",
        "gauge",
        s.window_body_micros,
    );
    g(
        &mut out,
        "fullnode_window_stage_micros",
        "Last sync window sequential staging-loop wall time in microseconds (per-block store reads + record derivation) — the residue phase between body precompute and VDF drain.",
        "gauge",
        s.window_stage_micros,
    );
    g(
        &mut out,
        "fullnode_window_confirm_micros",
        "Last sync window batched-confirm wall time in microseconds.",
        "gauge",
        s.window_confirm_micros,
    );
    g(
        &mut out,
        "fullnode_follow_fetch_wait_micros_total",
        "Cumulative producer-side microseconds awaiting follow-window network fetches (readahead take + direct-fetch fallback); this can overlap consumer processing.",
        "counter",
        s.follow_fetch_wait_micros,
    );
    g(
        &mut out,
        "fullnode_follow_step_micros_total",
        "Cumulative consumer-cycle wall time for follow windows, including queue-head wait, stage, VDF/signature drain, confirm, and recovery.",
        "counter",
        s.follow_step_micros,
    );
    g(
        &mut out,
        "fullnode_readahead_depth",
        "Current adaptive readahead depth K (windows fetched-or-in-flight ahead of the validator).",
        "gauge",
        s.readahead_depth,
    );
    g(
        &mut out,
        "fullnode_readahead_inflight_windows",
        "Windows currently fetched-or-in-flight in the follow readahead.",
        "gauge",
        s.readahead_inflight,
    );
    g(
        &mut out,
        "fullnode_readahead_hits_total",
        "Follow windows served from the readahead pipeline.",
        "counter",
        s.readahead_hits,
    );
    g(
        &mut out,
        "fullnode_readahead_misses_total",
        "Follow windows that fell back to a direct fetch (failed head fetch or replan).",
        "counter",
        s.readahead_misses,
    );
    g(
        &mut out,
        "fullnode_queue_resident_bytes",
        "Present block bytes held in the reorder buffer — the prefetch share of live allocation.",
        "gauge",
        s.queue_resident_bytes,
    );
    g(
        &mut out,
        "fullnode_outbound_tip",
        "Highest fresh OUTBOUND peer claim (the servable fetch frontier the follow producer clamps to); 0 = none.",
        "gauge",
        s.outbound_tip,
    );
    g(
        &mut out,
        "fullnode_queue_len",
        "Slots (in-flight + present) the reorder buffer holds ahead of the consumer.",
        "gauge",
        s.queue_len,
    );
    g(
        &mut out,
        "fullnode_window_blocks",
        "Blocks in the last sync window.",
        "gauge",
        s.window_blocks,
    );
    g(
        &mut out,
        "fullnode_window_tx_blocks",
        "Blocks in the last sync window carrying a transactions generator (the only ones window.body runs).",
        "gauge",
        s.window_tx_blocks,
    );
    g(
        &mut out,
        "fullnode_alloc_allocated_bytes",
        "jemalloc live allocated bytes (true retention signal).",
        "gauge",
        s.alloc_allocated,
    );
    g(
        &mut out,
        "fullnode_alloc_active_bytes",
        "jemalloc active bytes (pages backing live allocations; active - allocated = internal fragmentation).",
        "gauge",
        s.alloc_active,
    );
    g(
        &mut out,
        "fullnode_alloc_resident_bytes",
        "jemalloc resident bytes (allocated + allocator holdback).",
        "gauge",
        s.alloc_resident,
    );
    g(
        &mut out,
        "fullnode_alloc_retained_bytes",
        "jemalloc retained bytes (VM kept back from the OS, not in RSS; the allocator-retention half of the RSS attribution).",
        "gauge",
        s.alloc_retained,
    );
    g(
        &mut out,
        "fullnode_engine_cache_records",
        "Block records in the engine's bounded walk cache.",
        "gauge",
        s.engine_cache_records,
    );
    g(
        &mut out,
        "fullnode_engine_pending_orphans",
        "Blocks parked in the engine's pending-orphan map (parent unknown).",
        "gauge",
        s.engine_pending_orphans,
    );
    g(
        &mut out,
        "fullnode_engine_staged_generators",
        "Generators staged for the in-flight window (drained at confirm).",
        "gauge",
        s.engine_staged_generators,
    );
    g(
        &mut out,
        "fullnode_difficulty_window_cache_hits_total",
        "Consensus-walk records served from the in-memory record window (epoch-trough fix).",
        "counter",
        s.difficulty_window_cache_hits,
    );
    g(
        &mut out,
        "fullnode_difficulty_window_store_reads_total",
        "Consensus-walk records point-read from the store (cold start + per-peak head delta only once warm).",
        "counter",
        s.difficulty_window_store_reads,
    );
    g(
        &mut out,
        "fullnode_sync_from_height",
        "Configured --sync-from anchor height (0 = genesis).",
        "gauge",
        s.sync_from,
    );
    g(
        &mut out,
        "fullnode_net_bytes_in_total",
        "Bytes received on peer links (message payloads).",
        "counter",
        s.bytes_in,
    );
    g(
        &mut out,
        "fullnode_net_bytes_out_total",
        "Bytes sent on peer links (message payloads).",
        "counter",
        s.bytes_out,
    );
    g(
        &mut out,
        "fullnode_mempool_size",
        "Spend bundles resident in the mempool.",
        "gauge",
        s.mempool_size,
    );
    g(
        &mut out,
        "fullnode_mempool_cost",
        "Total CLVM cost resident in the mempool.",
        "gauge",
        s.mempool_cost,
    );
    g(
        &mut out,
        "fullnode_mempool_max_total_cost",
        "Mempool capacity ceiling in CLVM cost.",
        "gauge",
        s.mempool_max_cost,
    );
    g(
        &mut out,
        "fullnode_current_signage_point",
        "Index of the latest accepted signage point (0-63).",
        "gauge",
        s.sp_current_index,
    );
    g(
        &mut out,
        "fullnode_signage_points_total",
        "Signage points accepted since startup.",
        "counter",
        s.signage_points_total,
    );
    g(
        &mut out,
        "fullnode_last_reorg_depth",
        "Depth of the most recent reorg (0 = none observed).",
        "gauge",
        s.last_reorg_depth,
    );
    // Store commit/WAL/checkpoint telemetry — rendered only when the backend records it
    // (sqlite), so a backend without it exports NO store series rather than misleading zeros.
    if let Some(st) = &s.store {
        g(
            &mut out,
            "fullnode_sqlite_wal_bytes",
            "Current size of the SQLite -wal file in bytes (the bounded-WAL witness).",
            "gauge",
            st.wal_bytes,
        );
        g(
            &mut out,
            "fullnode_store_near_tip",
            "1 = near-tip band (per-block commits); 0 = catch-up band (window batch commits). Background checkpointing is active in both bands.",
            "gauge",
            st.near_tip,
        );
        render_histogram(
            &mut out,
            "fullnode_store_commit_seconds",
            "Writer batch COMMIT latency (body-append and confirm transactions), by confirm phase.",
            &[
                ("catch_up", &st.commit_catch_up),
                ("near_tip", &st.commit_near_tip),
            ],
        );
        render_histogram(
            &mut out,
            "fullnode_sqlite_checkpoint_seconds",
            "Successful WAL checkpoint pragma duration across all modes on the dedicated connection.",
            &[("", &st.checkpoint)],
        );
        g(
            &mut out,
            "fullnode_sqlite_wal_frames",
            "WAL length in frames as of the last checkpoint (the pragma's log column).",
            "gauge",
            st.wal_frames,
        );
        g(
            &mut out,
            "fullnode_sqlite_wal_frames_checkpointed_total",
            "WAL frames copied into the main DB by checkpoints.",
            "counter",
            st.wal_frames_checkpointed_total,
        );
        g(
            &mut out,
            "fullnode_sqlite_checkpoint_busy_total",
            "Checkpoint results with a nonzero busy flag; PASSIVE may be incomplete even with busy zero.",
            "counter",
            st.checkpoint_busy_total,
        );
        g(
            &mut out,
            "fullnode_sqlite_checkpoint_errors_total",
            "Checkpoint pragmas that failed outright (WAL not being drained).",
            "counter",
            st.checkpoint_errors_total,
        );
        g(
            &mut out,
            "fullnode_store_record_reads_total",
            "Block-record point reads on the read path (each element of a multi-get counts) — rate against the window cadence attributes the staging loop's read serialization.",
            "counter",
            st.record_reads,
        );
        g(
            &mut out,
            "fullnode_store_coin_reads_total",
            "Coin-record point reads on the read path (each element of a multi-get counts) — the confirmed-set validation read volume.",
            "counter",
            st.coin_reads,
        );
        g(
            &mut out,
            "fullnode_sqlite_read_pool_idle",
            "Read-pool connections currently idle; this count alone does not establish a pinned WAL reader.",
            "gauge",
            st.read_pool_idle,
        );
        g(
            &mut out,
            "fullnode_sqlite_read_pool_size",
            "Read-pool total connections (idle + in-use).",
            "gauge",
            st.read_pool_size,
        );
    }
    // Block-producer pipeline — the first-block funnel. Read top-to-bottom: the first counter
    // that is 0 while the one above it is > 0 names the stalled stage, and the
    // candidates_dropped{reason} with the count is the exact wall.
    g(
        &mut out,
        "fullnode_producer_declares_received_total",
        "DeclareProofOfSpace messages received from farmers.",
        "counter",
        s.producer_declares_received,
    );
    g(
        &mut out,
        "fullnode_producer_candidates_built_total",
        "Candidate unfinished blocks assembled from accepted proofs.",
        "counter",
        s.producer_candidates_built,
    );
    g(
        &mut out,
        "fullnode_producer_request_signed_values_total",
        "RequestSignedValues messages returned to farmers to sign.",
        "counter",
        s.producer_request_signed_values,
    );
    g(
        &mut out,
        "fullnode_producer_signed_values_received_total",
        "SignedValues replies received from farmers.",
        "counter",
        s.producer_signed_values_received,
    );
    g(
        &mut out,
        "fullnode_producer_ub_assembled_total",
        "Finished unfinished blocks (farmer foliage signatures spliced).",
        "counter",
        s.producer_ub_assembled,
    );
    g(
        &mut out,
        "fullnode_producer_full_blocks_total",
        "Full blocks confirmed from OUR farmed unfinished blocks (terminal success).",
        "counter",
        s.producer_full_blocks,
    );
    out.push_str(
        "# HELP fullnode_producer_declares_validated_total Declares validated, by result.\n",
    );
    out.push_str("# TYPE fullnode_producer_declares_validated_total counter\n");
    for (result, n) in &s.producer_validated {
        out.push_str(&format!(
            "fullnode_producer_declares_validated_total{{result=\"{result}\"}} {n}\n"
        ));
    }
    out.push_str(
        "# HELP fullnode_producer_candidates_dropped_total Producer-pipeline drops, by reason.\n",
    );
    out.push_str("# TYPE fullnode_producer_candidates_dropped_total counter\n");
    for (reason, n) in &s.producer_candidates_dropped {
        out.push_str(&format!(
            "fullnode_producer_candidates_dropped_total{{reason=\"{reason}\"}} {n}\n"
        ));
    }
    out.push_str(
        "# HELP fullnode_producer_candidates_requeued_total Producer candidates re-queued after a transient wall (retried, not lost), by reason.\n",
    );
    out.push_str("# TYPE fullnode_producer_candidates_requeued_total counter\n");
    for (reason, n) in &s.producer_candidates_requeued {
        out.push_str(&format!(
            "fullnode_producer_candidates_requeued_total{{reason=\"{reason}\"}} {n}\n"
        ));
    }
    out.push_str(
        "# HELP fullnode_producer_ub_broadcast_total Unfinished-block announcements sent, by peer type.\n",
    );
    out.push_str("# TYPE fullnode_producer_ub_broadcast_total counter\n");
    for (pt, n) in &s.producer_ub_broadcast {
        out.push_str(&format!(
            "fullnode_producer_ub_broadcast_total{{peer_type=\"{pt}\"}} {n}\n"
        ));
    }
    // Per-message-type traffic — the gossip-health series (is each protocol conversation
    // actually flowing, in which direction, at what rate).
    out.push_str(
        "# HELP fullnode_net_messages_in_total Messages received on peer links, by type.\n",
    );
    out.push_str("# TYPE fullnode_net_messages_in_total counter\n");
    for (label, n) in &s.messages_in {
        out.push_str(&format!(
            "fullnode_net_messages_in_total{{msg=\"{label}\"}} {n}\n"
        ));
    }
    out.push_str("# HELP fullnode_net_messages_out_total Messages sent on peer links, by type.\n");
    out.push_str("# TYPE fullnode_net_messages_out_total counter\n");
    for (label, n) in &s.messages_out {
        out.push_str(&format!(
            "fullnode_net_messages_out_total{{msg=\"{label}\"}} {n}\n"
        ));
    }
    out
}
