use super::super::*;
use dg_xch_core::blockchain::coin_record::CoinRecord;
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn follow_step_timer_records_consumer_cycle_time() {
    let metric = std::sync::atomic::AtomicU64::new(0);
    let started = std::time::Instant::now() - std::time::Duration::from_millis(2);
    {
        let _timer = super::super::sync::FollowStepTimer::new(&metric, started);
    }
    assert!(
        metric.load(std::sync::atomic::Ordering::Relaxed) >= 2_000,
        "elapsed consumer-cycle time must be accumulated"
    );
}

// A subscribed coin spent on branch A must read UNSPENT again after a reorg to branch B where
// the spend never happened, which means delivering the POST-ROLLBACK records to subscribers.
// Reporting only the reorg tip's own delta would leave the subscriber hearing nothing about
// the rollback. Drives the server's confirm tail (finish_follow_step) with exactly what the
// chaser produces for a landed reorg.
#[tokio::test]
async fn reorg_rollback_states_reach_subscribed_wallets() {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let db = std::env::temp_dir().join(format!(
        "fn_reorg_wallet_{}_{nanos}.sqlite",
        std::process::id()
    ));
    let node = FullNode::boot(Config {
        p2p: P2pSettings::default(),
        listen: "127.0.0.1:0".parse().unwrap(),
        rpc: "127.0.0.1:0".parse().unwrap(),
        introducer: None,
        manual_peers: Vec::new(),
        advertise: None,
        backend: Backend::Sqlite(db),
        network_id: "mainnet".to_string(),
        capture_dir: None,
        genesis_sync: false,
        sync_from: 0,
        uncompact: false,
        prefetch_memory_mb: None,
        prefetch_max_inflight: None,
        trusted_peers: Vec::new(),
        trusted_cidrs: Vec::new(),
        rpc_tls: crate::config::RpcTlsMode::Local,
        debug_endpoints: false,
    })
    .await
    .expect("boot");

    // Coin X: created at 90 (below the fork), spent on branch A at 101 — the reorg to
    // branch B rolls the spend back. Coin Y: created on branch A at 101 — the reorg deletes
    // it. Both post-rollback records arrive from the engine's ReorgReport.
    let x = CoinRecord {
        coin: dg_xch_core::blockchain::coin::Coin {
            parent_coin_info: Bytes32::from([0xC0; 32]),
            puzzle_hash: Bytes32::from([0xC1; 32]),
            amount: 1_000,
        },
        confirmed_block_index: 90,
        spent_block_index: 0,
        coinbase: false,
        timestamp: 1_700_000_000,
        spent: false,
    };
    let y = CoinRecord {
        coin: dg_xch_core::blockchain::coin::Coin {
            parent_coin_info: Bytes32::from([0xC2; 32]),
            puzzle_hash: Bytes32::from([0xC3; 32]),
            amount: 2_000,
        },
        confirmed_block_index: 0, // rolled back: no longer on chain
        spent_block_index: 0,
        coinbase: false,
        timestamp: 0,
        spent: false,
    };

    // A wallet peer subscribed to both coins by id.
    let peer = Bytes32::from([0x77; 32]);
    let (rx, added) = node
        .wallet
        .register_for_coin_updates(peer, None, &[x.coin.name(), y.coin.name()])
        .await
        .expect("subscribe");
    let mut rx = rx.expect("first registration hands the delivery receiver");
    assert_eq!(added.len(), 2);

    // The reorg's first re-applied block, exactly as the chaser reports it: the branch-B
    // delta with the rollback attached (fork at 100).
    let rec: dg_xch_core::blockchain::block_record::BlockRecord = {
        let records: Vec<dg_xch_core::blockchain::block_record::BlockRecord> =
            serde_json::from_str(include_str!("../../fixtures/block_records.json"))
                .expect("records fixture");
        records[0].clone()
    };
    let delta = dg_xch_node::BlockDelta {
        header_hash: Bytes32::from([0xB1; 32]),
        prev_hash: Bytes32::from([0xB0; 32]),
        height: 101,
        weight: 1_350,
        timestamp: 0, // non-transaction block: the mempool frame stays untouched
        record: rec,
        additions: Vec::new(),
        removals: Vec::new(),
        hints: Vec::new(),
    };
    let cd = ConfirmedDelta {
        delta,
        reorg: Some(ReorgWalletDelta {
            fork_height: 100,
            rolled_back: vec![
                CoinRecord {
                    spent_block_index: 0,
                    spent: false,
                    ..x
                },
                y,
            ],
        }),
    };
    node.finish_follow_step(None, std::slice::from_ref(&cd))
        .await
        .expect("confirm tail");

    let update = rx.try_recv().expect(
        "the subscriber must hear the rolled-back coin states (pre-threading it heard nothing)",
    );
    assert_eq!(
        update.fork_height, 100,
        "the TRUE fork height, not height-1"
    );
    assert_eq!(update.height, 101);
    let x_state = update
        .items
        .iter()
        .find(|s| s.coin == x.coin)
        .expect("coin X state present");
    assert_eq!(
        x_state.spent_height, None,
        "spent-on-branch-A coin reads unspent again after the reorg"
    );
    assert_eq!(x_state.created_height, Some(90));
    let y_state = update
        .items
        .iter()
        .find(|s| s.coin == y.coin)
        .expect("coin Y state present");
    assert_eq!(
        y_state.created_height, None,
        "created-on-branch-A coin reads not-on-chain after the reorg"
    );
}

#[test]
fn fast_sync_triggers_only_from_near_empty_store_far_behind() {
    assert!(wants_fast_sync(0, 9_000_000), "empty store, tip far ahead");
    assert!(
        wants_fast_sync(500, 9_000_000),
        "near-empty store, tip far ahead"
    );
    assert!(
        !wants_fast_sync(0, 10),
        "gap smaller than a follow-worthy delta"
    );
    assert!(
        !wants_fast_sync(9_000_000, 9_050_000),
        "local already synced: tip-follow owns it"
    );
    assert!(
        !wants_fast_sync(1500, 9_000_000),
        "local past the fresh-store gate"
    );
}

#[test]
fn deep_mid_chain_gap_selects_the_wp_anchored_long_sync_band() {
    // A node at 2M offline for ~a month (gap ≈ 50k blocks): long sync (gap > 300).
    assert!(
        wants_long_sync(2_000_000, 2_050_000),
        "a deep mid-chain gap must enter the WP-anchored long-sync band \
         (sync_blocks_behind_threshold = 300)"
    );
    assert!(
        !wants_long_sync(2_000_000, 2_000_300),
        "a gap at/below the threshold stays with the follow band"
    );
    // A tip below WEIGHT_PROOF_RECENT_BLOCKS (1000) cannot be weight-proof-anchored —
    // batch sync from zero covers that band.
    assert!(
        !wants_long_sync(0, 900),
        "a tip below the weight-proof floor is never long-synced"
    );
    assert!(wants_long_sync(0, 9_000_000) && wants_fast_sync(0, 9_000_000));
    assert!(wants_long_sync(1500, 9_000_000) && !wants_fast_sync(1500, 9_000_000));
}

// The gap-closes-mid-sync exit ladder: while the gap stays past the threshold
// the long-sync band owns catch-up toward the (re-polled, possibly advanced) target; within
// the threshold it hands off to the FOLLOW band; within the short-sync threshold the
// event-driven tip_follower owns the last blocks.
#[test]
fn long_sync_band_exits_cleanly_through_follow_then_near_tip() {
    let local = 2_000_000u32;
    // Deep in the gap — long sync, and the target advancing mid-sync keeps the SAME band.
    assert!(wants_long_sync(local, 2_050_000));
    assert!(
        wants_long_sync(local, 2_050_500),
        "advanced target: still long sync"
    );
    // Caught up to within the threshold: FOLLOW owns it (neither long-sync nor near-tip).
    let near = 2_050_500u32 - 200;
    assert!(!wants_long_sync(near, 2_050_500));
    assert!(!in_near_tip_band(near, 2_050_500, true));
    // Within the short-sync threshold: the tip_follower's event-driven band.
    assert!(in_near_tip_band(2_050_495, 2_050_500, true));
    // Caught up: no band wants work.
    assert!(!wants_long_sync(2_050_500, 2_050_500));
    assert!(!in_near_tip_band(2_050_500, 2_050_500, true));
}

// The fork-point → action mapping.
#[test]
fn long_sync_plan_follows_chia_fork_point_semantics() {
    let peak = 2_000_000u32;
    // No fork detected + a peer's peak+1 connects → lift to our peak: extend in place.
    assert_eq!(
        long_sync_plan(
            &WpForkPoint::NoForkDetected {
                conservative: 1_999_000
            },
            peak,
            true
        ),
        LongSyncPlan::Extend
    );
    // No fork detected but NO peer confirms our tip: keep the conservative two-sub-epoch
    // back-off — the reland re-follows from there (identical blocks are AlreadyHave).
    assert_eq!(
        long_sync_plan(
            &WpForkPoint::NoForkDetected {
                conservative: 1_999_000
            },
            peak,
            false
        ),
        LongSyncPlan::Rewind {
            fork_point: 1_999_000
        }
    );
    // A detected divergence below our peak MUST rewind through the engine reorg — never
    // blindly extend the stale branch.
    assert_eq!(
        long_sync_plan(
            &WpForkPoint::Diverged {
                fork_point: 1_998_500
            },
            peak,
            false
        ),
        LongSyncPlan::Rewind {
            fork_point: 1_998_500
        }
    );
    // No fork point within the walk window: fail closed, retry — never batch-sync blind.
    assert_eq!(
        long_sync_plan(&WpForkPoint::Unknown, peak, false),
        LongSyncPlan::Stall
    );
}

// The peer-free consumer's recovery signal round-trips through the driver
// channel and unparks it. `await_reset` sends a `()`-reply request and awaits; a mock driver drains
// the channel and replies — the RecoveryRequest → oneshot handshake the real orphan/repair/reset
// paths ride on (the consumer holds no lock while parked here).
#[tokio::test]
async fn recovery_signal_round_trips_and_unparks_the_consumer() {
    let (tx, mut rx) = mpsc::channel::<RecoveryRequest>(RECOVERY_CHANNEL_CAP);
    let driver = tokio::spawn(async move {
        match rx.recv().await {
            Some(RecoveryRequest::Orphan { from, to, reply }) => {
                assert_eq!(
                    (from, to),
                    (100, 131),
                    "the driver sees the orphaned window"
                );
                reply.send(()).is_ok()
            }
            _ => false,
        }
    });
    let unparked = await_reset(&tx, |reply| RecoveryRequest::Orphan {
        from: 100,
        to: 131,
        reply,
    })
    .await;
    assert!(unparked, "consumer unparked on the driver's reply");
    assert!(
        driver.await.unwrap(),
        "driver replied after servicing recovery"
    );
}

// Shutdown path: the driver dropped its receiver; the consumer's recovery send fails so it reports
// the channel closed and can exit its loop instead of parking forever.
#[tokio::test]
async fn recovery_send_reports_closed_channel_for_clean_consumer_exit() {
    let (tx, rx) = mpsc::channel::<RecoveryRequest>(RECOVERY_CHANNEL_CAP);
    drop(rx);
    let ok = await_reset(&tx, |reply| RecoveryRequest::Reset { reply }).await;
    assert!(
        !ok,
        "no driver → await_reset reports closed so the consumer exits"
    );
}

// A WEDGED driver that never services recovery must not hang the confirm consumer forever.
// An unbounded `await_reset` hangs permanently: the consumer stops draining the queue, the
// producer parks on a full buffer, and the whole pipeline freezes. It must return within
// RESET_REPLY_TIMEOUT and retry. The receiver is kept alive so the send succeeds but is never
// replied to; virtual time auto-advances to the internal timeout.
#[tokio::test(start_paused = true)]
async fn recovery_reply_timeout_unparks_the_consumer_when_the_driver_is_wedged() {
    // Keep the receiver alive so the request is buffered but NEVER replied to.
    let (tx, _rx_alive) = mpsc::channel::<RecoveryRequest>(RECOVERY_CHANNEL_CAP);
    // Outer bound is a test guard; the internal RESET_REPLY_TIMEOUT must fire first.
    let out = tokio::time::timeout(
        RESET_REPLY_TIMEOUT + Duration::from_secs(5),
        await_reset(&tx, |reply| RecoveryRequest::Reset { reply }),
    )
    .await;
    assert!(
        out.is_ok(),
        "await_reset must not hang when the driver never replies (bounded park)"
    );
    assert!(
        out.unwrap(),
        "on a reply-timeout the consumer proceeds (retry next window), it does not exit"
    );
}

// The confirm consumer must NEVER block on the announcer. A raw `peak_tx.send().await` on a
// full, undrained channel blocks (the first assertion proves it), so a wedged announcer stops
// the consumer draining the BlockQueue and parks the producer — a permanent wedge.
// `emit_confirmed_peak` drops the best-effort announcement under backpressure instead.
#[tokio::test]
async fn emit_confirmed_peak_never_blocks_the_consumer_on_a_wedged_announcer() {
    let (tx, _rx_wedged) = mpsc::channel::<ConfirmedPeak>(1); // never drained = stalled announcer
    tx.try_send(ConfirmedPeak {
        hash: Bytes32::default(),
        height: 1,
    })
    .expect("first fits");

    // A raw bounded send on the full channel blocks — the consumer wedge.
    let blocked = tokio::time::timeout(
        Duration::from_millis(200),
        tx.send(ConfirmedPeak {
            hash: Bytes32::default(),
            height: 2,
        }),
    )
    .await;
    assert!(
        blocked.is_err(),
        "a raw send on a full channel blocks the consumer"
    );

    // emit_confirmed_peak returns immediately (drops under backpressure) and never awaits.
    let emitted = emit_confirmed_peak(
        &tx,
        ConfirmedPeak {
            hash: Bytes32::default(),
            height: 3,
        },
    );
    assert!(
        emitted,
        "a dropped best-effort NewPeak announcement is not a fatal error — the consumer proceeds"
    );

    // And when the announcer is GONE, emit reports failure so the consumer exits cleanly.
    drop(_rx_wedged);
    assert!(
        !emit_confirmed_peak(
            &tx,
            ConfirmedPeak {
                hash: Bytes32::default(),
                height: 4,
            }
        ),
        "a closed announcer channel → emit reports failure for a clean consumer exit"
    );
}

// Red-first (Item 2, capabilities/version branching): a peer's unfinished-block announce type is
// chosen by its negotiated protocol version, split at 0.0.35 — old peers get
// v1 (NewUnfinishedBlock), new peers get v2 (NewUnfinishedBlock2).
#[test]
fn unfinished_announce_version_split_matches_chia_0_0_35_boundary() {
    assert!(
        !announce_v2_for(ChiaProtocolVersion::Chia0_0_34),
        "0.0.34 is an old client — v1"
    );
    assert!(
        !announce_v2_for(ChiaProtocolVersion::Chia0_0_35),
        "0.0.35 is the boundary, still old-client (<= 0.0.35) — v1"
    );
    assert!(
        announce_v2_for(ChiaProtocolVersion::Chia0_0_36),
        "0.0.36 is a new client — v2"
    );
    assert!(
        announce_v2_for(ChiaProtocolVersion::Chia0_0_37),
        "0.0.37 is a new client — v2"
    );
}

// A fresh peak book with its published claimed-peak gauge, as boot_with_store wires them.
fn test_book() -> (Arc<AtomicU32>, Arc<PeakBook>) {
    let claimed_peak = Arc::new(AtomicU32::new(0));
    let book = Arc::new(PeakBook::new(claimed_peak.clone()));
    (claimed_peak, book)
}

// Builds a StoreApi over a throwaway SQLite store sharing the given peak book — the peak-claim
// tests below all need the same scaffold. `claim_guard` None = the shared inbound server api
// (claims keyed by the real peer id); Some = one outbound connection (claims keyed by the guard).
async fn peak_test_api(
    claimed_peak: &Arc<AtomicU32>,
    book: &Arc<PeakBook>,
    claim_guard: Option<Arc<ClaimGuard>>,
) -> StoreApi<SqliteStore> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "fn_peakclaim_{}_{nanos}.sqlite",
        std::process::id()
    ));
    let store = open_backend(&Backend::Sqlite(path))
        .await
        .expect("open store");
    StoreApi {
        store,
        mempool: Arc::new(Mutex::new(Mempool::new(&MAINNET))),
        constants: MAINNET,
        claimed_peak: claimed_peak.clone(),
        peak_book: book.clone(),
        claim_guard,
        new_peak_signal: Arc::new(Notify::new()),
        known_peers: Arc::new(RwLock::new(Vec::new())),
        tx_requested: Arc::new(Mutex::new(HashMap::new())),
        slot_state: Arc::new(Mutex::new(SlotState::new(MAINNET))),
        sp_inbox: Arc::new(Mutex::new(Vec::new())),
        unfinished: Arc::new(Mutex::new(UnfinishedCache::new())),
        ub_inbox: Arc::new(Mutex::new(Vec::new())),
        ip_inbox: Arc::new(Mutex::new(Vec::new())),
        synced: Arc::new(AtomicBool::new(true)),
        wallet_compat: Arc::new(AtomicBool::new(false)),
        tx_inbox: Arc::new(Mutex::new(TxQueue::new(
            TX_INBOX_CAP,
            TX_INBOX_PER_PEER,
            MAINNET.max_block_cost_clvm / 2,
        ))),
        tx_announce: Arc::new(Mutex::new(Vec::new())),
        tx_origin: Arc::new(Mutex::new(HashMap::new())),
        wp_inbox: Arc::new(Mutex::new(Vec::new())),
        compact_vdf_inbox: Arc::new(Mutex::new(Vec::new())),
        proof_candidates: Arc::new(Mutex::new(ProofCandidateStore::default())),
        candidates: Arc::new(Mutex::new(CandidateBlockStore::default())),
        producer: Arc::new(ProducerMetrics::default()),
        farmed_headers: Arc::new(Mutex::new(VecDeque::new())),
        wallet: Arc::new(WalletNotifier::new()),
        trust: Arc::new(TrustPolicy::default()),
        wallet_sync_sem: Arc::new(LimitedSemaphore::new(
            WALLET_SYNC_ACTIVE_LIMIT,
            WALLET_SYNC_WAITING_LIMIT,
        )),
        record_window: Arc::new(Mutex::new(BlockRecordCache::new(64))),
        sync_metrics: Arc::new(SyncMetrics::default()),
    }
}

// An outbound peer's NewPeak records its per-connection claim (hash, height, WEIGHT —
// sync_store.peer_has_block), and the connection's NEWEST announcement replaces it (the
// withdrawal path). Another connection's lighter claim never lowers the published heaviest.
#[tokio::test]
async fn outbound_new_peak_records_claimed_peak_and_tip() {
    let (claimed_peak, book) = test_book();
    let api = peak_test_api(&claimed_peak, &book, Some(Arc::new(book.outbound_guard()))).await;

    let tip_hash = Bytes32::const_new([7u8; 32]);
    let peak = NewPeak {
        header_hash: tip_hash,
        height: 9_054_698,
        weight: 9_000,
        fork_point_with_previous_peak: 0,
        unfinished_reward_block_hash: Bytes32::default(),
    };
    api.on_new_peak(Bytes32::default(), peak.clone()).await;
    assert_eq!(claimed_peak.load(Ordering::Relaxed), 9_054_698);
    assert_eq!(
        book.heaviest(),
        Some(PeakClaim {
            header_hash: tip_hash,
            height: 9_054_698,
            weight: 9_000,
        })
    );

    // A LIGHTER claim from a DIFFERENT connection does not lower the published heaviest.
    let other = peak_test_api(&claimed_peak, &book, Some(Arc::new(book.outbound_guard()))).await;
    let lower = NewPeak {
        header_hash: Bytes32::const_new([8u8; 32]),
        height: 5,
        weight: 10,
        ..peak.clone()
    };
    other.on_new_peak(Bytes32::default(), lower.clone()).await;
    assert_eq!(claimed_peak.load(Ordering::Relaxed), 9_054_698);

    // The SAME connection re-announcing lower REPLACES its claim
    // — the withdrawal path the old fetch_max slot lacked.
    api.on_new_peak(Bytes32::default(), lower).await;
    assert_eq!(claimed_peak.load(Ordering::Relaxed), 5);
}

fn peak(hash: [u8; 32], height: u32, weight: u128) -> NewPeak {
    NewPeak {
        header_hash: Bytes32::const_new(hash),
        height,
        weight,
        fork_point_with_previous_peak: 0,
        unfinished_reward_block_hash: Bytes32::default(),
    }
}

// WEIGHT is the
// fork-choice ordering key, not height. A heavier-but-shorter peak must be the sync/weight-proof
// target over a longer-but-lighter fork, regardless of announcement order.
#[tokio::test]
async fn peak_selection_prefers_weight_over_height() {
    let heavy_short = peak([0xAA; 32], 100, 1_000);
    let light_long = peak([0xBB; 32], 120, 900);
    let expected = PeakClaim {
        header_hash: heavy_short.header_hash,
        height: heavy_short.height,
        weight: heavy_short.weight,
    };

    // Order 1: the heavy peak arrives first, the light-but-longer fork second.
    let (claimed_peak, book) = test_book();
    let api = peak_test_api(&claimed_peak, &book, None).await;
    api.on_new_peak(Bytes32::const_new([1; 32]), heavy_short.clone())
        .await;
    api.on_new_peak(Bytes32::const_new([2; 32]), light_long.clone())
        .await;
    assert_eq!(
        book.heaviest(),
        Some(expected),
        "heaviest claim is the target even when a longer-but-lighter fork arrives later"
    );
    assert_eq!(claimed_peak.load(Ordering::Relaxed), 100);

    // Order 2: the light-but-longer fork arrives first.
    let (claimed_peak, book) = test_book();
    let api = peak_test_api(&claimed_peak, &book, None).await;
    api.on_new_peak(Bytes32::const_new([2; 32]), light_long.clone())
        .await;
    api.on_new_peak(Bytes32::const_new([1; 32]), heavy_short.clone())
        .await;
    assert_eq!(
        book.heaviest(),
        Some(expected),
        "heaviest claim is the target even when it is shorter than an earlier claim"
    );
    assert_eq!(claimed_peak.load(Ordering::Relaxed), 100);
}

// A peer's
// peak claim dies with its connection. A bogus high announcement from a peer that then disconnects
// must not pin the claimed slot (and with it the FOLLOW band) forever.
#[tokio::test]
async fn withdrawn_claim_retracts_when_the_announcing_connection_drops() {
    let (claimed_peak, book) = test_book();
    // One OUTBOUND connection, as the factory builds it: claim keyed by the minted guard.
    let api = peak_test_api(&claimed_peak, &book, Some(Arc::new(book.outbound_guard()))).await;
    api.on_new_peak(
        Bytes32::const_new([9; 32]),
        peak([0xEE; 32], 9_999_999, u128::MAX),
    )
    .await;
    assert_eq!(claimed_peak.load(Ordering::Relaxed), 9_999_999);
    // The announcing connection goes away — its per-connection handler map (and with it this
    // StoreApi and its ClaimGuard) is dropped. The claim retracts in
    // sync_store.peer_disconnected.
    drop(api);
    assert_eq!(
        claimed_peak.load(Ordering::Relaxed),
        0,
        "a dead peer's phantom peak must not pin the claimed slot"
    );
    assert_eq!(
        book.heaviest(),
        None,
        "a dead peer's phantom tip must not remain the weight-proof target"
    );
}

// Inbound claims (the shared server api keys them by the REAL peer id) retract through the
// driver's per-tick reconcile against the live inbound map — the other half of the
// on_disconnect → sync_store.peer_disconnected.
#[tokio::test]
async fn inbound_claim_retracts_when_the_peer_leaves_the_live_map() {
    let (claimed_peak, book) = test_book();
    let api = peak_test_api(&claimed_peak, &book, None).await;
    let peer_id = Bytes32::const_new([9; 32]);
    api.on_new_peak(peer_id, peak([0xEE; 32], 9_999_999, u128::MAX))
        .await;
    assert_eq!(claimed_peak.load(Ordering::Relaxed), 9_999_999);
    // The driver's sweep with the peer absent from the live inbound map retracts its claim.
    book.reconcile(&std::collections::HashSet::new());
    assert_eq!(claimed_peak.load(Ordering::Relaxed), 0);
    assert_eq!(book.heaviest(), None);
}

// The weight-proof ↔ claim cross-check inputs ("Weight proof
// had the wrong height/weight"): validated_proof compares the proof's LAST recent-chain block
// (height, weight) against the announced claim. Against the real mainnet proof fixture, the
// attested pair is the fixture tip with a real (nonzero) weight — so an announcement whose
// height or weight differs (a phantom peak with an inflated weight) cannot pass the comparison.
#[test]
fn weight_proof_recent_chain_attests_the_claimed_tip() {
    let bytes =
        include_bytes!("../../../../weight-proof/tests/fixtures/weight_proof_mainnet_9054698.bin");
    let wp = dg_xch_core::blockchain::weight_proof::WeightProof::from_bytes(
        &mut std::io::Cursor::new(&bytes[..]),
        ChiaProtocolVersion::default(),
    )
    .expect("real mainnet weight proof deserializes");
    let last = wp.recent_chain_data.last().expect("recent chain non-empty");
    assert_eq!(
        last.height(),
        9_054_698,
        "the proof attests the fixture tip height"
    );
    assert!(
        last.weight() > 0,
        "the proof carries the tip's real cumulative weight for the claim comparison"
    );
}

#[tokio::test]
async fn sync_target_weight_gates_against_the_local_peak() {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let db = std::env::temp_dir().join(format!(
        "fn_synctarget_{}_{nanos}.sqlite",
        std::process::id()
    ));
    let config = Config {
        p2p: P2pSettings::default(),
        listen: "127.0.0.1:0".parse().unwrap(),
        rpc: "127.0.0.1:0".parse().unwrap(),
        introducer: None,
        manual_peers: Vec::new(),
        advertise: None,
        backend: Backend::Sqlite(db),
        network_id: "mainnet".to_string(),
        capture_dir: None,
        genesis_sync: false,
        sync_from: 0,
        uncompact: false,
        prefetch_memory_mb: None,
        prefetch_max_inflight: None,
        trusted_peers: Vec::new(),
        trusted_cidrs: Vec::new(),
        rpc_tls: crate::config::RpcTlsMode::Local,
        debug_endpoints: false,
    };
    let node = FullNode::boot(config).await.expect("boot");

    // No local peak, no claims: no target.
    assert_eq!(node.sync_target().await, None);

    let records: Vec<dg_xch_core::blockchain::block_record::BlockRecord> =
        serde_json::from_str(include_str!("../../fixtures/block_records.json"))
            .expect("records fixture");
    let rec = records
        .iter()
        .find(|r| r.height == 5_000_000)
        .expect("peak record present")
        .clone();
    node.store
        .add_block_records(std::slice::from_ref(&rec))
        .await
        .expect("records");
    node.store.set_peak(&rec.header_hash).await.expect("peak");
    assert_eq!(node.local_peak_weight().await, Some(rec.weight));

    // A longer-but-LIGHTER fork claim: taller than local, lighter than local — refused.
    let peer = Bytes32::const_new([1; 32]);
    node.peak_book.record(
        peer,
        true,
        PeakClaim {
            header_hash: Bytes32::const_new([0xBB; 32]),
            height: rec.height + 500,
            weight: rec.weight - 1,
        },
    );
    assert_eq!(
        node.sync_target().await,
        None,
        "a longer-but-lighter fork must not become the sync target"
    );

    // A strictly heavier claim IS the target.
    let heavy = PeakClaim {
        header_hash: Bytes32::const_new([0xCC; 32]),
        height: rec.height + 1,
        weight: rec.weight + 100,
    };
    node.peak_book
        .record(Bytes32::const_new([2; 32]), true, heavy);
    assert_eq!(node.sync_target().await, Some(heavy));

    // Quarantined (its weight proof failed): never re-selected, even though still claimed.
    node.peak_book.quarantine(heavy.header_hash, heavy.height);
    assert_eq!(
        node.sync_target().await,
        None,
        "a quarantined peak must not be re-selected while quarantined"
    );
}

// Red-first (beyond-tip reservation wedge, idxphase pg leg): the FOLLOW producer must clamp its
// fetch frontier to the SERVABLE outbound tip, not to the weight-heaviest claim. An inbound peer
// over-announcing past the real tip becomes the weight-heaviest target, but no peer we fetch from
// serves that range — driving the producer to it is a beyond-tip rejection spin (`claimed=9208323
// > tip=9208311`) that also emits a false "reservation wedge" WARN. Before the clamp,
// follow_fill_claimed returned the over-claim height; after, it clamps to the outbound tip.
#[tokio::test]
async fn follow_fill_clamps_the_frontier_to_the_servable_outbound_tip() {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let db = std::env::temp_dir().join(format!(
        "fn_clampfrontier_{}_{nanos}.sqlite",
        std::process::id()
    ));
    let config = Config {
        p2p: P2pSettings::default(),
        listen: "127.0.0.1:0".parse().unwrap(),
        rpc: "127.0.0.1:0".parse().unwrap(),
        introducer: None,
        manual_peers: Vec::new(),
        advertise: None,
        backend: Backend::Sqlite(db),
        network_id: "mainnet".to_string(),
        capture_dir: None,
        // genesis-sync so the FOLLOW band owns the fill (no weight-proof long-sync detour).
        genesis_sync: true,
        sync_from: 0,
        uncompact: false,
        prefetch_memory_mb: None,
        prefetch_max_inflight: None,
        trusted_peers: Vec::new(),
        trusted_cidrs: Vec::new(),
        rpc_tls: crate::config::RpcTlsMode::Local,
        debug_endpoints: false,
    };
    let node = Arc::new(FullNode::boot(config).await.expect("boot"));

    // An INBOUND peer over-claims 12 past the real tip with the heaviest weight -> the
    // weight-heaviest sync target, but a peer we never fetch from.
    node.peak_book.record(
        Bytes32::const_new([1; 32]),
        true,
        PeakClaim {
            header_hash: Bytes32::const_new([0xEE; 32]),
            height: 9_208_323,
            weight: u128::MAX,
        },
    );
    // The OUTBOUND peer we actually fetch from tops out at the real tip. Keep the guard alive so
    // the claim is not retracted before the assertion.
    let out_guard = node.peak_book.outbound_guard();
    node.peak_book.record(
        out_guard.key(),
        false,
        PeakClaim {
            header_hash: Bytes32::const_new([0xAA; 32]),
            height: 9_208_311,
            weight: 9_000,
        },
    );

    assert_eq!(
        node.sync_target().await.map(|t| t.height),
        Some(9_208_323),
        "the inbound over-claim is the weight-heaviest target",
    );
    assert_eq!(
        follow_fill_claimed(&node).await,
        Some(9_208_311),
        "the producer clamps the fetch frontier to the servable outbound tip, not the over-claim",
    );
}

// Red-first (`--sync-from` wedge): the anchor stages ancestry but sets no peak — the first
// confirmed body does — so gating the FOLLOW fill on a peak alone deadlocks the pipeline:
// the producer waits on a peak that only its own fill can create, while the driver re-anchors
// every tick. Once the anchor is staged the fill must open with no peak in the store.
#[tokio::test]
async fn follow_fill_opens_the_sync_from_band_once_anchored() {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let db = std::env::temp_dir().join(format!(
        "fn_syncfromband_{}_{nanos}.sqlite",
        std::process::id()
    ));
    let config = Config {
        p2p: P2pSettings::default(),
        listen: "127.0.0.1:0".parse().unwrap(),
        rpc: "127.0.0.1:0".parse().unwrap(),
        introducer: None,
        manual_peers: Vec::new(),
        advertise: None,
        backend: Backend::Sqlite(db),
        network_id: "mainnet".to_string(),
        capture_dir: None,
        genesis_sync: false,
        sync_from: 5_490_000,
        uncompact: false,
        prefetch_memory_mb: None,
        prefetch_max_inflight: None,
        trusted_peers: Vec::new(),
        trusted_cidrs: Vec::new(),
        rpc_tls: crate::config::RpcTlsMode::Local,
        debug_endpoints: false,
    };
    let node = Arc::new(FullNode::boot(config).await.expect("boot"));

    let out_guard = node.peak_book.outbound_guard();
    node.peak_book.record(
        out_guard.key(),
        false,
        PeakClaim {
            header_hash: Bytes32::const_new([0xAA; 32]),
            height: 9_222_868,
            weight: u128::MAX,
        },
    );

    assert_eq!(
        follow_fill_claimed(&node).await,
        None,
        "before the anchor is staged the driver's anchor_at owns the band",
    );

    *node.sync_from_anchor.write().await = Some(5_489_936);
    assert_eq!(
        follow_fill_claimed(&node).await,
        Some(9_222_868),
        "once the anchor is staged the fill opens with no peak in the store",
    );
}

// The wedge detector distinguishes the benign at-tip drain from the real pathology: frozen BELOW
// the servable tip (fetchable work nothing is requesting) is a wedge; frozen AT the tip is not.
#[test]
fn frozen_frontier_is_wedge_only_below_the_tip() {
    assert!(
        frozen_frontier_is_wedge(9_169_638, 9_208_311),
        "work below the tip left unrequested is a real wedge",
    );
    assert!(
        !frozen_frontier_is_wedge(9_208_311, 9_208_311),
        "frozen AT the servable tip is the benign drain-the-backlog state, not a wedge",
    );
}

// A candidate stored at declare time (placeholder foliage sigs) plus a farmer SignedValues
// reply: the foliage_block_data signature is verified against the plot key, both signatures
// are spliced in, and the finished block is pushed to ub_inbox — the same path a received
// unfinished block takes to the driver's validate+broadcast.
#[tokio::test]
async fn signed_values_splices_farmer_sigs_and_queues_for_broadcast() {
    use blst::min_pk::SecretKey;
    use dg_xch_core::blockchain::class_group_element::ClassgroupElement;
    use dg_xch_core::blockchain::proof_of_space::{ProofBytes, ProofOfSpace};
    use dg_xch_core::blockchain::sized_bytes::{Bytes48, Bytes96};
    use dg_xch_core::blockchain::unsized_bytes::UnsizedBytes;
    use dg_xch_core::blockchain::vdf_info::VdfInfo;
    use dg_xch_core::blockchain::vdf_proof::VdfProof;
    use dg_xch_core::clvm::bls_bindings::sign;
    use dg_xch_core::consensus::producer::{
        FarmerSignatures, create_unfinished_block_with_sigs, g2_infinity,
    };

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path =
        std::env::temp_dir().join(format!("fn_signed_{}_{nanos}.sqlite", std::process::id()));
    let store = open_backend(&Backend::Sqlite(path))
        .await
        .expect("open store");

    // A real plot key so the fbd-signature verify in on_signed_values passes.
    let sk = SecretKey::key_gen_v3(&[0x5Au8; 32], &[]).expect("sk");
    let plot_pk: Bytes48 = sk.sk_to_pk().into();
    let pos = ProofOfSpace {
        version: 0,
        plot_index: 0,
        meta_group: 0,
        strength: 0,
        challenge: Bytes32::from([1u8; 32]),
        pool_public_key: None,
        pool_contract_puzzle_hash: Some(Bytes32::from([2u8; 32])),
        plot_public_key: plot_pk,
        size: 32,
        proof: ProofBytes::from(vec![7u8; 64]),
    };
    let vdf = |c: u8, n: u64| VdfInfo {
        challenge: Bytes32::from([c; 32]),
        number_of_iterations: n,
        output: ClassgroupElement::get_default_element(),
    };
    let proof = |w: u8| VdfProof {
        witness_type: w,
        witness: UnsizedBytes::new(vec![0xAA]),
        normalized_to_identity: true,
    };
    // A transaction-block candidate so both foliage signatures get spliced. Placeholder foliage sigs.
    let placeholder = FarmerSignatures {
        challenge_chain_sp_signature: g2_infinity(),
        reward_chain_sp_signature: g2_infinity(),
        foliage_block_data_signature: g2_infinity(),
        foliage_transaction_block_signature: g2_infinity(),
    };
    let candidate = create_unfinished_block_with_sigs(
        &MAINNET,
        10,
        0,
        pos,
        MAINNET.genesis_challenge,
        Some(vdf(0x10, 1)),
        Some(proof(1)),
        Some(vdf(0x11, 2)),
        Some(proof(2)),
        Vec::new(),
        0,
        true,
        &[],
        None,
        MAINNET.genesis_challenge,
        MAINNET.genesis_challenge,
        dg_xch_core::blockchain::pool_target::PoolTarget {
            puzzle_hash: Bytes32::from([1u8; 32]),
            max_height: 0,
        },
        None,
        Bytes32::from([0xDDu8; 32]),
        1_600_000_000,
        b"server-emit",
        placeholder,
    )
    .expect("candidate builds");

    // The two hashes the farmer signs, and its real signatures over them (SignedValues).
    let fbd_hash = candidate
        .foliage
        .foliage_block_data
        .hash()
        .expect("fbd hash");
    let ftb_hash = candidate
        .foliage
        .foliage_transaction_block_hash
        .expect("tx block");
    let quality_string = Bytes32::from([0x99u8; 32]);
    let signed = SignedValues {
        quality_string,
        foliage_block_data_signature: sign(&sk, fbd_hash.as_ref()).into(),
        foliage_transaction_block_signature: sign(&sk, ftb_hash.as_ref()).into(),
    };

    let ub_inbox = Arc::new(Mutex::new(Vec::new()));
    let candidates = Arc::new(Mutex::new(CandidateBlockStore::default()));
    candidates.lock().await.insert(quality_string, 0, candidate);
    let api = StoreApi {
        store,
        mempool: Arc::new(Mutex::new(Mempool::new(&MAINNET))),
        constants: MAINNET,
        claimed_peak: Arc::new(AtomicU32::new(0)),
        peak_book: Arc::new(PeakBook::new(Arc::new(AtomicU32::new(0)))),
        claim_guard: None,
        new_peak_signal: Arc::new(Notify::new()),
        known_peers: Arc::new(RwLock::new(Vec::new())),
        tx_requested: Arc::new(Mutex::new(HashMap::new())),
        slot_state: Arc::new(Mutex::new(SlotState::new(MAINNET))),
        sp_inbox: Arc::new(Mutex::new(Vec::new())),
        unfinished: Arc::new(Mutex::new(UnfinishedCache::new())),
        ub_inbox: ub_inbox.clone(),
        ip_inbox: Arc::new(Mutex::new(Vec::new())),
        synced: Arc::new(AtomicBool::new(true)),
        wallet_compat: Arc::new(AtomicBool::new(false)),
        tx_inbox: Arc::new(Mutex::new(TxQueue::new(
            TX_INBOX_CAP,
            TX_INBOX_PER_PEER,
            MAINNET.max_block_cost_clvm / 2,
        ))),
        tx_announce: Arc::new(Mutex::new(Vec::new())),
        tx_origin: Arc::new(Mutex::new(HashMap::new())),
        wp_inbox: Arc::new(Mutex::new(Vec::new())),
        compact_vdf_inbox: Arc::new(Mutex::new(Vec::new())),
        proof_candidates: Arc::new(Mutex::new(ProofCandidateStore::default())),
        candidates,
        producer: Arc::new(ProducerMetrics::default()),
        farmed_headers: Arc::new(Mutex::new(VecDeque::new())),
        wallet: Arc::new(WalletNotifier::new()),
        trust: Arc::new(TrustPolicy::default()),
        wallet_sync_sem: Arc::new(LimitedSemaphore::new(
            WALLET_SYNC_ACTIVE_LIMIT,
            WALLET_SYNC_WAITING_LIMIT,
        )),
        record_window: Arc::new(Mutex::new(BlockRecordCache::new(64))),
        sync_metrics: Arc::new(SyncMetrics::default()),
    };

    api.on_signed_values(Bytes32::default(), signed.clone())
        .await;

    // The finished block landed in ub_inbox with the REAL (non-placeholder) farmer signatures.
    let queued = ub_inbox.lock().await;
    assert_eq!(queued.len(), 1, "one finished block queued for broadcast");
    let block = &queued[0];
    assert_eq!(
        block.foliage.foliage_block_data_signature, signed.foliage_block_data_signature,
        "fbd signature spliced"
    );
    assert_eq!(
        block.foliage.foliage_transaction_block_signature,
        Some(signed.foliage_transaction_block_signature),
        "ftb signature spliced for a tx block"
    );
    assert_ne!(
        block.foliage.foliage_block_data_signature,
        g2_infinity(),
        "no longer the placeholder"
    );

    // A bad fbd signature (wrong key) is rejected: nothing new queued.
    let wrong_sk = SecretKey::key_gen_v3(&[0xA5u8; 32], &[]).expect("sk2");
    let bad = SignedValues {
        quality_string,
        foliage_block_data_signature: sign(&wrong_sk, fbd_hash.as_ref()).into(),
        foliage_transaction_block_signature: Bytes96::from([0u8; 96]),
    };
    drop(queued);
    api.on_signed_values(Bytes32::default(), bad).await;
    assert_eq!(
        ub_inbox.lock().await.len(),
        1,
        "wrong-key signature rejected: no additional block queued"
    );
}

#[test]
fn mempool_payload_gate_matches_chia() {
    // All gates pass: a tx-block candidate, no coercion, mempool frame == prev tx block.
    assert!(may_build_transactions(true, false, Some(100), 100));
    // A non-transaction candidate never carries transactions.
    assert!(!may_build_transactions(false, false, Some(100), 100));
    // The empty-block coercion fired (candidate SP at/before the tx-peak window).
    assert!(!may_build_transactions(true, true, Some(100), 100));
    // Mempool frame lags the candidate's prev tx block (mid-reorg / stale revalidation).
    assert!(!may_build_transactions(true, false, Some(99), 100));
    // Pre-genesis: no mempool frame at all — fails closed.
    assert!(!may_build_transactions(true, false, None, 0));
}

// Chain (header_hash = [h;32]): h0 genesis tx + first-in-sub-slot; h1 non-tx; h2 tx; h3 non-tx peak.
#[tokio::test]
async fn candidate_store_walks_match_chia() {
    use dg_xch_core::blockchain::class_group_element::ClassgroupElement;

    fn rec(
        height: u32,
        timestamp: Option<u64>,
        first_slot_cc: Option<Bytes32>,
        total_iters: u128,
        rin: u8,
        fees: Option<u64>,
    ) -> BlockRecord {
        BlockRecord {
            header_hash: Bytes32::from([height as u8; 32]),
            prev_hash: Bytes32::from([height.wrapping_sub(1) as u8; 32]),
            height,
            weight: u128::from(height),
            total_iters,
            signage_point_index: 0,
            challenge_vdf_output: ClassgroupElement::get_default_element(),
            infused_challenge_vdf_output: None,
            reward_infusion_new_challenge: Bytes32::from([rin; 32]),
            challenge_block_info_hash: Bytes32::default(),
            sub_slot_iters: MAINNET.sub_slot_iters_starting,
            pool_puzzle_hash: Bytes32::from([0xB0 + height as u8; 32]),
            farmer_puzzle_hash: Bytes32::from([0xF0 + height as u8; 32]),
            required_iters: 1,
            deficit: 0,
            overflow: false,
            prev_transaction_block_height: 0,
            timestamp,
            prev_transaction_block_hash: None,
            fees,
            reward_claims_incorporated: None,
            finished_challenge_slot_hashes: first_slot_cc.map(|c| vec![c]),
            finished_infused_challenge_slot_hashes: None,
            finished_reward_slot_hashes: None,
            sub_epoch_summary_included: None,
        }
    }

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!("fn_walks_{}_{nanos}.sqlite", std::process::id()));
    let store = open_backend(&Backend::Sqlite(path))
        .await
        .expect("open store");

    let future_ts = now_secs() + 1_000_000;
    let h0 = rec(
        0,
        Some(500),
        Some(Bytes32::from([0xC0; 32])),
        100,
        0xE0,
        Some(9),
    );
    let h1 = rec(1, None, None, 150, 0xE1, None);
    let h2 = rec(2, Some(future_ts), None, 200, 0xE2, Some(50));
    let h3 = rec(3, None, Some(Bytes32::from([0xC3; 32])), 250, 0xE3, None);
    store
        .add_block_records(&[h0.clone(), h1.clone(), h2.clone(), h3.clone()])
        .await
        .expect("seed records");

    // Reward-chain backtrack: an exact reward_infusion match returns that block; a deeper match walks.
    let found = backtrack_prev_block(store.as_ref(), h3.clone(), Bytes32::from([0xE3; 32]))
        .await
        .expect("found")
        .expect("prev present");
    assert_eq!(found.height, 3, "first-hop reward_infusion match");
    let deep = backtrack_prev_block(store.as_ref(), h3.clone(), Bytes32::from([0xE1; 32]))
        .await
        .expect("found")
        .expect("prev present");
    assert_eq!(deep.height, 1, "backtrack walks to the matching block");

    // challenge_in_chain: h3 is itself first-in-sub-slot; h2 walks back to h0's finished challenge.
    assert_eq!(
        challenge_in_chain(store.as_ref(), &h3).await,
        Some(Bytes32::from([0xC3; 32]))
    );
    assert_eq!(
        challenge_in_chain(store.as_ref(), &h2).await,
        Some(Bytes32::from([0xC0; 32]))
    );

    // Prev linkage + reward-claim walk: prev tx block h2 (with its fees) then the non-tx h1 (fees 0).
    let linkage = resolve_prev_linkage(store.as_ref(), &MAINNET, &h3, 300)
        .await
        .expect("linkage");
    assert!(linkage.is_transaction_block, "sp total-iters past h2 => tx");
    assert_eq!(linkage.prev_block_hash, h3.header_hash);
    assert_eq!(linkage.prev_transaction_block_hash, h2.header_hash);
    assert_eq!(
        linkage.prev_transaction_block_height, 2,
        "the produce-path mempool gate keys on the prev tx block's height"
    );
    assert_eq!(linkage.reward_claims.len(), 2);
    assert_eq!(linkage.reward_claims[0].height, 2);
    assert_eq!(
        linkage.reward_claims[0].fees, 50,
        "prev tx block keeps its fees"
    );
    assert_eq!(linkage.reward_claims[1].height, 1);
    assert_eq!(
        linkage.reward_claims[1].fees, 0,
        "intermediate non-tx blocks: fees 0"
    );

    // Below the prev tx block's total-iters => not a tx block, no claims.
    let non_tx = resolve_prev_linkage(store.as_ref(), &MAINNET, &h3, 150)
        .await
        .expect("linkage");
    assert!(!non_tx.is_transaction_block);
    assert!(non_tx.reward_claims.is_empty());
    assert_eq!(
        non_tx.prev_transaction_block_height, 2,
        "a non-tx candidate still records the true prev tx block height"
    );

    // Timestamp is bumped strictly past the previous transaction block (h2's future timestamp).
    assert_eq!(
        candidate_timestamp(store.as_ref(), &h3).await,
        future_ts + 1,
        "timestamp > prev transaction block"
    );
}

// Build a genesis-style unfinished block (prev_block_hash == pos sub-slot cc challenge ==
// GENESIS_CHALLENGE, signage-point index 0) via the same producer path the live emit uses. This is the
// block a timelord's index-0 infusion point finishes into the genesis FullBlock.
fn genesis_unfinished_block() -> UnfinishedBlock {
    use dg_xch_core::blockchain::class_group_element::ClassgroupElement;
    use dg_xch_core::blockchain::proof_of_space::{ProofBytes, ProofOfSpace};
    use dg_xch_core::blockchain::sized_bytes::Bytes48;
    use dg_xch_core::blockchain::unsized_bytes::UnsizedBytes;
    use dg_xch_core::blockchain::vdf_info::VdfInfo;
    use dg_xch_core::blockchain::vdf_proof::VdfProof;
    use dg_xch_core::consensus::producer::{
        FarmerSignatures, create_unfinished_block_with_sigs, g2_infinity,
    };
    let pos = ProofOfSpace {
        version: 0,
        plot_index: 0,
        meta_group: 0,
        strength: 0,
        challenge: MAINNET.genesis_challenge,
        pool_public_key: None,
        pool_contract_puzzle_hash: Some(Bytes32::from([2u8; 32])),
        plot_public_key: Bytes48::from([3u8; 48]),
        size: 32,
        proof: ProofBytes::from(vec![7u8; 64]),
    };
    let vdf = |c: u8, n: u64| VdfInfo {
        challenge: Bytes32::from([c; 32]),
        number_of_iterations: n,
        output: ClassgroupElement::get_default_element(),
    };
    let proof = |w: u8| VdfProof {
        witness_type: w,
        witness: UnsizedBytes::new(vec![0xAA]),
        normalized_to_identity: true,
    };
    let placeholder = FarmerSignatures {
        challenge_chain_sp_signature: g2_infinity(),
        reward_chain_sp_signature: g2_infinity(),
        foliage_block_data_signature: g2_infinity(),
        foliage_transaction_block_signature: g2_infinity(),
    };
    create_unfinished_block_with_sigs(
        &MAINNET,
        0,
        0,
        pos,
        MAINNET.genesis_challenge,
        Some(vdf(0x10, 1)),
        Some(proof(1)),
        Some(vdf(0x11, 2)),
        Some(proof(2)),
        Vec::new(),
        0,
        true,
        &[],
        None,
        MAINNET.genesis_challenge,
        MAINNET.genesis_challenge,
        dg_xch_core::blockchain::pool_target::PoolTarget {
            puzzle_hash: MAINNET.genesis_pre_farm_pool_puzzle_hash,
            max_height: 0,
        },
        None,
        Bytes32::from([0xDDu8; 32]),
        1_600_000_000,
        b"infusion-genesis",
        placeholder,
    )
    .expect("genesis unfinished block builds")
}

#[tokio::test]
async fn infusion_return_handlers_queue_only_when_synced() {
    use dg_xch_core::blockchain::challenge_chain_subslot::ChallengeChainSubSlot;
    use dg_xch_core::blockchain::class_group_element::ClassgroupElement;
    use dg_xch_core::blockchain::end_of_subslot_bundle::EndOfSubSlotBundle;
    use dg_xch_core::blockchain::reward_chain_subslot::RewardChainSubSlot;
    use dg_xch_core::blockchain::subslot_proofs::SubSlotProofs;
    use dg_xch_core::blockchain::unsized_bytes::UnsizedBytes;
    use dg_xch_core::blockchain::vdf_info::VdfInfo;
    use dg_xch_core::blockchain::vdf_proof::VdfProof;

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path =
        std::env::temp_dir().join(format!("fn_ipdisp_{}_{nanos}.sqlite", std::process::id()));
    let store = open_backend(&Backend::Sqlite(path)).await.expect("store");

    let ip_inbox = Arc::new(Mutex::new(Vec::new()));
    let sp_inbox = Arc::new(Mutex::new(Vec::new()));
    let synced = Arc::new(AtomicBool::new(true));
    let make_api = |synced: Arc<AtomicBool>| StoreApi {
        store: store.clone(),
        mempool: Arc::new(Mutex::new(Mempool::new(&MAINNET))),
        constants: MAINNET,
        claimed_peak: Arc::new(AtomicU32::new(0)),
        peak_book: Arc::new(PeakBook::new(Arc::new(AtomicU32::new(0)))),
        claim_guard: None,
        new_peak_signal: Arc::new(Notify::new()),
        known_peers: Arc::new(RwLock::new(Vec::new())),
        tx_requested: Arc::new(Mutex::new(HashMap::new())),
        slot_state: Arc::new(Mutex::new(SlotState::new(MAINNET))),
        sp_inbox: sp_inbox.clone(),
        unfinished: Arc::new(Mutex::new(UnfinishedCache::new())),
        ub_inbox: Arc::new(Mutex::new(Vec::new())),
        ip_inbox: ip_inbox.clone(),
        synced,
        wallet_compat: Arc::new(AtomicBool::new(false)),
        tx_inbox: Arc::new(Mutex::new(TxQueue::new(
            TX_INBOX_CAP,
            TX_INBOX_PER_PEER,
            MAINNET.max_block_cost_clvm / 2,
        ))),
        tx_announce: Arc::new(Mutex::new(Vec::new())),
        tx_origin: Arc::new(Mutex::new(HashMap::new())),
        wp_inbox: Arc::new(Mutex::new(Vec::new())),
        compact_vdf_inbox: Arc::new(Mutex::new(Vec::new())),
        proof_candidates: Arc::new(Mutex::new(ProofCandidateStore::default())),
        candidates: Arc::new(Mutex::new(CandidateBlockStore::default())),
        producer: Arc::new(ProducerMetrics::default()),
        farmed_headers: Arc::new(Mutex::new(VecDeque::new())),
        wallet: Arc::new(WalletNotifier::new()),
        trust: Arc::new(TrustPolicy::default()),
        wallet_sync_sem: Arc::new(LimitedSemaphore::new(
            WALLET_SYNC_ACTIVE_LIMIT,
            WALLET_SYNC_WAITING_LIMIT,
        )),
        record_window: Arc::new(Mutex::new(BlockRecordCache::new(64))),
        sync_metrics: Arc::new(SyncMetrics::default()),
    };

    let vdf = |c: u8| VdfInfo {
        challenge: Bytes32::from([c; 32]),
        number_of_iterations: 1,
        output: ClassgroupElement::get_default_element(),
    };
    let proof = VdfProof {
        witness_type: 0,
        witness: UnsizedBytes::default(),
        normalized_to_identity: false,
    };
    let ip = NewInfusionPointVDF {
        unfinished_reward_hash: Bytes32::from([9u8; 32]),
        challenge_chain_ip_vdf: vdf(1),
        challenge_chain_ip_proof: proof.clone(),
        reward_chain_ip_vdf: vdf(2),
        reward_chain_ip_proof: proof.clone(),
        infused_challenge_chain_ip_vdf: None,
        infused_challenge_chain_ip_proof: None,
    };
    let sp = NewSignagePointVDF {
        index_from_challenge: 5,
        challenge_chain_sp_vdf: vdf(3),
        challenge_chain_sp_proof: proof.clone(),
        reward_chain_sp_vdf: vdf(4),
        reward_chain_sp_proof: proof.clone(),
    };
    let eos_bundle = EndOfSubSlotBundle {
        challenge_chain: ChallengeChainSubSlot {
            challenge_chain_end_of_slot_vdf: vdf(5),
            infused_challenge_chain_sub_slot_hash: None,
            subepoch_summary_hash: None,
            new_sub_slot_iters: None,
            new_difficulty: None,
        },
        infused_challenge_chain: None,
        reward_chain: RewardChainSubSlot {
            end_of_slot_vdf: vdf(6),
            challenge_chain_sub_slot_hash: Bytes32::from([7u8; 32]),
            infused_challenge_chain_sub_slot_hash: None,
            deficit: 0,
        },
        proofs: SubSlotProofs {
            challenge_chain_slot_proof: proof.clone(),
            infused_challenge_chain_slot_proof: None,
            reward_chain_slot_proof: proof,
        },
    };
    let eos = NewEndOfSubSlotVDF {
        end_of_sub_slot_bundle: eos_bundle,
    };

    // Synced: all three queue for the driver.
    let api = make_api(synced.clone());
    api.on_new_infusion_point_vdf(Bytes32::default(), ip.clone())
        .await;
    api.on_new_signage_point_vdf(Bytes32::default(), sp.clone())
        .await;
    api.on_new_end_of_sub_slot_vdf(Bytes32::default(), eos.clone())
        .await;
    assert_eq!(
        ip_inbox.lock().await.len(),
        1,
        "infusion point queued when synced"
    );
    assert_eq!(
        sp_inbox.lock().await.len(),
        2,
        "signage-point + end-of-sub-slot VDFs routed to the slot-state inbox when synced"
    );

    // Not synced: all three drop (`if sync_store.get_sync_mode(): return None`).
    ip_inbox.lock().await.clear();
    sp_inbox.lock().await.clear();
    let api = make_api(Arc::new(AtomicBool::new(false)));
    api.on_new_infusion_point_vdf(Bytes32::default(), ip).await;
    api.on_new_signage_point_vdf(Bytes32::default(), sp).await;
    api.on_new_end_of_sub_slot_vdf(Bytes32::default(), eos)
        .await;
    assert!(
        ip_inbox.lock().await.is_empty(),
        "infusion point dropped while syncing"
    );
    assert!(
        sp_inbox.lock().await.is_empty(),
        "SP/EOS dropped while syncing"
    );
}

// The in-process infusion assembly (`new_infusion_point_vdf`, the load-bearing path):
// a cached genesis unfinished block + an index-0 infusion point (all GENESIS_CHALLENGE) is finished by
// `assemble_infusion_block` into exactly the FullBlock `unfinished_block_to_full_block` produces — proving
// the cache lookup, the genesis rc-backtrack (prev_b = None), the empty finished-sub-slot collection, the
// genesis sub-slot-start iters (0), and the assembly all wire together against a populated SlotState.
// The final engine peak-set on a REAL block is proven byte-identically by the core fixture reconstruction
// (unfinished_to_full_block_reconstruct.rs) — a fake-VDF genesis block cannot clear live consensus in-test,
// so this asserts the assembly, and process_ip_inbox is exercised to prove the full drive path runs.
#[tokio::test]
async fn infusion_point_finishes_cached_genesis_unfinished_block() {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let db = std::env::temp_dir().join(format!("fn_ipasm_{}_{nanos}.sqlite", std::process::id()));
    let node = Arc::new(
        FullNode::boot(Config {
            p2p: P2pSettings::default(),
            listen: "127.0.0.1:0".parse().unwrap(),
            rpc: "127.0.0.1:0".parse().unwrap(),
            introducer: None,
            manual_peers: Vec::new(),
            advertise: None,
            backend: Backend::Sqlite(db),
            network_id: "mainnet".to_string(),
            capture_dir: None,
            genesis_sync: false,
            sync_from: 0,
            uncompact: false,
            prefetch_memory_mb: None,
            prefetch_max_inflight: None,
            trusted_peers: Vec::new(),
            trusted_cidrs: Vec::new(),
            rpc_tls: crate::config::RpcTlsMode::Local,
            debug_endpoints: false,
        })
        .await
        .expect("boot node"),
    );

    let ub = genesis_unfinished_block();
    let partial_hash = ub.reward_chain_block.hash().expect("reward hash");
    node.unfinished
        .lock()
        .await
        .add_block(partial_hash, 0, ub.clone(), 1);

    // An index-0 infusion point whose challenges are all GENESIS_CHALLENGE: the rc backtrack is the
    // identity on the fresh (genesis-only) SlotState, so target_rc_hash == GENESIS ⇒ prev_b = None;
    // last_slot_cc_hash == GENESIS == challenge_in_chain ⇒ finished_sub_slots == [].
    let genesis = MAINNET.genesis_challenge;
    let ip_vdf = |n: u64| dg_xch_core::blockchain::vdf_info::VdfInfo {
        challenge: genesis,
        number_of_iterations: n,
        output:
            dg_xch_core::blockchain::class_group_element::ClassgroupElement::get_default_element(),
    };
    let ip_proof = |w: u8| dg_xch_core::blockchain::vdf_proof::VdfProof {
        witness_type: w,
        witness: dg_xch_core::blockchain::unsized_bytes::UnsizedBytes::new(vec![0xBB]),
        normalized_to_identity: true,
    };
    let req = NewInfusionPointVDF {
        unfinished_reward_hash: partial_hash,
        challenge_chain_ip_vdf: ip_vdf(100),
        challenge_chain_ip_proof: ip_proof(1),
        reward_chain_ip_vdf: ip_vdf(200),
        reward_chain_ip_proof: ip_proof(2),
        infused_challenge_chain_ip_vdf: None,
        infused_challenge_chain_ip_proof: None,
    };

    let assembled = assemble_infusion_block(&node, &req)
        .await
        .expect("genesis infusion assembles a FullBlock");

    // It must equal the independent unfinished_block_to_full_block construction (prev None ⇒ genesis
    // tx block, height 0, weight == difficulty_starting, empty finished sub-slots).
    let expected = unfinished_block_to_full_block(
        &ub,
        req.challenge_chain_ip_vdf,
        req.challenge_chain_ip_proof.clone(),
        req.reward_chain_ip_vdf,
        req.reward_chain_ip_proof.clone(),
        None,
        None,
        Vec::new(),
        None,
        true,
        MAINNET.difficulty_starting,
    )
    .expect("expected build");
    assert_eq!(
        assembled, expected,
        "assembled infusion block matches the reference construction"
    );
    assert_eq!(assembled.reward_chain_block.height, 0, "genesis height");
    assert_eq!(
        assembled.reward_chain_block.weight,
        u128::from(MAINNET.difficulty_starting),
        "genesis weight == difficulty_starting"
    );
    assert!(
        assembled.reward_chain_block.is_transaction_block,
        "genesis block is a transaction block"
    );
    // The reward-chain infusion VDFs are the timelord's, spliced into the finished reward block.
    assert_eq!(
        assembled.reward_chain_block.challenge_chain_ip_vdf,
        req.challenge_chain_ip_vdf
    );
    assert_eq!(
        assembled.reward_chain_block.reward_chain_ip_vdf,
        req.reward_chain_ip_vdf
    );
    // The foliage's reward_block_hash was re-derived from the finished reward block.
    assert_eq!(
        assembled.foliage.reward_block_hash,
        assembled
            .reward_chain_block
            .hash()
            .expect("finished reward hash"),
        "foliage commits the finished reward block hash"
    );

    // Drive the full inbox path once: process_ip_inbox drains the queue, assembles, and routes to the
    // engine (a fake-VDF genesis block is rejected by consensus — logged, no panic). The proof here is
    // that the drive path runs to completion and the inbox is drained.
    node.ip_inbox.lock().await.push(req);
    let registry: Arc<dyn OutboundPeers> =
        Arc::new(dg_xch_p2p::PeerRegistry::new(P2pSettings::default()));
    let inbound: PeerMap = Arc::new(RwLock::new(HashMap::new()));
    process_ip_inbox(&node, &registry, &inbound).await;
    assert!(
        node.ip_inbox.lock().await.is_empty(),
        "process_ip_inbox drained the infusion inbox"
    );
}

struct FaultStore {
    inner: Arc<SqliteStore>,
    fail_get_block_record: Arc<AtomicBool>,
    // Edge-controller probes: how many times the server fired the deferred build / the
    // falling-edge shed. Counted at the store seam so the tests observe the spawned tasks'
    // actual store calls, not the latch state.
    build_calls: Arc<std::sync::atomic::AtomicUsize>,
    shed_calls: Arc<std::sync::atomic::AtomicUsize>,
}

impl FaultStore {
    fn new(inner: Arc<SqliteStore>, fail_get_block_record: Arc<AtomicBool>) -> Self {
        Self {
            inner,
            fail_get_block_record,
            build_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            shed_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }
}

#[async_trait]
impl dg_xch_stores::CoinStore for FaultStore {
    async fn get_coin_record(
        &self,
        coin_name: &Bytes32,
    ) -> Result<Option<CoinRecord>, dg_xch_stores::StoreError> {
        self.inner.get_coin_record(coin_name).await
    }
    async fn get_coin_records(
        &self,
        names: &[Bytes32],
    ) -> Result<Vec<CoinRecord>, dg_xch_stores::StoreError> {
        self.inner.get_coin_records(names).await
    }
    #[cfg(any(feature = "coin-index", test))]
    async fn batch_coin_states_by_puzzle_hashes(
        &self,
        puzzle_hashes: &[Bytes32],
        min_height: u32,
        filters: &dg_xch_core::protocols::wallet::CoinStateFilters,
        max_items: usize,
    ) -> Result<
        (Vec<dg_xch_core::protocols::wallet::CoinState>, Option<u32>),
        dg_xch_stores::StoreError,
    > {
        self.inner
            .batch_coin_states_by_puzzle_hashes(puzzle_hashes, min_height, filters, max_items)
            .await
    }
    async fn apply_block(
        &self,
        height: u32,
        timestamp: u64,
        additions: &[CoinRecord],
        removals: &[Bytes32],
    ) -> Result<(), dg_xch_stores::StoreError> {
        self.inner
            .apply_block(height, timestamp, additions, removals)
            .await
    }
    async fn apply_block_in(
        &self,
        batch: &mut dg_xch_stores::BatchHandle,
        height: u32,
        timestamp: u64,
        additions: &[CoinRecord],
        removals: &[Bytes32],
    ) -> Result<(), dg_xch_stores::StoreError> {
        self.inner
            .apply_block_in(batch, height, timestamp, additions, removals)
            .await
    }
    async fn rollback_to(&self, fork_height: u32) -> Result<u64, dg_xch_stores::StoreError> {
        self.inner.rollback_to(fork_height).await
    }
    async fn rollback_to_in(
        &self,
        batch: &mut dg_xch_stores::BatchHandle,
        fork_height: u32,
    ) -> Result<u64, dg_xch_stores::StoreError> {
        self.inner.rollback_to_in(batch, fork_height).await
    }
    #[cfg(any(feature = "coin-index", test))]
    async fn get_unspent_by_puzzle_hash(
        &self,
        ph: &Bytes32,
    ) -> Result<Vec<CoinRecord>, dg_xch_stores::StoreError> {
        self.inner.get_unspent_by_puzzle_hash(ph).await
    }
    #[cfg(any(feature = "coin-index", test))]
    async fn get_coins_by_parent(
        &self,
        parent: &Bytes32,
    ) -> Result<Vec<CoinRecord>, dg_xch_stores::StoreError> {
        self.inner.get_coins_by_parent(parent).await
    }
    #[cfg(any(feature = "coin-index", test))]
    async fn get_coins_added_at_height(
        &self,
        height: u32,
    ) -> Result<Vec<CoinRecord>, dg_xch_stores::StoreError> {
        self.inner.get_coins_added_at_height(height).await
    }
    #[cfg(any(feature = "coin-index", test))]
    async fn get_coins_removed_at_height(
        &self,
        height: u32,
    ) -> Result<Vec<CoinRecord>, dg_xch_stores::StoreError> {
        self.inner.get_coins_removed_at_height(height).await
    }
    async fn apply_hints_in(
        &self,
        batch: &mut dg_xch_stores::BatchHandle,
        pairs: &[(Bytes32, Bytes32)],
    ) -> Result<(), dg_xch_stores::StoreError> {
        self.inner.apply_hints_in(batch, pairs).await
    }
    async fn apply_hints(
        &self,
        pairs: &[(Bytes32, Bytes32)],
    ) -> Result<(), dg_xch_stores::StoreError> {
        self.inner.apply_hints(pairs).await
    }
    #[cfg(feature = "hint")]
    async fn get_coins_for_hint(
        &self,
        hint: &Bytes32,
        max_items: usize,
    ) -> Result<Vec<Bytes32>, dg_xch_stores::StoreError> {
        self.inner.get_coins_for_hint(hint, max_items).await
    }
    #[cfg(any(feature = "coin-index", test))]
    async fn get_coin_states_by_puzzle_hashes(
        &self,
        puzzle_hashes: &[Bytes32],
        min_height: u32,
        include_spent: bool,
        max_items: usize,
    ) -> Result<Vec<dg_xch_core::protocols::wallet::CoinState>, dg_xch_stores::StoreError> {
        self.inner
            .get_coin_states_by_puzzle_hashes(puzzle_hashes, min_height, include_spent, max_items)
            .await
    }
}

#[async_trait]
impl dg_xch_stores::BlockStore for FaultStore {
    async fn get_block_record(
        &self,
        hh: &Bytes32,
    ) -> Result<Option<BlockRecord>, dg_xch_stores::StoreError> {
        if self.fail_get_block_record.load(Ordering::Relaxed) {
            return Err(dg_xch_stores::StoreError::Corrupt(
                "injected get_block_record fault".to_string(),
            ));
        }
        self.inner.get_block_record(hh).await
    }
    async fn get_block_record_by_height(
        &self,
        h: u32,
    ) -> Result<Option<BlockRecord>, dg_xch_stores::StoreError> {
        self.inner.get_block_record_by_height(h).await
    }
    async fn get_peak(&self) -> Result<Option<(Bytes32, u32)>, dg_xch_stores::StoreError> {
        self.inner.get_peak().await
    }
    async fn min_record_height(&self) -> Result<Option<u32>, dg_xch_stores::StoreError> {
        self.inner.min_record_height().await
    }
    async fn get_block(
        &self,
        hh: &Bytes32,
    ) -> Result<Option<dg_xch_core::blockchain::full_block::FullBlock>, dg_xch_stores::StoreError>
    {
        self.inner.get_block(hh).await
    }
    async fn add_block_records(
        &self,
        records: &[BlockRecord],
    ) -> Result<(), dg_xch_stores::StoreError> {
        self.inner.add_block_records(records).await
    }
    async fn add_block_records_in(
        &self,
        batch: &mut dg_xch_stores::BatchHandle,
        records: &[BlockRecord],
    ) -> Result<(), dg_xch_stores::StoreError> {
        self.inner.add_block_records_in(batch, records).await
    }
    async fn begin(&self) -> Result<dg_xch_stores::BatchHandle, dg_xch_stores::StoreError> {
        self.inner.begin().await
    }
    async fn append_many(
        &self,
        batch: &mut dg_xch_stores::BatchHandle,
        blocks: &[dg_xch_core::blockchain::full_block::FullBlock],
    ) -> Result<(), dg_xch_stores::StoreError> {
        self.inner.append_many(batch, blocks).await
    }
    async fn commit(
        &self,
        batch: dg_xch_stores::BatchHandle,
    ) -> Result<(), dg_xch_stores::StoreError> {
        self.inner.commit(batch).await
    }
    fn near_tip(&self) -> bool {
        self.inner.near_tip()
    }
    fn set_near_tip(&self, near_tip: bool) {
        self.inner.set_near_tip(near_tip);
    }
    async fn get_unassociated(&self, limit: usize) -> Result<Vec<u32>, dg_xch_stores::StoreError> {
        self.inner.get_unassociated(limit).await
    }
    async fn set_peak(&self, new_peak: &Bytes32) -> Result<u64, dg_xch_stores::StoreError> {
        self.inner.set_peak(new_peak).await
    }
    async fn set_peak_in(
        &self,
        batch: &mut dg_xch_stores::BatchHandle,
        new_peak: &Bytes32,
    ) -> Result<u64, dg_xch_stores::StoreError> {
        self.inner.set_peak_in(batch, new_peak).await
    }
    async fn get_status(
        &self,
        hh: &Bytes32,
    ) -> Result<dg_xch_stores::BlockStatus, dg_xch_stores::StoreError> {
        self.inner.get_status(hh).await
    }
    async fn set_status(
        &self,
        hh: &Bytes32,
        s: dg_xch_stores::BlockStatus,
    ) -> Result<(), dg_xch_stores::StoreError> {
        self.inner.set_status(hh, s).await
    }
    async fn set_status_in(
        &self,
        batch: &mut dg_xch_stores::BatchHandle,
        hh: &Bytes32,
        s: dg_xch_stores::BlockStatus,
    ) -> Result<(), dg_xch_stores::StoreError> {
        self.inner.set_status_in(batch, hh, s).await
    }
    async fn savepoint(&self) -> Result<dg_xch_stores::Savepoint, dg_xch_stores::StoreError> {
        self.inner.savepoint().await
    }
    async fn rollback(
        &self,
        sp: dg_xch_stores::Savepoint,
    ) -> Result<u64, dg_xch_stores::StoreError> {
        self.inner.rollback(sp).await
    }
    async fn get_generator_at_height(
        &self,
        h: u32,
    ) -> Result<Option<dg_xch_core::clvm::program::SerializedProgram>, dg_xch_stores::StoreError>
    {
        self.inner.get_generator_at_height(h).await
    }
    async fn get_sub_epoch_segments(
        &self,
        ses_hash: &Bytes32,
    ) -> Result<Option<Vec<u8>>, dg_xch_stores::StoreError> {
        self.inner.get_sub_epoch_segments(ses_hash).await
    }
    async fn persist_sub_epoch_segments(
        &self,
        ses_hash: &Bytes32,
        bytes: &[u8],
    ) -> Result<(), dg_xch_stores::StoreError> {
        self.inner.persist_sub_epoch_segments(ses_hash, bytes).await
    }
    async fn build_indexes(&self) -> Result<(), dg_xch_stores::StoreError> {
        self.build_calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.inner.build_indexes().await
    }
    async fn shed_service_indexes(&self) -> Result<(), dg_xch_stores::StoreError> {
        self.shed_calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.inner.shed_service_indexes().await
    }
}

// A STORE ERROR resolving an unfinished block's parent must NOT be misclassified as "we are
// behind" and lost. Treating any `Err` like a missing parent drops the candidate and
// `remove_requesting`s it, which loses our OWN winning candidate on a DB hiccup. The
// candidate is re-queued instead and never counted as `ub_prev_unknown`.
#[tokio::test]
async fn ub_store_error_requeues_candidate_never_counts_it_as_prev_unknown() {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let db = std::env::temp_dir().join(format!("fn_ubfault_{}_{nanos}.sqlite", std::process::id()));
    let inner = open_backend(&Backend::Sqlite(db)).await.expect("store");
    let fail = Arc::new(AtomicBool::new(false));
    let store = Arc::new(FaultStore::new(inner, fail.clone()));
    let node = Arc::new(
        FullNode::boot_with_store(
            Config {
                p2p: P2pSettings::default(),
                listen: "127.0.0.1:0".parse().unwrap(),
                rpc: "127.0.0.1:0".parse().unwrap(),
                introducer: None,
                manual_peers: Vec::new(),
                advertise: None,
                backend: Backend::Sqlite(std::path::PathBuf::from("unused")),
                network_id: "mainnet".to_string(),
                capture_dir: None,
                genesis_sync: false,
                sync_from: 0,
                uncompact: false,
                prefetch_memory_mb: None,
                prefetch_max_inflight: None,
                trusted_peers: Vec::new(),
                trusted_cidrs: Vec::new(),
                rpc_tls: crate::config::RpcTlsMode::Local,
                debug_endpoints: false,
            },
            store,
        )
        .expect("boot node with fault store"),
    );

    // A ready unfinished block sits in the inbox; the parent lookup is the first store touch it hits.
    let ub = genesis_unfinished_block();
    node.ub_inbox.lock().await.push(ub);

    // Arm the fault: the parent lookup now errors (transient backend outage), even though on a healthy
    // store this parent would resolve.
    fail.store(true, Ordering::Relaxed);
    process_ub_inbox(&node).await;

    // The candidate was PRESERVED: put back on the inbox for a retry, NOT dropped.
    assert_eq!(
        node.ub_inbox.lock().await.len(),
        1,
        "store error must re-queue the candidate, not lose it"
    );
    // A store error is NEVER the same event as a genuine 'we are behind' miss.
    assert_eq!(
        node.producer.dropped_count("ub_prev_unknown"),
        0,
        "a store error must not be counted as ub_prev_unknown"
    );
    assert_eq!(
        node.producer.requeued_count("ub_prev_store_error"),
        1,
        "the re-queue must be recorded under its own reason"
    );

    // With the backend recovered, the same candidate now resolves its (absent) genesis parent as a
    // genuine miss — the correct 'we are behind' park — proving the retry path is real, not a black hole.
    fail.store(false, Ordering::Relaxed);
    process_ub_inbox(&node).await;
    assert_eq!(
        node.ub_inbox.lock().await.len(),
        0,
        "recovered store drains the re-queued candidate"
    );
    assert_eq!(
        node.producer.dropped_count("ub_prev_unknown"),
        1,
        "genesis parent absent on a healthy store is the genuine ub_prev_unknown park"
    );
}

// The two-edge index-phase controller in update_synced. Rising edge (not-synced -> synced):
// build the deferred secondary indexes once. Falling edge (deep behind: tip_lag past the
// hysteresis band): shed them once, so re-catch-up runs index-lean with HOT spends, and
// re-arm the build latch so the next tip edge rebuilds. A shallow dip (a few blocks, a
// restart wobble) must never churn a multi-GB drop/rebuild — only depth past
// SHED_TIP_LAG_BLOCKS sheds.
#[tokio::test]
async fn deep_fall_behind_sheds_indexes_once_and_the_tip_edge_rebuilds() {
    use dg_xch_core::blockchain::class_group_element::ClassgroupElement;

    // A minimal main-chain record at `height` whose transaction-block timestamp is `ts` —
    // chain_is_current derives synced solely from that timestamp's freshness.
    fn rec_at(height: u32, ts: u64) -> BlockRecord {
        BlockRecord {
            header_hash: Bytes32::from([height as u8; 32]),
            prev_hash: Bytes32::from([height.wrapping_sub(1) as u8; 32]),
            height,
            weight: u128::from(height) * 100,
            total_iters: u128::from(height),
            signage_point_index: 0,
            challenge_vdf_output: ClassgroupElement::get_default_element(),
            infused_challenge_vdf_output: None,
            reward_infusion_new_challenge: Bytes32::default(),
            challenge_block_info_hash: Bytes32::default(),
            sub_slot_iters: MAINNET.sub_slot_iters_starting,
            pool_puzzle_hash: Bytes32::default(),
            farmer_puzzle_hash: Bytes32::default(),
            required_iters: 1,
            deficit: 0,
            overflow: false,
            prev_transaction_block_height: 0,
            timestamp: Some(ts),
            prev_transaction_block_hash: None,
            fees: None,
            reward_claims_incorporated: None,
            finished_challenge_slot_hashes: None,
            finished_infused_challenge_slot_hashes: None,
            finished_reward_slot_hashes: None,
            sub_epoch_summary_included: None,
        }
    }

    async fn confirm_at<S: dg_xch_stores::BlockStore + dg_xch_stores::CoinStore>(
        store: &S,
        height: u32,
        ts: u64,
    ) {
        let rec = rec_at(height, ts);
        store
            .add_block_records(std::slice::from_ref(&rec))
            .await
            .expect("record");
        store.set_peak(&rec.header_hash).await.expect("peak");
    }

    async fn wait_count(counter: &Arc<std::sync::atomic::AtomicUsize>, want: usize) -> bool {
        for _ in 0..200 {
            if counter.load(Ordering::Relaxed) == want {
                return true;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        counter.load(Ordering::Relaxed) == want
    }

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let db =
        std::env::temp_dir().join(format!("fn_idxphase_{}_{nanos}.sqlite", std::process::id()));
    let inner = open_backend(&Backend::Sqlite(db)).await.expect("store");
    let store = Arc::new(FaultStore::new(inner, Arc::new(AtomicBool::new(false))));
    let node = Arc::new(
        FullNode::boot_with_store(
            Config {
                p2p: P2pSettings::default(),
                listen: "127.0.0.1:0".parse().unwrap(),
                rpc: "127.0.0.1:0".parse().unwrap(),
                introducer: None,
                manual_peers: Vec::new(),
                advertise: None,
                backend: Backend::Sqlite(std::path::PathBuf::from("unused")),
                network_id: "mainnet".to_string(),
                capture_dir: None,
                genesis_sync: false,
                sync_from: 0,
                uncompact: false,
                prefetch_memory_mb: None,
                prefetch_max_inflight: None,
                trusted_peers: Vec::new(),
                trusted_cidrs: Vec::new(),
                rpc_tls: crate::config::RpcTlsMode::Local,
                debug_endpoints: false,
            },
            store.clone(),
        )
        .expect("boot node"),
    );
    let now = || {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
    };

    // Rising edge at tip: the deferred build fires exactly once.
    confirm_at(&*store, 100, now()).await;
    node.claimed_peak.store(100, Ordering::Relaxed);
    node.update_synced().await;
    assert!(
        wait_count(&store.build_calls, 1).await,
        "the sync->tip rising edge fires the deferred index build"
    );

    // A shallow fall (stale tip, a few blocks behind) must NOT shed — hysteresis.
    confirm_at(&*store, 110, now() - 3_600).await;
    node.claimed_peak.store(120, Ordering::Relaxed);
    for _ in 0..3 {
        node.update_synced().await;
    }
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(
        store.shed_calls.load(Ordering::Relaxed),
        0,
        "a shallow dip below tip must never churn the index set"
    );

    // A DEEP fall (tip_lag past the hysteresis band): shed fires exactly once, off the
    // follow path, no matter how often update_synced re-observes the phase.
    node.claimed_peak.store(110 + 50_001, Ordering::Relaxed);
    node.update_synced().await;
    assert!(
        wait_count(&store.shed_calls, 1).await,
        "the deep falling edge sheds the secondary indexes"
    );
    for _ in 0..3 {
        node.update_synced().await;
    }
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(
        store.shed_calls.load(Ordering::Relaxed),
        1,
        "the shed is one-shot per falling edge"
    );

    // Back at tip: the rising edge re-fires the build (the shed re-armed the build latch).
    confirm_at(&*store, 200, now()).await;
    node.claimed_peak.store(200, Ordering::Relaxed);
    node.update_synced().await;
    assert!(
        wait_count(&store.build_calls, 2).await,
        "the next tip edge rebuilds what the shed dropped"
    );
}

#[test]
fn index_zero_eos_carries_sub_slot_source_data() {
    use dg_xch_core::blockchain::challenge_chain_subslot::ChallengeChainSubSlot;
    use dg_xch_core::blockchain::class_group_element::ClassgroupElement;
    use dg_xch_core::blockchain::reward_chain_subslot::RewardChainSubSlot;
    use dg_xch_core::blockchain::subslot_proofs::SubSlotProofs;
    use dg_xch_core::blockchain::unsized_bytes::UnsizedBytes;
    use dg_xch_core::blockchain::vdf_info::VdfInfo;
    use dg_xch_core::blockchain::vdf_proof::VdfProof;

    let vdf = VdfInfo {
        challenge: Bytes32::from([1u8; 32]),
        number_of_iterations: 1,
        output: ClassgroupElement::get_default_element(),
    };
    let proof = VdfProof {
        witness_type: 0,
        witness: UnsizedBytes::default(),
        normalized_to_identity: false,
    };
    let eos = EndOfSubSlotBundle {
        challenge_chain: ChallengeChainSubSlot {
            challenge_chain_end_of_slot_vdf: vdf,
            infused_challenge_chain_sub_slot_hash: None,
            subepoch_summary_hash: None,
            new_sub_slot_iters: None,
            new_difficulty: None,
        },
        infused_challenge_chain: None,
        reward_chain: RewardChainSubSlot {
            end_of_slot_vdf: vdf,
            challenge_chain_sub_slot_hash: Bytes32::from([2u8; 32]),
            infused_challenge_chain_sub_slot_hash: None,
            deficit: 0,
        },
        proofs: SubSlotProofs {
            challenge_chain_slot_proof: proof.clone(),
            infused_challenge_chain_slot_proof: None,
            reward_chain_slot_proof: proof,
        },
    };
    let sp = farmer_announce_for_eos(&eos, 100, 200, 9, 8).expect("eos hashes");
    assert_eq!(sp.signage_point_index, 0);
    let src = sp
        .sp_source_data
        .as_ref()
        .expect("sp_source_data populated at index 0");
    assert!(
        src.sub_slot_data.is_some(),
        "index 0 must carry sub_slot_data"
    );
    assert!(src.vdf_data.is_none(), "index 0 must NOT carry vdf_data");
    let ss = src.sub_slot_data.as_ref().unwrap();
    assert_eq!(ss.cc_sub_slot, eos.challenge_chain);
    assert_eq!(ss.rc_sub_slot, eos.reward_chain);
    let bytes = sp.to_bytes(ChiaProtocolVersion::Chia0_0_37).unwrap();
    let back = NewSignagePoint::from_bytes(
        &mut std::io::Cursor::new(bytes.as_slice()),
        ChiaProtocolVersion::Chia0_0_37,
    )
    .unwrap();
    assert_eq!(back, sp);
}

#[test]
fn near_tip_band_matches_chia_short_sync_threshold() {
    // No confirmed peak: never the near-tip band (from-zero catch-up is the bulk/batch job).
    assert!(!in_near_tip_band(0, 20, false));
    // At the tip (gap 0): nothing to follow.
    assert!(!in_near_tip_band(100, 100, true));
    assert!(in_near_tip_band(100, 101, true), "1 behind engages");
    assert!(
        in_near_tip_band(100, 120, true),
        "20 behind (the threshold) engages"
    );
    assert!(
        !in_near_tip_band(100, 121, true),
        "21 behind is the batch band"
    );
    assert!(
        !in_near_tip_band(100, 9_160_916, true),
        "far behind is bulk/batch, not near-tip"
    );
    assert_eq!(
        SHORT_SYNC_BLOCKS_BEHIND_THRESHOLD, 20,
        "short_sync_blocks_behind_threshold"
    );
}

// Emission contract, farmer leg: a normal-index
// accepted SP is announced to farmers as NewSignagePoint carrying sp_source_data.vdf_data (the cc/rc
// SP-VDF outputs), never sub_slot_data, with the SP sub-slot challenge as challenge_hash.
#[test]
fn farmer_sp_announce_emits_vdf_source_data() {
    use dg_xch_core::blockchain::class_group_element::ClassgroupElement;
    use dg_xch_core::blockchain::signage_point::SignagePoint;
    use dg_xch_core::blockchain::vdf_info::VdfInfo;
    let vdf = |c: u8| VdfInfo {
        challenge: Bytes32::from([c; 32]),
        number_of_iterations: 1,
        output: ClassgroupElement::get_default_element(),
    };
    let sp = SignagePoint {
        cc_vdf: Some(vdf(1)),
        cc_proof: None,
        rc_vdf: Some(vdf(2)),
        rc_proof: None,
    };
    let out = farmer_announce_for_sp(&sp, 7, 100, 200, 9, 8).expect("vdfs present");
    assert_eq!(
        out.challenge_hash,
        Bytes32::from([1u8; 32]),
        "cc sub-slot challenge"
    );
    assert_eq!(out.signage_point_index, 7);
    let src = out.sp_source_data.expect("sp_source_data populated");
    assert!(src.vdf_data.is_some(), "normal index carries vdf_data");
    assert!(
        src.sub_slot_data.is_none(),
        "normal index has no sub_slot_data"
    );
}

// Emission contract, full-node leg:
// an accepted SP relays to full nodes as NewSignagePointOrEndOfSubSlot keyed on the SP sub-slot
// challenge + index; an accepted EOS relays at index 0 keyed on the finished sub-slot hash with the
// previous challenge chained.
#[test]
fn full_node_sp_and_eos_announces_carry_chia_fields() {
    use dg_xch_core::blockchain::challenge_chain_subslot::ChallengeChainSubSlot;
    use dg_xch_core::blockchain::class_group_element::ClassgroupElement;
    use dg_xch_core::blockchain::reward_chain_subslot::RewardChainSubSlot;
    use dg_xch_core::blockchain::signage_point::SignagePoint;
    use dg_xch_core::blockchain::subslot_proofs::SubSlotProofs;
    use dg_xch_core::blockchain::unsized_bytes::UnsizedBytes;
    use dg_xch_core::blockchain::vdf_info::VdfInfo;
    use dg_xch_core::blockchain::vdf_proof::VdfProof;
    let vdf = |c: u8| VdfInfo {
        challenge: Bytes32::from([c; 32]),
        number_of_iterations: 1,
        output: ClassgroupElement::get_default_element(),
    };
    let sp = SignagePoint {
        cc_vdf: Some(vdf(3)),
        cc_proof: None,
        rc_vdf: Some(vdf(4)),
        rc_proof: None,
    };
    let state = dg_xch_node::slots::SlotState::new(MAINNET);
    let a = announce_for_sp(&state, 9, &sp).expect("vdfs present");
    assert_eq!(a.challenge_hash, Bytes32::from([3u8; 32]));
    assert_eq!(a.index_from_challenge, 9);
    assert_eq!(a.last_rc_infusion, Bytes32::from([4u8; 32]));

    let proof = VdfProof {
        witness_type: 0,
        witness: UnsizedBytes::default(),
        normalized_to_identity: false,
    };
    let eos = EndOfSubSlotBundle {
        challenge_chain: ChallengeChainSubSlot {
            challenge_chain_end_of_slot_vdf: vdf(5),
            infused_challenge_chain_sub_slot_hash: None,
            subepoch_summary_hash: None,
            new_sub_slot_iters: None,
            new_difficulty: None,
        },
        infused_challenge_chain: None,
        reward_chain: RewardChainSubSlot {
            end_of_slot_vdf: vdf(6),
            challenge_chain_sub_slot_hash: Bytes32::from([7u8; 32]),
            infused_challenge_chain_sub_slot_hash: None,
            deficit: 0,
        },
        proofs: SubSlotProofs {
            challenge_chain_slot_proof: proof.clone(),
            infused_challenge_chain_slot_proof: None,
            reward_chain_slot_proof: proof,
        },
    };
    let e = announce_for_eos(&eos).expect("hashable");
    assert_eq!(
        e.index_from_challenge, 0,
        "an EOS announce is index 0 by protocol convention"
    );
    assert_eq!(
        e.prev_challenge_hash,
        Some(Bytes32::from([5u8; 32])),
        "previous challenge chained from the EOS cc VDF"
    );
    assert_eq!(e.challenge_hash, eos.challenge_chain.hash().unwrap());
    assert_eq!(e.last_rc_infusion, Bytes32::from([6u8; 32]));
}

// ---- light-wallet query surface, against the real mainnet block 5,000,000 --------------
// The block is a transaction block with a generator, 275 additions across 50 puzzle hashes, and 301
// removals — so the served puzzle/solution, additions, removals, and header-block paths run on real
// wire data, exactly what a light wallet pulls during trusted sync.
#[cfg(feature = "coin-index")]
#[cfg(feature = "coin-index")]
mod wallet_queries;
