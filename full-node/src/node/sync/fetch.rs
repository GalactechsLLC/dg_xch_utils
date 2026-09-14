use super::*;

pub(super) fn prefetch_config_for<S: BlockStore + CoinStore + Send + Sync + 'static>(
    node: &Arc<FullNode<S>>,
) -> dg_xch_node::sync::PrefetchConfig {
    prefetch_config(
        node.config.p2p.target_outbound,
        node.config.prefetch_memory_mb,
        node.config.prefetch_max_inflight,
    )
}

pub(in crate::node) fn prefetch_config(
    target_outbound: usize,
    memory_mb: Option<u64>,
    max_inflight: Option<usize>,
) -> dg_xch_node::sync::PrefetchConfig {
    let peers = target_outbound.max(1);
    let mb = memory_mb.unwrap_or(dg_xch_node::sync::READAHEAD_BYTE_BUDGET / (1024 * 1024));
    // A standard V3 connection admits two concurrent RequestBlocks. Use both by default so a slow
    // range on one peer does not leave its other protocol slot (and validation cores) idle.
    let max_inflight = max_inflight.or_else(|| Some(peers.saturating_mul(2)));
    dg_xch_node::sync::PrefetchConfig::aggressive(mb, max_inflight, peers)
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
    // Inbound claims can exceed what the outbound fetch sources can serve.
    Some(
        node.peak_book
            .outbound_tip()
            .map_or(claimed, |tip| claimed.min(tip)),
    )
}

fn fetch_retry_delay(failures: u32) -> Duration {
    Duration::from_millis(250 << failures.saturating_sub(1).min(3))
}

async fn wait_fetch_retry(queue: &BlockQueue, generation: u64, delay: Duration) {
    tokio::select! {
        () = tokio::time::sleep(delay) => {}
        () = queue.wait_replan(generation) => {}
    }
}

async fn take_prefetched(
    readahead: &mut dg_xch_node::sync::WindowReadahead,
    from: u32,
    to: u32,
    claimed: u32,
) -> Option<Vec<FullBlock>> {
    if to == claimed && readahead.inflight() == 0 {
        None
    } else {
        readahead.take(from, to).await
    }
}

/// Fetches into the byte-bounded queue without taking the Chaser lock. Queue generations
/// reject stale completions and cancel in-flight work after a rebase. Peer sources are
/// rebuilt when connection instances change, including reconnections to the same endpoint.
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
    let mut source_peers: Vec<Arc<OutboundPeer>> = Vec::new();
    let mut failures = 0u32;
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
            failures = 0;
        }
        let from = queue.next_fetch_height();
        if from > claimed {
            tokio::time::sleep(DRIVER_TICK).await;
            continue;
        }
        let to = claimed.min(from.saturating_add(FETCH_BATCH - 1));
        // Refresh sources when a connection instance changes, even if it reconnected to the same
        // endpoint. Retaining an OutboundPeerSource by endpoint alone pins the dead websocket forever.
        let peers = registry.live_peers().await;
        if peers.is_empty() {
            tokio::time::sleep(DRIVER_TICK).await;
            continue;
        }
        let same_connections = peers.len() == source_peers.len()
            && peers
                .iter()
                .zip(&source_peers)
                .all(|(current, cached)| Arc::ptr_eq(current, cached));
        if !same_connections {
            readahead.abort_all();
            peer_sources = peers
                .iter()
                .map(|p| {
                    Arc::new(OutboundPeerSource::new(p.clone(), REQUEST_TIMEOUT))
                        as Arc<dyn BlockRangeSource>
                })
                .collect();
            source_peers = peers.clone();
        }
        let prefetched = tokio::select! {
            result = take_prefetched(&mut readahead, from, to, claimed) => result,
            () = queue.wait_replan(dispatch_gen) => {
                readahead.abort_all();
                continue;
            }
        };
        if to < claimed {
            readahead.fill(&peer_sources, to.saturating_add(1), claimed, FETCH_BATCH);
        }
        let fetched = match prefetched {
            Some(blocks) => Some(blocks),
            None => {
                let selected = dg_xch_node::sync::source::select_fetch_source(
                    &peer_sources,
                    rotation,
                    to,
                    |source| Some(usize::from(readahead.busy_peer(source.peer_id()))),
                );
                let Some(index) = selected else {
                    wait_fetch_retry(&queue, dispatch_gen, DRIVER_TICK).await;
                    continue;
                };
                rotation = index.wrapping_add(1);
                let source = &peer_sources[index];
                let started = std::time::Instant::now();
                // Bound the whole operation, including websocket-lock acquisition and send. The
                // guarded request ticket makes cancelling this future release correlation/V3 state.
                let direct = tokio::select! {
                    result = tokio::time::timeout(REQUEST_TIMEOUT, source.fetch_range(from, to)) => {
                        match result {
                            Ok(result) => result,
                            Err(_) => Err(SyncError::PeerStalled(source.peer_id())),
                        }
                    }
                    () = queue.wait_replan(dispatch_gen) => {
                        readahead.abort_all();
                        continue;
                    }
                };
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
                        failures = failures.saturating_add(1);
                        let delay = fetch_retry_delay(failures);
                        let tip_wait =
                            to == claimed && matches!(e, SyncError::RangeRejected { .. });
                        log::log!(
                            if tip_wait {
                                log::Level::Debug
                            } else {
                                log::Level::Warn
                            },
                            "producer fetch deferred peer={}:{} advertised_height={:?} from={} to={} tip_wait={} retry_ms={} queued_blocks={} error={}",
                            source_peers[index].endpoint.0,
                            source_peers[index].endpoint.1,
                            source.advertised_height(),
                            from,
                            to,
                            tip_wait,
                            delay.as_millis(),
                            queue.len(),
                            e
                        );
                        wait_fetch_retry(&queue, dispatch_gen, delay).await;
                        None
                    }
                }
            }
        };
        if let Some(blocks) = fetched
            && !blocks.is_empty()
        {
            failures = 0;
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

#[cfg(test)]
#[path = "../../../tests/unit/node/fetch.rs"]
mod tests;
