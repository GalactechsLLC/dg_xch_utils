use super::{
    BOOT_GRACE_SECS, CONFIRM_MAX_SECS, HealthState, MetricsSnapshot, STALL_SECS, StoreSnapshot,
    render_metrics,
};
use dg_xch_stores::HistogramSnapshot;

fn below_tip_with_peers() -> MetricsSnapshot {
    MetricsSnapshot {
        peak_height: 50,
        claimed_peak: 100,
        outbound_peers: 5,
        ..Default::default()
    }
}

const BOOT: u64 = 1_000_000;

// Inside the boot grace window the probe is healthy even when below tip, with peers, and no
// progress recorded — a cold start must not be killed while it fetches + verifies its first proof.
#[test]
fn boot_grace_is_healthy_even_when_below_tip() {
    let hs = HealthState::new_at(BOOT);
    let v = hs.verdict(&below_tip_with_peers(), BOOT + BOOT_GRACE_SECS - 1, 0);
    assert_eq!(v.status, "200 OK");
    assert!(v.body.contains("boot grace"), "body: {}", v.body);
}

// A node whose confirmed peak is at or above the best peer-announced tip is caught up: pausing
// between blocks is not a stall, so it stays healthy however long since the last advance.
#[test]
fn caught_up_is_healthy() {
    let hs = HealthState::new_at(BOOT);
    let snap = MetricsSnapshot {
        peak_height: 100,
        claimed_peak: 100,
        outbound_peers: 5,
        ..Default::default()
    };
    // Far past grace and far past the stall window — caught-up must still win.
    let v = hs.verdict(&snap, BOOT + BOOT_GRACE_SECS + STALL_SECS * 10, 0);
    assert_eq!(v.status, "200 OK");
    assert!(v.body.contains("caught up"), "body: {}", v.body);
}

// No outbound peers = nothing to sync from; a restart cannot fix an empty address book, so the
// probe must not thrash the pod for a network-side condition.
#[test]
fn no_peers_is_healthy() {
    let hs = HealthState::new_at(BOOT);
    let snap = MetricsSnapshot {
        peak_height: 50,
        claimed_peak: 100,
        outbound_peers: 0,
        ..Default::default()
    };
    let v = hs.verdict(&snap, BOOT + BOOT_GRACE_SECS + STALL_SECS + 5, 0);
    assert_eq!(v.status, "200 OK");
    assert!(v.body.contains("no outbound peers"), "body: {}", v.body);
}

// Below tip, with peers, past grace, but progress recorded inside the stall window: healthy.
#[test]
fn recent_progress_is_healthy() {
    let hs = HealthState::new_at(BOOT);
    let progressed_at = BOOT + BOOT_GRACE_SECS + 10;
    hs.observe(60, 0, progressed_at); // confirmed peak advanced 0 -> 60
    let v = hs.verdict(&below_tip_with_peers(), progressed_at + STALL_SECS - 1, 0);
    assert_eq!(v.status, "200 OK");
    assert!(v.body.contains("advancing"), "body: {}", v.body);
}

// The silent-stall signature: below tip, with peers, past grace, no advance for > STALL_SECS.
// This is the exact condition the worker-thread panic produced and the one the orchestrator must restart.
#[test]
fn stalled_below_tip_is_unhealthy() {
    let hs = HealthState::new_at(BOOT);
    // last_progress is seeded to BOOT; evaluate well past both grace and the stall window.
    let now = BOOT + STALL_SECS + BOOT_GRACE_SECS + 1;
    let v = hs.verdict(&below_tip_with_peers(), now, 0);
    assert_eq!(v.status, "503 Service Unavailable");
    assert!(v.body.contains("stalled"), "body: {}", v.body);
}

// The download counter is a secondary progress witness: during fast-sync the confirmed peak does
// not move until a window lands, but blocks_downloaded climbs — that must count as liveness.
#[test]
fn download_progress_alone_is_healthy() {
    let hs = HealthState::new_at(BOOT);
    let t = BOOT + BOOT_GRACE_SECS + 10;
    hs.observe(0, 5000, t); // peak flat, only blocks_downloaded advanced
    let v = hs.verdict(&below_tip_with_peers(), t + STALL_SECS - 1, 0);
    assert_eq!(v.status, "200 OK");
    assert!(v.body.contains("advancing"), "body: {}", v.body);
}

// A /debug/heap request against a process without ACTIVE jemalloc profiling
// must fail loud with the exact activation env var — never resolve to an empty profile. This
// holds in BOTH unprofiled environments: a prof-less build (macOS dev / non-`profiling`
// feature: opt.prof itself is absent) and a prof-compiled build started without
// `_RJEM_MALLOC_CONF` (opt.prof reads false). The only environment where it may succeed is a
// live deploy that set the env var — which no test runner does.
#[test]
#[cfg(feature = "profiling")]
fn heap_dump_without_active_profiling_fails_naming_the_env_var() {
    let err = super::profiling::jemalloc_prof_dump()
        .expect_err("dump must not succeed without _RJEM_MALLOC_CONF");
    assert!(
        err.contains("_RJEM_MALLOC_CONF=prof:true,prof_active:true,lg_prof_sample:19"),
        "error must carry the exact activation string, got: {err}"
    );
}

// The render must expose every counter/gauge as a Prometheus
// series with its value, including the peak and claimed heights the dashboard graphs.
#[test]
fn render_exposes_all_series_with_values() {
    let mut commit_near_tip = HistogramSnapshot::default();
    commit_near_tip.buckets[3] = 7; // <= 0.1s
    commit_near_tip.count = 9;
    commit_near_tip.sum_micros = 1_500_000; // 1.5s total
    let snap = MetricsSnapshot {
        peak_height: 9_054_698,
        claimed_peak: 9_054_720,
        tip_lag: 22,
        sync_base_height: Some(4_575_000),
        store: Some(StoreSnapshot {
            wal_bytes: 487_000_000,
            near_tip: 1,
            commit_catch_up: HistogramSnapshot::default(),
            commit_near_tip,
            checkpoint: HistogramSnapshot {
                buckets: [5; 12],
                count: 6,
                sum_micros: 250_000,
            },
            wal_frames: 1_200,
            wal_frames_checkpointed_total: 88_000,
            checkpoint_busy_total: 4,
            checkpoint_errors_total: 1,
            record_reads: 65_432,
            coin_reads: 12_345,
            read_pool_idle: 3,
            read_pool_size: 4,
        }),
        blocks_downloaded: 42,
        blocks_confirmed: 40,
        reclaimed: 3,
        peak_window: 256,
        peak_inflight_blocks: 32,
        rss_bytes: 123_456_789,
        outbound_peers: 8,
        inbound_peers: 5,
        inbound_connections: 37,
        window_vdf_micros: 2_660_000,
        window_sig_micros: 44_000,
        window_body_micros: 120_000,
        window_stage_micros: 1_900_000,
        window_confirm_micros: 9_500,
        window_blocks: 32,
        window_tx_blocks: 13,
        window_body_provided: 11,
        window_pre_wait_micros: 71_000,
        window_drain_wait_micros: 380_000,
        alloc_allocated: 900_000_000,
        alloc_active: 950_000_000,
        alloc_resident: 1_100_000_000,
        alloc_retained: 4_000_000_000,
        engine_cache_records: 4_096,
        engine_pending_orphans: 17,
        engine_staged_generators: 12,
        difficulty_window_cache_hits: 51_310,
        difficulty_window_store_reads: 32,
        follow_fetch_wait_micros: 777_000,
        follow_step_micros: 1_888_000,
        readahead_depth: 6,
        readahead_inflight: 4,
        readahead_hits: 30,
        readahead_misses: 2,
        queue_resident_bytes: 268_435_456,
        queue_len: 96,
        outbound_tip: 9_100_000,
        sync_from: 4_575_000,
        messages_in: vec![("new_peak", 7), ("new_transaction", 3)],
        messages_out: vec![("request_transaction", 2)],
        bytes_in: 10_240,
        bytes_out: 2_048,
        mempool_size: 5,
        mempool_cost: 1_000_000,
        mempool_max_cost: 550_000_000_000,
        sp_current_index: 41,
        signage_points_total: 1_234,
        last_reorg_depth: 2,
        producer_declares_received: 88,
        producer_candidates_built: 55,
        producer_request_signed_values: 44,
        producer_signed_values_received: 33,
        producer_ub_assembled: 22,
        producer_full_blocks: 11,
        producer_validated: vec![("accepted", 50), ("pospace_verify_fail", 4)],
        producer_candidates_dropped: vec![("no_timelord_peer", 6), ("ub_prev_unknown", 2)],
        producer_candidates_requeued: vec![("ub_prev_store_error", 3)],
        producer_ub_broadcast: vec![("timelord", 9)],
    };
    let text = render_metrics(&snap);
    assert!(text.contains("fullnode_peak_height 9054698"));
    assert!(text.contains("fullnode_claimed_peak_height 9054720"));
    assert!(text.contains("fullnode_blocks_downloaded_total 42"));
    assert!(text.contains("fullnode_blocks_confirmed_total 40"));
    assert!(text.contains("fullnode_reservations_reclaimed_total 3"));
    assert!(text.contains("fullnode_peak_reservation_window 256"));
    assert!(text.contains("fullnode_peak_inflight_blocks 32"));
    assert!(text.contains("fullnode_process_resident_bytes 123456789"));
    assert!(text.contains("fullnode_outbound_peers 8"));
    assert!(text.contains("fullnode_window_vdf_micros 2660000"));
    assert!(text.contains("fullnode_window_sig_micros 44000"));
    assert!(text.contains("fullnode_window_body_micros 120000"));
    assert!(text.contains("fullnode_window_stage_micros 1900000"));
    assert!(text.contains("fullnode_window_confirm_micros 9500"));
    assert!(text.contains("fullnode_window_blocks 32"));
    assert!(text.contains("fullnode_window_tx_blocks 13"));
    assert!(text.contains("fullnode_window_body_provided 11"));
    assert!(text.contains("fullnode_window_pre_wait_micros 71000"));
    assert!(text.contains("fullnode_window_drain_wait_micros 380000"));
    assert!(text.contains("fullnode_alloc_allocated_bytes 900000000"));
    assert!(text.contains("fullnode_alloc_resident_bytes 1100000000"));
    assert!(text.contains("fullnode_engine_cache_records 4096"));
    assert!(text.contains("fullnode_engine_pending_orphans 17"));
    assert!(text.contains("fullnode_engine_staged_generators 12"));
    assert!(text.contains("fullnode_difficulty_window_cache_hits_total 51310"));
    assert!(text.contains("fullnode_difficulty_window_store_reads_total 32"));
    assert!(text.contains("fullnode_follow_fetch_wait_micros_total 777000"));
    assert!(text.contains("fullnode_follow_step_micros_total 1888000"));
    assert!(text.contains("fullnode_readahead_depth 6"));
    assert!(text.contains("fullnode_queue_resident_bytes 268435456"));
    assert!(text.contains("fullnode_queue_len 96"));
    assert!(text.contains("fullnode_readahead_inflight_windows 4"));
    assert!(text.contains("fullnode_readahead_hits_total 30"));
    assert!(text.contains("fullnode_readahead_misses_total 2"));
    assert!(text.contains("fullnode_sync_from_height 4575000"));
    assert!(text.contains("fullnode_inbound_peers 5"));
    assert!(text.contains("fullnode_inbound_connections 37"));
    assert!(text.contains("fullnode_net_bytes_in_total 10240"));
    assert!(text.contains("fullnode_net_bytes_out_total 2048"));
    assert!(text.contains("fullnode_mempool_size 5"));
    assert!(text.contains("fullnode_mempool_cost 1000000"));
    assert!(text.contains("fullnode_mempool_max_total_cost 550000000000"));
    assert!(text.contains("fullnode_current_signage_point 41"));
    assert!(text.contains("fullnode_signage_points_total 1234"));
    assert!(text.contains("fullnode_last_reorg_depth 2"));
    assert!(text.contains("fullnode_producer_declares_received_total 88"));
    assert!(text.contains("fullnode_producer_candidates_built_total 55"));
    assert!(text.contains("fullnode_producer_request_signed_values_total 44"));
    assert!(text.contains("fullnode_producer_signed_values_received_total 33"));
    assert!(text.contains("fullnode_producer_ub_assembled_total 22"));
    assert!(text.contains("fullnode_producer_full_blocks_total 11"));
    assert!(text.contains("fullnode_producer_declares_validated_total{result=\"accepted\"} 50"));
    assert!(
        text.contains(
            "fullnode_producer_candidates_requeued_total{reason=\"ub_prev_store_error\"} 3"
        )
    );
    assert!(
        text.contains("fullnode_producer_candidates_dropped_total{reason=\"no_timelord_peer\"} 6")
    );
    assert!(text.contains("fullnode_producer_ub_broadcast_total{peer_type=\"timelord\"} 9"));
    assert!(text.contains("fullnode_net_messages_in_total{msg=\"new_peak\"} 7"));
    assert!(text.contains("fullnode_net_messages_in_total{msg=\"new_transaction\"} 3"));
    assert!(text.contains("fullnode_net_messages_out_total{msg=\"request_transaction\"} 2"));
    // TYPE lines are present (Prometheus rejects a series without one).
    assert!(text.contains("# TYPE fullnode_peak_height gauge"));
    assert!(text.contains("# TYPE fullnode_blocks_downloaded_total counter"));
    // The tip-lag / sync-floor / memory-attribution gauges.
    assert!(text.contains("fullnode_tip_lag 22"));
    assert!(text.contains("fullnode_sync_base_height 4575000"));
    assert!(text.contains("fullnode_alloc_active_bytes 950000000"));
    assert!(text.contains("fullnode_alloc_retained_bytes 4000000000"));
    // WAL + phase gauges.
    assert!(text.contains("fullnode_sqlite_wal_bytes 487000000"));
    assert!(text.contains("fullnode_store_near_tip 1"));
    assert!(text.contains("fullnode_sqlite_wal_frames 1200"));
    assert!(text.contains("fullnode_sqlite_wal_frames_checkpointed_total 88000"));
    assert!(text.contains("fullnode_sqlite_checkpoint_busy_total 4"));
    assert!(text.contains("fullnode_sqlite_checkpoint_errors_total 1"));
    assert!(text.contains("fullnode_store_record_reads_total 65432"));
    assert!(text.contains("fullnode_store_coin_reads_total 12345"));
    // The phase-labelled commit histogram in full Prometheus histogram shape —
    // cumulative buckets, +Inf, _sum in seconds, _count.
    assert!(text.contains("# TYPE fullnode_store_commit_seconds histogram"));
    assert!(text.contains("fullnode_store_commit_seconds_bucket{phase=\"near_tip\",le=\"0.1\"} 7"));
    assert!(
        text.contains("fullnode_store_commit_seconds_bucket{phase=\"near_tip\",le=\"+Inf\"} 9")
    );
    assert!(text.contains("fullnode_store_commit_seconds_sum{phase=\"near_tip\"} 1.5"));
    assert!(text.contains("fullnode_store_commit_seconds_count{phase=\"near_tip\"} 9"));
    assert!(
        text.contains("fullnode_store_commit_seconds_bucket{phase=\"catch_up\",le=\"+Inf\"} 0")
    );
    // The unlabelled checkpoint histogram renders bare le buckets.
    assert!(text.contains("fullnode_sqlite_checkpoint_seconds_bucket{le=\"0.01\"} 5"));
    assert!(text.contains("fullnode_sqlite_checkpoint_seconds_bucket{le=\"+Inf\"} 6"));
    assert!(text.contains("fullnode_sqlite_checkpoint_seconds_count 6"));
}

// A backend that records no store telemetry (mmap, postgres) must export NO store series at all
// — zeros would read as "commits observed, all instant" and poison the latency queries.
#[test]
fn render_skips_store_series_when_backend_has_none() {
    let snap = MetricsSnapshot {
        peak_height: 10,
        claimed_peak: 12,
        tip_lag: 2,
        store: None,
        sync_base_height: None,
        ..Default::default()
    };
    let text = render_metrics(&snap);
    assert!(
        text.contains("fullnode_tip_lag 2"),
        "tip lag is backend-independent"
    );
    assert!(!text.contains("fullnode_sqlite_wal_bytes"));
    assert!(!text.contains("fullnode_store_commit_seconds"));
    assert!(!text.contains("fullnode_store_near_tip"));
    // An empty store has NO sync floor — absent, never a fake genesis 0.
    assert!(!text.contains("fullnode_sync_base_height"));
}

#[test]
fn confirm_in_flight_is_healthy_though_peak_is_frozen() {
    let hs = HealthState::new_at(BOOT);
    // Evaluate long past both grace and the stall window with no observed progress.
    let now = BOOT + BOOT_GRACE_SECS + STALL_SECS * 3;
    // A confirm went in flight recently and is still running (well under the ceiling).
    let inflight_since = now - 40;
    let v = hs.verdict(&below_tip_with_peers(), now, inflight_since);
    assert_eq!(v.status, "200 OK");
    assert!(v.body.contains("confirm in flight"), "body: {}", v.body);
}

// The anti-unkillable guard: a confirm that has been "in flight" past CONFIRM_MAX_SECS with no
// peak advance is not a slow write, it is a wedged/deadlocked writer — the node must still 503 so
// the orchestrator can restart it. Without this ceiling a permanent deadlock would keep the pod
// alive forever behind a stuck confirm.
#[test]
fn confirm_in_flight_past_ceiling_is_unhealthy() {
    let hs = HealthState::new_at(BOOT);
    let now = BOOT + BOOT_GRACE_SECS + STALL_SECS * 3;
    // In flight longer than the ceiling permits — a genuine deadlock, not a legitimate commit.
    let inflight_since = now - (CONFIRM_MAX_SECS + 5);
    let v = hs.verdict(&below_tip_with_peers(), now, inflight_since);
    assert_eq!(v.status, "503 Service Unavailable");
    assert!(v.body.contains("stalled"), "body: {}", v.body);
}

#[test]
fn stall_dump_debounces_per_episode() {
    let hs = HealthState::new_at(BOOT);
    assert!(hs.should_dump_stall(), "first 503 of the episode dumps");
    assert!(!hs.should_dump_stall(), "second 503 is suppressed");
    assert!(!hs.should_dump_stall(), "third 503 is suppressed");
    hs.clear_stall(); // a 200 ends the episode
    assert!(hs.should_dump_stall(), "next episode dumps again");
}
