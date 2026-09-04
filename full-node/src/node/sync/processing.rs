use super::*;

pub(in crate::node) fn emit_confirmed_peak(
    peak_tx: &mpsc::Sender<ConfirmedPeak>,
    peak: ConfirmedPeak,
) -> bool {
    match peak_tx.try_send(peak) {
        Ok(()) | Err(mpsc::error::TrySendError::Full(_)) => true,
        Err(mpsc::error::TrySendError::Closed(_)) => false,
    }
}

// The peer-facing peak announcer: drains the height-monotone ConfirmedPeak feed
// and runs the three broadcasts the peer-free consumer cannot. Ends when the consumer drops its sender.
pub(super) async fn peak_announcer<S: BlockStore + CoinStore + Send + Sync + 'static>(
    node: Arc<FullNode<S>>,
    registry: Arc<dyn OutboundPeers>,
    inbound_peers: PeerMap,
    mut peak_rx: mpsc::Receiver<ConfirmedPeak>,
) {
    while let Some(ConfirmedPeak { hash, height }) = peak_rx.recv().await {
        broadcast_new_peak(&node, &registry, hash, height).await;
        update_slot_state_on_peak(&node, hash).await;
        broadcast_new_peak_timelord(&node, &inbound_peers, hash).await;
    }
}

/// The detached, peer-free block processor. Drains the landed
/// [`BlockQueue`] in strict height order and runs the FROZEN validation/confirm core
/// (`follow_step_blocks` → `follow_blocks_reporting`), never acquiring a peer, lease, or registry.
/// Everything it needs from the network arrives through the queue or is delegated to the thin driver over
/// the [`RecoveryRequest`] channel; confirmed peaks leave via the [`ConfirmedPeak`] channel to the
/// announcer. No `Chaser` lock is held across a recovery send, by construction: `follow_step_blocks`
/// scopes the `chaser.lock()` inside itself and has fully returned — guard dropped — before this loop
/// inspects the `Err` and sends. The producer never locks the `Chaser` either; it only fetches and
/// pushes to the queue.
pub(super) async fn block_processor<S: BlockStore + CoinStore + Send + Sync + 'static>(
    node: Arc<FullNode<S>>,
    queue: Arc<BlockQueue>,
    recovery_tx: mpsc::Sender<RecoveryRequest>,
    peak_tx: mpsc::Sender<ConfirmedPeak>,
) {
    // Cross-window body pipeline: while window N runs its stage/vdf/sig/confirm phases, window
    // N+1's body precompute (pure CPU over blocks already resident in the queue) runs here in a
    // blocking task. Keyed by the window's first height so a rebase/reorg between spawn and use
    // discards it (worst case: wasted compute, never a stale verdict — the engine's flag-key
    // guard re-verifies every precompute at stage time).
    let (pipe_constants, pipe_assume_valid, pipe_metrics) = {
        let chaser = node.chaser.lock().await;
        (
            chaser.constants(),
            chaser.assume_valid(),
            chaser.metrics().clone(),
        )
    };
    let mut pre_task: Option<(
        u32,
        tokio::task::JoinHandle<
            std::collections::HashMap<u32, dg_xch_node::engine::PrecomputedBody>,
        >,
    )> = None;
    // Stage-ahead pipeline (depth 1): the previous window, staged with its vdf/sig drain running
    // on a blocking thread. Confirmed at the top of the NEXT iteration, after this iteration's
    // stage has overlapped the drain. Depth 1 is enough — the drain dominates the serial residue.
    let mut pipeline: Option<(
        dg_xch_node::sync::StagedWindow,
        tokio::task::JoinHandle<dg_xch_node::sync::WindowVerdict>,
    )> = None;
    'consumer: while node.run.load(Ordering::Relaxed) {
        let step_started = std::time::Instant::now();
        // Park until the head height is present; the idle tick is only a shutdown backstop.
        tokio::select! {
            () = queue.wait_ready() => {}
            () = tokio::time::sleep(CONSUMER_IDLE) => {}
        }
        if !node.run.load(Ordering::Relaxed) {
            break;
        }
        // Mark the whole drain+seed+confirm window in flight: the /health stall dump names a wedged
        // confirm with its age, AND it is the flag the driver's peak-reconcile reads to know a confirm is
        // in progress (so it never rebases mid-window). Set BEFORE the drain so the drain→confirm gap is
        // covered; cleared on every exit path below.
        node.follow_inflight_since
            .store(unix_secs(), Ordering::Relaxed);
        let window = queue.drain_ready_window(FOLLOW_BATCH);
        if window.is_empty() && pipeline.is_none() {
            node.follow_inflight_since.store(0, Ordering::Relaxed);
            continue;
        }
        let _step_timer = FollowStepTimer::new(&pipe_metrics.follow_step_micros, step_started);
        let from = window.first().map_or(0, FullBlock::height);
        let to = window.last().map_or(0, FullBlock::height);
        // --sync-from out-of-span ref pre-seed (peer-free; the driver fetches). The Chaser lock is
        // dropped before the SeedRefs send and re-taken only to apply the returned generators.
        if node.config.sync_from > 0 {
            let missing = {
                let chaser = node.chaser.lock().await;
                chaser.missing_ref_heights(&window).await
            }; // guard dropped here — no lock held across the send below
            if !missing.is_empty() {
                let (tx, rx) = oneshot::channel();
                if recovery_tx
                    .send(RecoveryRequest::SeedRefs {
                        heights: missing,
                        reply: tx,
                    })
                    .await
                    .is_err()
                {
                    break;
                }
                match rx.await {
                    Ok(generators) => {
                        let mut chaser = node.chaser.lock().await;
                        chaser.clear_seed_generators();
                        for (h, g) in generators {
                            chaser.seed_ref_generator(h, g);
                        }
                    }
                    Err(_) => break, // driver gone
                }
            }
        }
        // Join the precompute spawned while the PREVIOUS window validated; a height mismatch
        // (rebase, reorg, partial advance) discards it.
        pipe_metrics
            .window_pre_wait_micros
            .store(0, Ordering::Relaxed);
        let pre = match pre_task.take() {
            Some((h, handle)) if h == from => {
                let join_started = std::time::Instant::now();
                let joined = handle.await.ok();
                pipe_metrics
                    .window_pre_wait_micros
                    .store(join_started.elapsed().as_micros() as u64, Ordering::Relaxed);
                joined
            }
            Some((_, handle)) => {
                handle.abort();
                None
            }
            None => None,
        };
        // Spawn the NEXT window's precompute before validating this one, so the two overlap.
        // Out-of-window compression refs resolve from the confirmed store up front (final in a
        // forward sync); the ones the store cannot serve yet fall to the engine's inline path.
        let next = queue.peek_ready_window(FOLLOW_BATCH);
        if let Some(next_from) = next.first().map(FullBlock::height)
            && next
                .iter()
                .any(|b| b.is_transaction_block() && b.transactions_generator.is_some())
        {
            let extra = {
                let chaser = node.chaser.lock().await;
                chaser.confirmed_ref_generators(&next).await
            };
            let constants = pipe_constants;
            pre_task = Some((
                next_from,
                tokio::task::spawn_blocking(move || {
                    dg_xch_node::sync::precompute_window_bodies_standalone(
                        &NativePrimitives,
                        &constants,
                        pipe_assume_valid,
                        &next,
                        &extra,
                    )
                }),
            ));
        }
        // Confirm results to consume this iteration, each with the bounds of the window it
        // belongs to (the pipeline lags announcement by one window).
        let mut steps: Vec<(u32, u32, StepOutcome)> = Vec::new();
        let near_tip = { node.chaser.lock().await.near_tip() };
        if near_tip || window.is_empty() {
            // Near the tip (or on a drain-only tick) the pipeline empties first: the per-block
            // path must own the writer and the overlay alone.
            if let Some((prev, handle)) = pipeline.take() {
                let (pfrom, pto) = prev.bounds();
                let verdict = join_drain(handle, &pipe_metrics).await;
                steps.push((pfrom, pto, node.confirm_step_window(prev, verdict).await));
            }
            if !window.is_empty() && steps.iter().all(|(_, _, s)| s.is_ok()) {
                steps.push((from, to, node.follow_step_blocks_pre(&window, pre).await));
            }
        } else {
            // Stage THIS window first — it overlaps the previous window's in-flight drain —
            // then confirm the predecessor, then hand this window's drain to a blocking thread.
            let staged = node.stage_step_window(window, pre).await;
            // Spawn THIS window's drain before confirming the predecessor: the drain (pure CPU
            // on already-built queues) then also overlaps that confirm and the next iteration's
            // driver work — otherwise that residue runs with the verification cores idle.
            let mut stage_failed: Option<SyncError> = None;
            let mut spawned: Option<(
                dg_xch_node::sync::StagedWindow,
                tokio::task::JoinHandle<dg_xch_node::sync::WindowVerdict>,
            )> = None;
            match staged {
                Ok(mut staged_window) => {
                    let input = staged_window.take_drain_input();
                    let constants = pipe_constants;
                    spawned = Some((
                        staged_window,
                        tokio::task::spawn_blocking(move || {
                            dg_xch_node::sync::drain_staged_window(
                                &NativePrimitives,
                                &constants,
                                input,
                            )
                        }),
                    ));
                }
                Err(e) => stage_failed = Some(e),
            }
            if let Some((prev, handle)) = pipeline.take() {
                let (pfrom, pto) = prev.bounds();
                let verdict = join_drain(handle, &pipe_metrics).await;
                steps.push((pfrom, pto, node.confirm_step_window(prev, verdict).await));
            }
            if let Some(e) = stage_failed {
                // Stage failure leaves the overlay for us (the predecessor's confirm had to
                // land first); clear it now that it has.
                node.chaser.lock().await.clear_staged_overlay();
                steps.push((from, to, Err(e)));
            }
            if steps.iter().all(|(_, _, s)| s.is_ok()) {
                pipeline = spawned;
            } else if let Some((_, handle)) = spawned.take() {
                // A failed confirm (or stage) retracted the overlay this window staged
                // against: abort its drain and drop it — the queue reset re-fetches both
                // spans. The extra clear is idempotent and covers the confirm-failure path.
                handle.abort();
                node.chaser.lock().await.clear_staged_overlay();
            }
        }
        if pipeline.is_none() {
            node.follow_inflight_since.store(0, Ordering::Relaxed);
        }
        for (sfrom, sto, step) in steps {
            match step {
                Ok(Some((hash, height))) => {
                    // Height-monotone SPSC feed to the announcer. NON-BLOCKING: a stalled announcer must
                    // never stall the confirm consumer, which would back-pressure it off the BlockQueue
                    // and stall the whole pipeline. `emit_confirmed_peak` drops a best-effort
                    // announcement under a full buffer and only reports failure when the announcer is gone.
                    if !emit_confirmed_peak(&peak_tx, ConfirmedPeak { hash, height }) {
                        break 'consumer;
                    }
                    // The window fully advanced the peak iff the confirmed height reached the drained top;
                    // in that case `low_water == height + 1` already holds. A partial advance (a tail
                    // that staged as a side-branch candidate without outweighing) left `low_water` ahead of
                    // the peak, so realign by rebasing to `peak + 1` — the driver drops the drained-but-
                    // unconfirmed tail and the producer re-fetches it.
                    if height < sto
                        && !await_reset(&recovery_tx, |reply| RecoveryRequest::Reset { reply })
                            .await
                    {
                        break 'consumer;
                    }
                }
                // No peak advance: the whole window staged as candidates below the peak (a known-parent side
                // branch). `low_water` advanced on drain but the peak did not, so realign to `peak + 1`. The
                // engine keeps the staged candidates, so weight still accumulates toward an eventual reorg.
                Ok(None) => {
                    if !await_reset(&recovery_tx, |reply| RecoveryRequest::Reset { reply }).await {
                        break 'consumer;
                    }
                }
                Err(e) if e.is_orphan() => {
                    warn!(
                        "consumer window orphaned; delegating backtrack to the driver from={} to={} error={}",
                        sfrom, sto, e
                    );
                    if !await_reset(&recovery_tx, |reply| RecoveryRequest::Orphan {
                        from: sfrom,
                        to: sto,
                        reply,
                    })
                    .await
                    {
                        break 'consumer;
                    }
                }
                Err(e) if e.is_missing_record() => {
                    warn!(
                        "consumer needs records below the floor; delegating repair from={} to={} error={}",
                        sfrom, sto, e
                    );
                    if !await_reset(&recovery_tx, |reply| RecoveryRequest::MissingRecord {
                        reply,
                    })
                    .await
                    {
                        break 'consumer;
                    }
                }
                Err(e) => {
                    warn!(
                        "consumer follow step failed; requesting a queue reset from={} to={} error={}",
                        sfrom, sto, e
                    );
                    if !await_reset(&recovery_tx, |reply| RecoveryRequest::Reset { reply }).await {
                        break 'consumer;
                    }
                }
            }
        }
    }
    if let Some((_, handle)) = pre_task.take() {
        handle.abort();
    }
    // A staged-unconfirmed window at shutdown vanishes wholly (crash class A): abort its drain;
    // resume re-fetches from the durable peak.
    if let Some((_, handle)) = pipeline.take() {
        handle.abort();
    }
}

// One confirmed-or-failed follow step awaiting consumption, keyed by its window bounds.
type StepOutcome = Result<Option<(Bytes32, u32)>, SyncError>;

pub(in crate::node) struct FollowStepTimer<'a> {
    metric: &'a std::sync::atomic::AtomicU64,
    started: std::time::Instant,
}

impl<'a> FollowStepTimer<'a> {
    pub(in crate::node) fn new(
        metric: &'a std::sync::atomic::AtomicU64,
        started: std::time::Instant,
    ) -> Self {
        Self { metric, started }
    }
}

impl Drop for FollowStepTimer<'_> {
    fn drop(&mut self) {
        let micros = u64::try_from(self.started.elapsed().as_micros()).unwrap_or(u64::MAX);
        self.metric.fetch_add(micros, Ordering::Relaxed);
    }
}

// Join a spawned window drain, recording how long the confirm actually waited on it (the
// stage-ahead pipeline's backpressure gauge). A panicked drain fails closed: nothing confirms
// and the window re-stages after the queue reset.
async fn join_drain(
    handle: tokio::task::JoinHandle<dg_xch_node::sync::WindowVerdict>,
    metrics: &std::sync::Arc<dg_xch_node::sync::SyncMetrics>,
) -> dg_xch_node::sync::WindowVerdict {
    let waited = std::time::Instant::now();
    let verdict = handle.await;
    metrics
        .window_drain_wait_micros
        .store(waited.elapsed().as_micros() as u64, Ordering::Relaxed);
    verdict.unwrap_or_else(|_| dg_xch_node::sync::WindowVerdict::failed_closed())
}

// Send a `()`-reply recovery request and park on its completion (called only after the Chaser lock
// is dropped). Returns false if the driver channel is gone (shutdown) so the consumer can exit.
pub(in crate::node) async fn await_reset(
    recovery_tx: &mpsc::Sender<RecoveryRequest>,
    make: impl FnOnce(oneshot::Sender<()>) -> RecoveryRequest,
) -> bool {
    let (tx, rx) = oneshot::channel();
    if recovery_tx.send(make(tx)).await.is_err() {
        return false;
    }
    // Bounded park: a driver loop that never services this recovery must not hang the confirm
    // consumer forever, which would stop the queue draining and park the producer on a full buffer.
    // On a reply-timeout, PROCEED (retry the window next iteration) rather than exit — the driver's
    // per-tick queue reconcile and the stall watchdog restore the head invariant independently, so
    // retrying is safe.
    match tokio::time::timeout(RESET_REPLY_TIMEOUT, rx).await {
        Ok(r) => r.is_ok(),
        Err(_) => {
            warn!("recovery reply timed out; consumer proceeding (retry next window)");
            true
        }
    }
}
