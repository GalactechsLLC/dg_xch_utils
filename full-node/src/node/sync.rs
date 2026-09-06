//! Chain synchronization runtime and recovery pipeline.

use super::*;

mod fetch;
mod processing;
mod recovery;

use fetch::{fetch_scheduler, prefetch_config_for};
use processing::{block_processor, peak_announcer};
use recovery::{follow_head, handle_recovery};

#[cfg(test)]
pub(super) use fetch::{follow_fill_claimed, frozen_frontier_is_wedge, prefetch_config};
#[cfg(test)]
pub(super) use processing::{FollowStepTimer, await_reset, emit_confirmed_peak};
pub(crate) async fn reap_wallet_subscriptions_once<
    S: BlockStore + CoinStore + Send + Sync + 'static,
>(
    node: &Arc<FullNode<S>>,
    inbound_peers: &PeerMap,
) {
    let live: std::collections::HashSet<Bytes32> =
        inbound_peers.read().await.keys().copied().collect();
    node.wallet.retain_live(&live).await;
}

pub(crate) async fn tip_follower<S: BlockStore + CoinStore + Send + Sync + 'static>(
    node: Arc<FullNode<S>>,
    registry: Arc<dyn OutboundPeers>,
    inbound_peers: PeerMap,
) {
    let mut rotation = 0usize;
    while node.run.load(Ordering::Relaxed) {
        // Wake on a NewPeak advance; the idle timeout is only a backstop against a lost wakeup.
        tokio::select! {
            () = node.new_peak_signal.notified() => {}
            () = tokio::time::sleep(TIP_FOLLOW_IDLE) => {}
        }
        if !node.run.load(Ordering::Relaxed) {
            break;
        }
        let Some((_, local)) = node.store.get_peak().await.ok().flatten() else {
            // No confirmed peak yet: from-zero catch-up is the driver's bulk/batch job.
            continue;
        };
        // The weight-gated heaviest claim (lighter-than-local announcements are
        // dropped, so a longer-but-lighter fork never becomes the near-tip pull target).
        let Some(target) = node.sync_target().await else {
            continue;
        };
        let claimed = target.height;
        if !in_near_tip_band(local, claimed, true) {
            continue;
        }
        // Entering the near-tip band: per-block commits + the active WAL checkpointer keep the WAL tiny.
        node.store.set_near_tip(true);
        let peers = registry.live_peers().await;
        if peers.is_empty() {
            continue;
        }
        let peer = peers[rotation % peers.len()].clone();
        rotation = rotation.wrapping_add(1);
        let source: Arc<dyn BlockRangeSource> =
            Arc::new(OutboundPeerSource::new(peer, REQUEST_TIMEOUT));
        // `new_peak` ladder: forward-extend [local+1, claimed] first (a direct child of the peak
        // is the common case and needs one forward fetch, no backward peak-refetch), and fall to
        // short_sync_backtrack only on the unknown-parent orphan — so the follower pins tip at lag 0-1.
        match node
            .sync_tip_step(&source, local.saturating_add(1), claimed)
            .await
        {
            Ok(Some((hash, height))) => {
                broadcast_new_peak(&node, &registry, hash, height).await;
                update_slot_state_on_peak(&node, hash).await;
                broadcast_new_peak_timelord(&node, &inbound_peers, hash).await;
            }
            Ok(None) => {}
            // A deeper reorg or a peer that cannot serve the tip: defer to the driver's batch/bulk bands.
            Err(e) => {
                debug!(
                    "tip-follow step deferred to the driver local={} claimed={} error={}",
                    local, claimed, e
                );
            }
        }
    }
}

// Consumer idle backstop: the block processor is event-driven on the queue's `ready` signal; this tick
// only exists so a shutdown (run flag cleared) is observed even when no block is arriving.
const CONSUMER_IDLE: Duration = Duration::from_secs(2);
// Bound on the ConfirmedPeak announcer channel: sized well past the queue window depth so
// the height-monotone SPSC feed never backpressures the consumer under a normal peak cadence.
const PEAK_CHANNEL_CAP: usize = 256;
// Bound on the consumer→driver recovery channel: recovery is rare (reorg / --sync-from ref miss / epoch
// wall), one outstanding request at a time in practice; a small cap is ample and keeps it bounded.
pub(super) const RECOVERY_CHANNEL_CAP: usize = 8;
// Stall-reclaim bound for the DECOUPLED fetch/confirm pipeline (a whole-pipeline liveness backstop). The
// bulk `download_worker` reclaims a per-reservation stall to the pool; the decoupled genesis/follow
// pipeline (WindowReadahead + BlockQueue) has no such reclaim — every individual fetch is timeout-bounded
// but nothing detects the pipeline AS A WHOLE ceasing to advance. This is that whole-pipeline watchdog
// bound: if the confirmed frontier (`queue.low_water`) does not advance for this long WHILE work remains,
// peers are live, and no confirm is legitimately in flight, the driver force-rebases to break the wedge.
// 180s = 2× REQUEST_TIMEOUT (the longest a single window fetch can legitimately stall outside a
// confirm); confirm time is excluded via `follow_inflight_since` so healthy — even slow —
// validation never trips it.
const RECLAIM_TIMEOUT: Duration = Duration::from_secs(180);
// Bound on how long the peer-free consumer parks on a recovery reply before giving up and retrying the
// window, so a stuck driver loop can never hang the confirm consumer forever. Set above the worst
// legitimate `handle_recovery` (MissingRecord's 8xDRIVER_TICK re-arm, a bounded backtrack's fetches)
// so it never abandons an in-progress recovery.
pub(super) const RESET_REPLY_TIMEOUT: Duration = Duration::from_secs(300);

fn unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The consumer→driver recovery channel. The detached, peer-free
/// [`block_processor`] emits one of these when a confirmed window needs a peer at confirm time and parks
/// on the reply **holding no engine/`Chaser` lock**. The thin driver services each by running the
/// EXISTING, unmoved recovery routines with its rotation peer, then
/// rebases the queue to restore the head invariant (`queue.low_water == confirmed_peak + 1`).
/// Recovery orchestration itself never moves.
pub(super) enum RecoveryRequest {
    /// The window `[from, to]` returned the unknown-parent orphan — mainnet reorged at/below our tip. The
    /// driver runs the recovery ladder (`sync_backtrack` → deep-fork `bulk_sync`), then rebases to the
    /// (possibly rewound) peak + 1.
    Orphan {
        from: u32,
        to: u32,
        reply: oneshot::Sender<()>,
    },
    /// `--sync-from` only: the window references out-of-span generator heights the engine lacks. The
    /// driver fetches those generators with its peer and returns them; the consumer seeds them into the
    /// engine overlay and retries the confirm — all while holding no lock.
    SeedRefs {
        heights: Vec<u32>,
        reply: oneshot::Sender<Vec<(u32, dg_xch_core::clvm::program::SerializedProgram)>>,
    },
    /// A stage walk needed a block record below the store floor (the epoch-boundary wall). The driver
    /// re-arms `resume_repair` (header backfill + cache re-warm), then rebases so the window re-stages.
    MissingRecord { reply: oneshot::Sender<()> },
    /// A transient confirm failure (a peer served a bad body, a store hiccup): the window was drained but
    /// not confirmed, so the driver rebases to the unchanged peak + 1 and the producer re-fetches it.
    Reset { reply: oneshot::Sender<()> },
}

/// The consumer→announcer post-confirm signal. SPSC, height-monotone. The three
/// peer-facing broadcasts do NOT run in the peer-free consumer (that would re-couple it to the registry);
/// the announcer runs them one step later, which is behavior-preserving because NewPeak is
/// idempotent/monotone. The non-peer wallet/mempool delta application already ran inside the consumer's
/// `finish_follow_step`, so only the `(hash, height)` the broadcasts need travels the channel.
pub(super) struct ConfirmedPeak {
    pub(super) hash: Bytes32,
    pub(super) height: u32,
}

pub(crate) async fn sync_driver<S: BlockStore + CoinStore + Send + Sync + 'static>(
    node: Arc<FullNode<S>>,
    registry: Arc<dyn OutboundPeers>,
    inbound_peers: PeerMap,
) {
    // Resume repair runs once per process before the first follow step: warm the cold walk cache and
    // backfill epoch-depth records if the store floor is too shallow (the restart walls).
    let mut repaired = false;
    // Rotation cursor for the driver's recovery peer (orphan backtrack / --sync-from ref fetch).
    let mut follow_rotation = 0usize;
    // Sync-decoupling: the three components run as independent tasks communicating only through the
    // bounded reorder queue and the recovery/announce channels. The FETCH producer
    // (fetch_scheduler) fills the queue to its byte budget across peers; the CONFIRM consumer
    // (block_processor) drains it in height order and runs the frozen validation core; this driver is
    // the thin orchestrator that runs gossip + the bulk/anchor/fast-sync bands, services the peer-free
    // consumer's recovery requests, and rebases the queue to keep the head invariant. A slow confirm no longer gates
    // the next fetch and a slow fetch no longer gates the confirm — the whole point of the refactor.
    let queue = Arc::new(BlockQueue::new(
        follow_head(&node).await,
        prefetch_config_for(&node).byte_budget,
        node.sync_metrics.clone(),
    ));
    let (recovery_tx, mut recovery_rx) = mpsc::channel::<RecoveryRequest>(RECOVERY_CHANNEL_CAP);
    let (peak_tx, peak_rx) = mpsc::channel::<ConfirmedPeak>(PEAK_CHANNEL_CAP);
    let mut pipeline_tasks = tokio::task::JoinSet::new();
    pipeline_tasks.spawn(block_processor(
        node.clone(),
        queue.clone(),
        recovery_tx,
        peak_tx,
    ));
    pipeline_tasks.spawn(peak_announcer(
        node.clone(),
        registry.clone(),
        inbound_peers.clone(),
        peak_rx,
    ));
    let mut fetch_task = tokio::spawn(fetch_scheduler(
        node.clone(),
        registry.clone(),
        queue.clone(),
    ));
    // Stall-reclaim watchdog for the decoupled pipeline (see RECLAIM_TIMEOUT). Tracks the confirmed
    // frontier (`queue.low_water`) across driver ticks and force-rebases a stalled pipeline, so any
    // stall — a stuck announcer, a lost wakeup, a silent peer set — is bounded rather than
    // permanent, and is counted in `reservations_reclaimed`.
    let mut stall_watchdog = dg_xch_node::sync::StallWatchdog::new(
        queue.low_water(),
        std::time::Instant::now(),
        RECLAIM_TIMEOUT,
    );
    while node.run.load(Ordering::Relaxed) {
        tokio::time::sleep(DRIVER_TICK).await;
        // A producer panic/early exit must not silently strand an empty queue. Reap it here and
        // construct a fresh producer with fresh peer-source instances; persisted chain state is
        // untouched and the generation guard rejects any completion from the retired epoch.
        if fetch_task.is_finished() {
            match (&mut fetch_task).await {
                Ok(()) => warn!("fetch scheduler exited unexpectedly; restarting"),
                Err(e) => warn!("fetch scheduler failed; restarting error={e}"),
            }
            queue.rebase(follow_head(&node).await);
            fetch_task = tokio::spawn(fetch_scheduler(
                node.clone(),
                registry.clone(),
                queue.clone(),
            ));
        }
        // Service any recovery the peer-free consumer delegated (orphan backtrack / --sync-from ref
        // fetch / missing-record repair / transient reset), running the UNMOVED recovery routines with
        // the driver's peer while the consumer is parked holding nothing. Each handler rebases the
        // queue to the engine peak, restoring the head invariant; the producer picks up the generation bump and replans.
        while let Ok(req) = recovery_rx.try_recv() {
            handle_recovery(&node, &registry, &queue, &mut follow_rotation, req).await;
        }
        // Reconcile the queue head with the engine peak when the consumer is idle: a driver-side path
        // (bulk/anchor/fast-sync/infusion/tip_follower) may have advanced or rewound the peak outside the
        // consumer, so the queue must rebase to `peak + 1`. Skipped while a confirm is in flight
        // (`follow_inflight_since != 0`) — the consumer's `low_water` legitimately leads the not-yet-
        // advanced peak across its drain→confirm window, and a rebase then would drop the live window.
        // The rebase bumps the generation; the fetch_scheduler treats that as its abort-and-replan signal.
        if node.follow_inflight_since.load(Ordering::Relaxed) == 0 {
            let head = follow_head(&node).await;
            if queue.low_water() != head {
                queue.rebase(head);
            }
        }
        // Recompute the synced flag every tick: it
        // opens the tip-context gossip gates only when the confirmed chain is CURRENT (last tx
        // block within 7 minutes), and decays back to false when the tip goes stale.
        node.update_synced().await;
        // Peak-claim retraction sweep: drop the
        // claims of inbound peers that left the live map and any claim past its liveness TTL, so a
        // dead peer's phantom peak un-pins the sync bands within one tick. (Outbound claims retract
        // with their connection's ClaimGuard drop; the TTL is the backstop.)
        {
            let live: std::collections::HashSet<Bytes32> =
                inbound_peers.read().await.keys().copied().collect();
            node.peak_book.reconcile(&live);
        }
        node.sync_metrics.outbound_tip.store(
            u64::from(node.peak_book.outbound_tip().unwrap_or(0)),
            Ordering::Relaxed,
        );
        let target = node.sync_target().await;
        let claimed = target.as_ref().map_or(0, |t| t.height);
        let peak = node.store.get_peak().await.ok().flatten();
        let local = peak.map_or(0, |(_, h)| h);
        // Bulk catch-up (and startup): batch commits + a quiet checkpointer, so the full slow-disk write
        // budget goes to the confirm writer. The tip_follower flips this to near-tip mode inside the band.
        if !in_near_tip_band(local, claimed, peak.is_some()) {
            node.store.set_near_tip(false);
        }
        if !repaired {
            match node.resume_repair(&registry, false).await {
                Ok(done) => repaired = done,
                Err(e) => warn!("resume repair failed, retrying next tick error={}", e),
            }
            if !repaired {
                continue;
            }
        }
        // Stall-reclaim watchdog: the decoupled pipeline has no per-reservation reclaim, so bound ANY
        // whole-pipeline stall here. If the confirmed frontier has not advanced for RECLAIM_TIMEOUT while
        // work remains, peers are live, and no confirm is legitimately in flight, force a rebase (bumps the
        // queue generation → the fetch_scheduler aborts its readahead and replans + is woken off
        // wait_space) and count the reclaim. `claimed` is the heaviest claim (the work frontier). A confirm
        // is "legitimately in flight" only while its marker is fresh; a marker older than the reclaim bound
        // is itself a wedged confirm and must not suppress the reclaim.
        {
            let peers_live = !registry.live_peers().await.is_empty();
            let since = node.follow_inflight_since.load(Ordering::Relaxed);
            let confirm_in_flight =
                since != 0 && unix_secs().saturating_sub(since) < RECLAIM_TIMEOUT.as_secs();
            if stall_watchdog.tick(
                &queue,
                &node.sync_metrics,
                std::time::Instant::now(),
                claimed,
                peers_live,
                confirm_in_flight,
            ) {
                warn!(
                    "decoupled sync pipeline stalled; forced queue rebase (stall reclaim) low_water={} next_fetch={} peak={} generation={} readahead_inflight={} resident_windows={} claimed={} outbound_tip={:?}",
                    queue.low_water(),
                    queue.next_fetch_height(),
                    local,
                    queue.current_gen(),
                    node.sync_metrics.readahead_inflight.load(Ordering::Relaxed),
                    queue.len(),
                    claimed,
                    node.peak_book.outbound_tip()
                );
                // A generation bump can interrupt well-behaved fetch waits, but the watchdog is the
                // last-resort boundary for arbitrary task/lock failure. Recreate the producer so its
                // readahead and cached websocket sources cannot survive the recovery epoch.
                fetch_task.abort();
                let _ = (&mut fetch_task).await;
                fetch_task = tokio::spawn(fetch_scheduler(
                    node.clone(),
                    registry.clone(),
                    queue.clone(),
                ));
                info!("fetch scheduler restarted after stall reclaim");
            }
        }
        // Caught up = no claim strictly heavier than our confirmed peak — a height comparison
        // would chase a longer-but-lighter fork forever.
        if target.is_none() && peak.is_some() {
            continue;
        }
        // Far-behind from a near-empty store: tip-follow (FOLLOW_BATCH/step) can never converge on a ~6.9M
        // tip. Drive the weight-proof bulk sync to the recent chain, then fall through to tip-follow.
        // `--genesis-sync` disables this entirely: the historical chain is validated block by block from 0.
        // `--sync-from H`: with no confirmed peak yet, establish the mid-chain anchor first
        // (candidates for the span below H), then fall through to the follow loop which starts
        // at the span's base instead of 0. Retries every tick until a peer serves the proof+span.
        if node.config.sync_from > 0 && peak.is_none() && !node.sync_from_anchored().await {
            match node.anchor_at(&registry, node.config.sync_from).await {
                Ok(true) => {}
                Ok(false) => continue,
                Err(e) => {
                    warn!("sync-from anchor failed, retrying next tick error={}", e);
                    continue;
                }
            }
        }
        if !node.config.genesis_sync
            && node.config.sync_from == 0
            && wants_long_sync(local, claimed)
        {
            if wants_fast_sync(local, claimed) {
                // Near-empty-store sub-band: the recent-chain jump (unchanged from-zero landing).
                match node.bulk_sync(&registry).await {
                    Ok(Some((_, h))) => {
                        info!("fast-sync landed at recent-chain peak height={}", h);
                        // Band-exit seam: the sync_range confirm path bypassed
                        // the per-block follow side effects, so fire peak-post-processing ONCE now
                        // — mempool revalidation + NewPeak/NewPeakTimelord/NewPeakWallet.
                        node.finish_sync_transition(&registry, &inbound_peers).await;
                    }
                    // No tip/peer yet, or the proof/download failed — retry next tick.
                    Ok(None) => {}
                    Err(e) => warn!("fast-sync failed, retrying next tick error={}", e),
                }
                continue;
            }
            // Mid-chain deep gap: validate the weight proof and resolve the
            // fork point ONCE per landing — including the reorg-across-the-gap reland when the
            // fork point is below our peak. Once anchored, fall through: gossip keeps running
            // while the detached fetch/confirm pipeline batch-syncs the gap from the fork point;
            // the per-tick queue reconcile above rebases onto a relanded peak automatically.
            match node.ensure_long_sync_anchor(&registry).await {
                Ok(true) => {}
                Ok(false) => continue,
                Err(e) => {
                    warn!("long-sync anchor failed, retrying next tick error={}", e);
                    continue;
                }
            }
        }
        // Refresh the RequestPeers gossip answer from the live outbound set: what we can vouch
        // for is exactly who we are connected to right now.
        {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            let snapshot: Vec<TimestampedPeerInfo> = registry
                .live_peers()
                .await
                .iter()
                .map(|p| TimestampedPeerInfo {
                    host: p.endpoint.0.clone(),
                    port: p.endpoint.1,
                    timestamp: now,
                })
                .collect();
            *node.known_peers.write().await = snapshot;
        }
        // Re-gossip freshly-admitted transactions: everything the
        // mempool accepted since last tick — from peer gossip or local push_tx — goes out as
        // NewTransaction to the peers we hold. Expire in-flight fetch guards by AGE:
        // a request older than REQUEST_TIMEOUT with no body is dropped,
        // so the id is re-requestable and cannot pin the pending map. A blanket clear here would
        // wipe a just-issued request and make its legitimate response look unsolicited.
        node.tx_requested
            .lock()
            .await
            .retain(|_, t| t.at.elapsed() < REQUEST_TIMEOUT);
        broadcast_transactions(&node, &registry).await;
        // Validate received slot gossip into the state machine, then relay what was
        // accepted (driver-side so validation always has record ancestry + next-SSI context).
        process_sp_inbox(&node).await;
        broadcast_sp_announcements(&node, &registry).await;
        // Push the farmer-form signage points to inbound farmer peers (the node→farmer
        // half of the farmer interface; the outbound relay above is full-node gossip only).
        broadcast_farmer_signage_points(&node, &inbound_peers).await;
        broadcast_ub_timelord_announcements(&node, &inbound_peers).await;
        // Pre-validate received unfinished blocks and relay the accepted ones.
        process_ub_inbox(&node).await;
        broadcast_ub_announcements(&node, &registry).await;
        process_ip_inbox(&node, &registry, &inbound_peers).await;
        // Compact-VDF consume: validate + swap pulled compact proofs, re-gossip accepted ones.
        process_compact_vdf_inbox(&node).await;
        broadcast_compact_vdf_announcements(&node, &registry).await;
        // The block-follow producer/consumer is fully detached: the fetch_scheduler keeps the queue
        // filled (FOLLOW band) and the block_processor drains + confirms it, both on their own tasks.
        // The near-tip band stays with the event-driven tip_follower and the far-behind band with the
        // bulk/anchor/fast-sync arms above; this loop is now pure orchestration + gossip.
    }
    // Drain-on-shutdown: the run flag is already clear, so the sub-tasks are winding down on their own
    // idle backstops; abort makes the teardown prompt and leaks no task past the driver.
    fetch_task.abort();
    let _ = fetch_task.await;
    pipeline_tasks.abort_all();
    while pipeline_tasks.join_next().await.is_some() {}
}
