use super::*;

// The confirmed head the queue should rebase to = engine peak + 1, or the follow base when no peak yet.
pub(super) async fn follow_head<S: BlockStore + CoinStore + Send + Sync + 'static>(
    node: &Arc<FullNode<S>>,
) -> u32 {
    match node.store.get_peak().await.ok().flatten() {
        Some((_, h)) => h.saturating_add(1),
        None if node.config.sync_from > 0 => node.config.sync_from.saturating_sub(63),
        None => 0,
    }
}

// One rotation peer as a block-range source for driver-side recovery fetches.
async fn recovery_source(
    registry: &Arc<dyn OutboundPeers>,
    rotation: &mut usize,
) -> Option<Arc<dyn BlockRangeSource>> {
    let peers = registry.live_peers().await;
    if peers.is_empty() {
        return None;
    }
    let idx = *rotation % peers.len();
    *rotation = rotation.wrapping_add(1);
    Some(
        Arc::new(OutboundPeerSource::new(peers[idx].clone(), REQUEST_TIMEOUT))
            as Arc<dyn BlockRangeSource>,
    )
}

async fn fetch_seed_refs<S: BlockStore + CoinStore + Send + Sync + 'static>(
    node: &Arc<FullNode<S>>,
    source: &Arc<dyn BlockRangeSource>,
    heights: &[u32],
) -> Vec<(u32, dg_xch_core::clvm::program::SerializedProgram)> {
    let mut out = Vec::with_capacity(heights.len());
    for &h in heights {
        let cached = {
            let mut cache = node.seed_ref_cache.lock().await;
            let hit = cache.iter().position(|(height, _)| *height == h);
            hit.and_then(|i| cache.remove(i)).map(|entry| {
                let generator = entry.1.clone();
                cache.push_back(entry);
                generator
            })
        };
        let generator = match cached {
            Some(generator) => generator,
            None => {
                let Ok(fetched) = source.fetch_range(h, h).await else {
                    warn!("recovery peer failed to serve ref block height={}", h);
                    continue;
                };
                let Some(generator) = fetched
                    .into_iter()
                    .find(|b| b.height() == h)
                    .and_then(|b| b.transactions_generator)
                else {
                    warn!(
                        "recovery peer served no generator for ref block height={}",
                        h
                    );
                    continue;
                };
                info!(
                    "fetched out-of-span generator ref for the consumer height={}",
                    h
                );
                let mut cache = node.seed_ref_cache.lock().await;
                cache.push_back((h, generator.clone()));
                if cache.len() > SEED_REF_CACHE_CAP {
                    cache.pop_front();
                }
                generator
            }
        };
        out.push((h, generator));
    }
    out
}

// Rebase the queue to the current engine peak — the driver's head-invariant restore after any recovery that changed,
// or left unchanged, the peak. The generation bump wakes the fetch_scheduler to abort in-flight windows
// and replan on the (possibly rewound) branch, so no readahead handle crosses the task boundary.
async fn rebase_to_peak<S: BlockStore + CoinStore + Send + Sync + 'static>(
    node: &Arc<FullNode<S>>,
    queue: &Arc<BlockQueue>,
) {
    let head = follow_head(node).await;
    queue.rebase(head);
}

// Service one consumer recovery request with the driver's peer (the peer-mediated half of recovery). Runs the
// EXISTING sync_backtrack / bulk_sync / resume_repair unchanged; the consumer is parked holding nothing.
pub(super) async fn handle_recovery<S: BlockStore + CoinStore + Send + Sync + 'static>(
    node: &Arc<FullNode<S>>,
    registry: &Arc<dyn OutboundPeers>,
    queue: &Arc<BlockQueue>,
    rotation: &mut usize,
    req: RecoveryRequest,
) {
    match req {
        RecoveryRequest::SeedRefs { heights, reply } => {
            let generators = match recovery_source(registry, rotation).await {
                Some(source) => fetch_seed_refs(node, &source, &heights).await,
                None => Vec::new(),
            };
            let _ = reply.send(generators);
        }
        RecoveryRequest::Orphan { from, to, reply } => {
            warn!(
                "driver servicing orphan backtrack for the consumer from={} to={}",
                from, to
            );
            if let Some(source) = recovery_source(registry, rotation).await {
                match node.sync_backtrack(&source, from, to).await {
                    Ok(Some((_, h))) => info!("backtrack converged past the fork height={}", h),
                    Ok(None) => {}
                    Err(SyncError::DeepFork { base, floor }) => {
                        warn!(
                            "fork deeper than the backtrack cap; driving long sync base={} floor={}",
                            base, floor
                        );
                        match node.bulk_sync(registry).await {
                            Ok(Some((_, h))) => {
                                info!("long sync landed after deep fork height={}", h)
                            }
                            Ok(None) => {}
                            Err(e) => {
                                warn!("deep-fork long sync failed, retry next window error={}", e)
                            }
                        }
                    }
                    Err(e) => {
                        warn!(
                            "orphan backtrack failed, retry next window from={} to={} error={}",
                            from, to, e
                        )
                    }
                }
            }
            rebase_to_peak(node, queue).await;
            let _ = reply.send(());
        }
        RecoveryRequest::MissingRecord { reply } => {
            // Re-arm resume repair until it completes (bounded): floor re-measure + epoch-depth backfill
            // + cache re-warm.
            // `deepen = true`: the consumer PROVED a stage walk missed a record, so a clean floor
            // walk means the miss is below the standard backfill floor — anchor the backfill at
            // the walk's reach point instead of replying "nothing to do" forever.
            for _ in 0..8 {
                match node.resume_repair(registry, true).await {
                    Ok(true) => break,
                    Ok(false) => tokio::time::sleep(DRIVER_TICK).await,
                    Err(e) => {
                        warn!(
                            "resume repair failed during recovery, retry next window error={}",
                            e
                        );
                        break;
                    }
                }
            }
            rebase_to_peak(node, queue).await;
            let _ = reply.send(());
        }
        RecoveryRequest::Reset { reply } => {
            rebase_to_peak(node, queue).await;
            let _ = reply.send(());
        }
    }
}

// Emit a confirmed peak to the announcer WITHOUT ever blocking the confirm consumer. NewPeak is
// best-effort gossip — a newer confirmed peak supersedes an older one — and, the load-bearing reason,
// the peer-free consumer must NEVER park on the announcer: a blocking `peak_tx.send().await` on a full
// buffer back-pressures the consumer off the BlockQueue, which in turn parks the producer on a full
// buffer — a permanent sync stall. `try_send` drops the announcement iff the 256-deep buffer is full,
// and the next confirmed peak carries a fresher tip. Returns false only when the announcer is GONE
// (receiver dropped) so the consumer can exit cleanly.
