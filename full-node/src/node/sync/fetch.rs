use super::*;

// The readahead/queue byte budget + fan-out config: the shipped default, or the aggressive
// large-RAM profile when `--prefetch-memory-mb`/`--prefetch-max-inflight` is set. Shared by the queue
// (its byte ceiling) and the fetch scheduler (its lookahead depth + per-peer fan-out).
pub(super) fn prefetch_config_for<S: BlockStore + CoinStore + Send + Sync + 'static>(
    node: &Arc<FullNode<S>>,
) -> dg_xch_node::sync::PrefetchConfig {
    if node.config.prefetch_memory_mb.is_some() || node.config.prefetch_max_inflight.is_some() {
        let mb = node
            .config
            .prefetch_memory_mb
            .unwrap_or(dg_xch_node::sync::READAHEAD_BYTE_BUDGET / (1024 * 1024));
        dg_xch_node::sync::PrefetchConfig::aggressive(
            mb,
            node.config.prefetch_max_inflight,
            dg_xch_node::sync::TARGET_OUTBOUND,
        )
    } else {
        dg_xch_node::sync::PrefetchConfig::default()
    }
}

pub(in crate::node) async fn follow_fill_claimed<
    S: BlockStore + CoinStore + Send + Sync + 'static,
>(
    node: &Arc<FullNode<S>>,
) -> Option<u32> {
    // The weight-gated heaviest claim (`sync_store.get_heaviest_peak` behind the new_peak weight
    // drop): `None` = caught up or nothing heavier claimed — a longer-but-lighter fork never
    // becomes the FOLLOW fill target.
    let target = node.sync_target().await?;
    let claimed = target.height;
    let peak = node.store.get_peak().await.ok().flatten();
    let local = peak.map_or(0, |(_, h)| h);
    let has_peak = peak.is_some();
    if node.config.sync_from > 0 && !has_peak && !node.sync_from_anchored().await {
        return None; // the driver's anchor_at establishes the mid-chain span first
    }
    if !node.config.genesis_sync && node.config.sync_from == 0 && wants_long_sync(local, claimed) {
        if wants_fast_sync(local, claimed) {
            return None; // the driver's from-zero weight-proof fast-sync owns the band
        }
        // Mid-chain deep gap: fill only once the driver has anchored the landing (weight proof
        // validated + fork point resolved); an unanchored fill would batch-download toward an
        // unproven heavy claim.
        if !node.long_sync_anchored().await {
            return None;
        }
    }
    if in_near_tip_band(local, claimed, has_peak) {
        return None; // the event-driven tip_follower owns the near-tip band
    }
    // Clamp the fetch frontier to what our fetch sources (outbound peers) actually advertise. The
    // weight-heaviest target can ride an inbound/unfetchable claim past the servable tip; requesting
    // past it is a beyond-tip range every peer rejects, spun every tick (the producer's `from >
    // claimed` guard then idles at the tip while the validator drains the resident backlog). No
    // outbound claim yet -> leave it unclamped, as before.
    Some(
        node.peak_book
            .outbound_tip()
            .map_or(claimed, |tip| claimed.min(tip)),
    )
}

// A frozen fetch frontier is a genuine reservation wedge only when fetchable work remains BELOW the
// servable tip (windows nothing is requesting). Frozen AT the tip (`from == claimed`, `claimed`
// already clamped to the servable outbound tip) is the benign drain-the-backlog state, not a wedge.
pub(in crate::node) fn frozen_frontier_is_wedge(from: u32, claimed: u32) -> bool {
    from < claimed
}

/// The detached fetch producer. Owns the readahead engine and the peer
/// sources (rebuilt ONLY when the live set changes),
/// and keeps the [`BlockQueue`] filled to its byte budget across peers, biased to over-fill so the
/// detached consumer is never starved. It touches neither the `Chaser` nor the recovery/announce
/// paths — it only fetches and `complete`s into the queue.
///
/// Reorg coordination is lock-free through the queue generation: a [`BlockQueue::rebase`] (driven by the
/// driver on a reorg or any driver-side peak change) bumps the generation, which is BOTH the stale-
/// completion guard AND this producer's signal to abort its in-flight windows and replan on the new
/// branch — so no separate coordination channel is needed. The dispatch generation is read just before
/// the fetch and carried into `complete`, so a window whose fetch spans a concurrent rebase is dropped by
/// the guard rather than admitted onto a superseded branch.
pub(super) async fn fetch_scheduler<S: BlockStore + CoinStore + Send + Sync + 'static>(
    node: Arc<FullNode<S>>,
    registry: Arc<dyn OutboundPeers>,
    queue: Arc<BlockQueue>,
) {
    let cfg = prefetch_config_for(&node);
    let mut readahead = dg_xch_node::sync::WindowReadahead::with_config(
        node.sync_metrics.clone(),
        REQUEST_TIMEOUT,
        cfg,
    );
    let mut rotation = 0usize;
    let mut last_gen = queue.current_gen();
    let mut peer_sources: Vec<Arc<dyn BlockRangeSource>> = Vec::new();
    let mut source_sig: Vec<(String, u16)> = Vec::new();
    // Sync-stall diagnostics: track the fetch frontier across ticks to catch the decoupled-prefetch wedge
    // (the frontier freezes while the confirm cursor / claimed tip keeps moving).
    let mut last_from = 0u32;
    let mut frozen_from_ticks = 0u32;
    while node.run.load(Ordering::Relaxed) {
        if !queue.can_admit() {
            tokio::select! {
                () = queue.wait_space() => {}
                () = tokio::time::sleep(DRIVER_TICK) => {}
            }
            continue;
        }
        // Only the FOLLOW band is ours; pause (and drop in-flight windows) when another band owns catch-up.
        let Some(claimed) = follow_fill_claimed(&node).await else {
            readahead.abort_all();
            tokio::time::sleep(DRIVER_TICK).await;
            continue;
        };
        // Read the dispatch generation BEFORE the fetch frontier (and carry it to `complete`): any rebase
        // that interleaves makes the completion a harmless guard-drop, never a wrong-branch admit. A gen
        // change since our last dispatch means a rebase superseded our windows — abort and replan.
        let dispatch_gen = queue.current_gen();
        if dispatch_gen != last_gen {
            readahead.abort_all();
            last_gen = dispatch_gen;
        }
        let from = queue.next_fetch_height();
        // Sync-stall diagnostics. A frozen fetch frontier is only a real wedge when fetchable work
        // remains BELOW the servable tip (`from < claimed`): windows exist that nothing is
        // requesting. A frontier frozen AT the tip (`from == claimed`, `claimed` already clamped to
        // the servable outbound tip) is benign — nothing higher is fetchable and the validator is
        // draining the resident backlog — so it must not raise the scary WARN.
        if from == last_from && from <= claimed {
            frozen_from_ticks += 1;
            if frozen_frontier_is_wedge(from, claimed)
                && (frozen_from_ticks == 3 || frozen_from_ticks.is_multiple_of(16))
            {
                warn!(
                    "fetch frontier frozen below the tip while work remains — decoupled prefetch reservation wedge from={} claimed={} frozen_ticks={} low_water={} generation={} resident_windows={} readahead_inflight={}",
                    from,
                    claimed,
                    frozen_from_ticks,
                    queue.low_water(),
                    queue.current_gen(),
                    queue.len(),
                    node.sync_metrics.readahead_inflight.load(Ordering::Relaxed)
                );
            } else if !frozen_frontier_is_wedge(from, claimed) && frozen_from_ticks == 3 {
                debug!(
                    "fetch frontier at the servable tip; validator draining resident backlog from={} claimed={} resident_windows={}",
                    from,
                    claimed,
                    queue.len()
                );
            }
        } else {
            last_from = from;
            frozen_from_ticks = 0;
        }
        if from > claimed {
            tokio::time::sleep(DRIVER_TICK).await;
            continue;
        }
        let to = claimed.min(from.saturating_add(FOLLOW_BATCH - 1));
        // Refresh sources only when the live peer set changed — retires the per-tick rebuild.
        let peers = registry.live_peers().await;
        if peers.is_empty() {
            tokio::time::sleep(DRIVER_TICK).await;
            continue;
        }
        let sig: Vec<(String, u16)> = peers
            .iter()
            .map(|p| (p.endpoint.0.clone(), p.endpoint.1))
            .collect();
        if sig != source_sig {
            peer_sources = peers
                .iter()
                .map(|p| {
                    Arc::new(OutboundPeerSource::new(p.clone(), REQUEST_TIMEOUT))
                        as Arc<dyn BlockRangeSource>
                })
                .collect();
            source_sig = sig;
        }
        let rotation_start = rotation % peer_sources.len();
        rotation = rotation.wrapping_add(1);
        // The direct-fetch fallback peer: rotation-ordered, preferring one WITHOUT an in-flight readahead
        // window (so two ranges never collide on one connection under per_peer==1).
        let source = (0..peer_sources.len())
            .map(|i| &peer_sources[(rotation_start + i) % peer_sources.len()])
            .find(|s| !readahead.busy_peer(s.peer_id()))
            .cloned()
            .unwrap_or_else(|| peer_sources[rotation_start % peer_sources.len()].clone());
        let prefetched = readahead.take(from, to).await;
        if to < claimed {
            readahead.fill(&peer_sources, to.saturating_add(1), claimed, FOLLOW_BATCH);
        }
        let fetched = match prefetched {
            Some(blocks) => Some(blocks),
            None => {
                let started = std::time::Instant::now();
                let direct = source.fetch_range(from, to).await;
                readahead
                    .metrics()
                    .follow_fetch_wait_micros
                    .fetch_add(started.elapsed().as_micros() as u64, Ordering::Relaxed);
                match direct {
                    Ok(mut blocks) => {
                        blocks.sort_by_key(dg_xch_core::blockchain::full_block::FullBlock::height);
                        Some(blocks)
                    }
                    Err(e) => {
                        warn!(
                            "producer fetch failed, retrying next tick from={} to={} error={}",
                            from, to, e
                        );
                        None
                    }
                }
            }
        };
        if let Some(blocks) = fetched
            && !blocks.is_empty()
        {
            // FOLLOW-band download liveness: count delivered bodies into the same counter the bulk
            // download worker feeds at its write-through (sync/mod.rs download_worker). Without
            // this the follow band was a metrics blindspot — `fullnode_blocks_downloaded_total`
            // (and the /health secondary liveness witness watching it) froze while the follow
            // producer was in fact delivering blocks into the queue.
            readahead
                .metrics()
                .blocks_downloaded
                .fetch_add(blocks.len() as u64, Ordering::Relaxed);
            for block in blocks {
                queue.complete(block, dispatch_gen);
            }
        }
    }
    readahead.abort_all();
}
