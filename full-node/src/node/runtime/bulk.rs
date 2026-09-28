use super::*;

#[cfg(test)]
#[path = "../../../tests/unit/node/anchor_hardening.rs"]
mod tests;

impl<S> FullNode<S>
where
    S: BlockStore + CoinStore + Send + Sync + 'static,
{
    /// From-zero bulk sync: fetch the weight proof to the highest peer-announced tip, verify it, epoch-anchor
    /// the recent-chain headers, and download + confirm the recent bodies through the reservation window
    /// across all live outbound peers (the W==P contract). Returns the confirmed peak, or `None` if no tip or
    /// no live peer is available yet (retry next tick). After this lands, the cheap tip-follow driver takes
    /// over — this does not backfill deep history.
    ///
    /// # Errors
    /// Returns an I/O error if the weight-proof fetch, its validation, or the body download/confirm fails.
    pub async fn bulk_sync(
        &self,
        registry: &Arc<dyn OutboundPeers>,
    ) -> Result<Option<(Bytes32, u32)>, Error> {
        let peers = registry.live_peers().await;
        let Some(validated) = self.validated_proof(&peers).await? else {
            return Ok(None);
        };
        let sources: Vec<Arc<dyn BlockRangeSource>> = peers
            .iter()
            .map(|p| {
                let base = Arc::new(OutboundPeerSource::new(p.clone(), REQUEST_TIMEOUT))
                    as Arc<dyn BlockRangeSource>;
                // When capturing, wrap each source so every downloaded block range is written to disk for
                // offline replay (capture happens at fetch, before confirm, so it records even ranges that
                // later fail to confirm).
                match &self.config.capture_dir {
                    Some(dir) => Arc::new(CapturingSource::new(base, dir.clone()))
                        as Arc<dyn BlockRangeSource>,
                    None => base,
                }
            })
            .collect();
        let peak = {
            let mut chaser = self.chaser.lock().await;
            let peak = chaser
                .fast_sync_with_summaries(&validated.wp, &validated.summaries, &sources)
                .await
                .map_err(|e| Error::other(e.to_string()))?;
            // Epoch-depth backfill (records only) so the next LIVE epoch-boundary retarget can walk
            // the previous epoch locally, then load the full ancestry into the engine's walk cache.
            // A backfill miss is retryable and must not fail the sync — the boundary is far ahead;
            // the fail-closed walk simply stays light until records exist.
            let anchor = validated
                .wp
                .recent_chain_data
                .first()
                .map(dg_xch_core::blockchain::header_block::HeaderBlock::height)
                .unwrap_or(0);
            match chaser
                .backfill_epoch_depth(&sources, &validated.summaries, anchor)
                .await
            {
                Ok(n) => log::info!("epoch-depth backfill complete records={}", n),
                Err(e) => {
                    log::warn!(
                        "epoch-depth backfill incomplete; will stay light near the boundary error={}",
                        e
                    )
                }
            }
            match chaser.warm_engine_cache().await {
                Ok(n) => {
                    log::info!("engine walk cache warmed from store records={}", n);
                    // mm-OOM visibility: a pod that dies seconds after start still leaves its
                    // post-warm memory shape in the log (the OOMed node left zero allocation evidence).
                    crate::metrics::log_startup_memory("fast_sync", n);
                }
                Err(e) => log::warn!("engine cache warm failed error={}", e),
            }
            peak
        };
        if peak.is_some() {
            self.update_synced().await;
        }
        Ok(peak)
    }

    pub(crate) async fn long_sync_anchored(&self) -> bool {
        self.long_sync_anchor.read().await.is_some()
    }

    pub(crate) async fn sync_from_anchored(&self) -> bool {
        self.sync_from_anchor.read().await.is_some()
    }

    /// Establish — once per landing — the `_sync` trust anchor for a MID-CHAIN deep gap
    ///: a validated weight proof for the heaviest claim (cached
    /// across ticks by [`FullNode::validated_proof`]), the fork point of its summaries against our
    /// chain (`get_fork_point`), the `check_fork_next_block` peer
    /// probe, and — when the fork point is below our
    /// peak (the offline period saw a reorg deeper than our tip) — the reland through the
    /// engine's atomic reorg. Returns `true` once anchored (the detached fetch/confirm pipeline
    /// then batch-syncs the gap), `false` to retry next tick (no claim, peers, proof, or peak
    /// yet).
    ///
    /// # Errors
    /// Returns an I/O error on a failed proof fetch/validation, an unresolvable fork point, or a
    /// reland that could not move the peak.
    pub(in crate::node) async fn ensure_long_sync_anchor(
        &self,
        registry: &Arc<dyn OutboundPeers>,
    ) -> Result<bool, Error> {
        let peers = registry.live_peers().await;
        let Some(validated) = self.validated_proof(&peers).await? else {
            return Ok(false);
        };
        if self.long_sync_anchor.read().await.as_ref() == Some(&validated.tip) {
            return Ok(true);
        }
        let Some((peak_hash, peak_height)) = self
            .store
            .get_peak()
            .await
            .map_err(|e| Error::other(e.to_string()))?
        else {
            // No confirmed peak: the from-zero fast-sync arm owns the band, never this anchor.
            return Ok(false);
        };
        let fork = wp_fork_point(
            self.store.as_ref(),
            &validated.summaries,
            self.constants.sub_epoch_blocks,
        )
        .await
        .map_err(|e| Error::other(e.to_string()))?;
        // check_fork_next_block probes ONLY the no-fork case (fork point == the
        // no-divergence conservative value); a detected divergence keeps its fork point.
        let connects = match &fork {
            WpForkPoint::NoForkDetected { .. } => {
                next_block_connects(&peers, peak_hash, peak_height).await
            }
            _ => false,
        };
        match long_sync_plan(&fork, peak_height, connects) {
            LongSyncPlan::Extend => {}
            LongSyncPlan::Rewind { fork_point } => {
                info!(
                    "long sync: WP fork point below the local peak; relanding through the engine reorg fork_point={} peak_height={}",
                    fork_point, peak_height
                );
                self.long_sync_rewind(&peers, fork_point).await?;
            }
            LongSyncPlan::Stall => {
                return Err(Error::other(format!(
                    "long sync: no WP fork point within the walk window (peak {peak_height}); \
                     refusing to batch-sync toward a chain the proof does not attest"
                )));
            }
        }
        *self.long_sync_anchor.write().await = Some(validated.tip);
        info!(
            "long-sync landing anchored (weight proof validated, fork point resolved) peak_height={} tip={}",
            peak_height, validated.tip
        );
        Ok(true)
    }

    // The reorg-across-the-gap reland (the fork point below the peak): drive the
    // chaser's window re-follow from the fork point with the FullNode's per-peak side effects
    // (wallet coin-state + mempool revalidation fire for every reorg delta, exactly as in
    // [`FullNode::sync_backtrack`]).
    pub(in crate::node) async fn long_sync_rewind(
        &self,
        peers: &[Arc<OutboundPeer>],
        fork_point: u32,
    ) -> Result<(), Error> {
        let Some(peer) = peers.first() else {
            return Err(Error::other("long-sync reland: no live peer"));
        };
        let source: Arc<dyn BlockRangeSource> =
            Arc::new(OutboundPeerSource::new(peer.clone(), REQUEST_TIMEOUT));
        self.follow_inflight_since.store(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
            Ordering::Relaxed,
        );
        let stepped = {
            let mut chaser = self.chaser.lock().await;
            chaser.long_sync_reland_reporting(&source, fork_point).await
        };
        self.follow_inflight_since.store(0, Ordering::Relaxed);
        let confirmed = stepped.map_err(|e| Error::other(e.to_string()))?;
        self.finish_confirmed_window(confirmed)
            .await
            .map_err(|e| Error::other(e.to_string()))?;
        Ok(())
    }

    /// One-time mid-chain anchor for `--sync-from H`: validate the weight proof for the
    /// claimed tip (its sub-epoch summaries give the WHOLE chain's epoch schedule), download the
    /// span just below H from a live peer, run the existing headers-first candidate pass over it
    /// against that schedule, and warm the engine cache. The follow driver then body-syncs from
    /// the span's start; the first bodies anchor on the candidate ancestry exactly like a
    /// weight-proof checkpoint. Returns `false` (retry next tick) until a peer + proof exist.
    ///
    /// # Errors
    /// Returns an I/O error if the header pass rejects the fetched span.
    pub(in crate::node) async fn anchor_at(
        &self,
        registry: &Arc<dyn OutboundPeers>,
        h: u32,
    ) -> Result<bool, Error> {
        let peers = registry.live_peers().await;
        let Some(validated) = self.validated_proof(&peers).await? else {
            return Ok(false);
        };
        let sources: Vec<Arc<dyn BlockRangeSource>> = peers
            .iter()
            .map(|peer| {
                Arc::new(OutboundPeerSource::new(peer.clone(), REQUEST_TIMEOUT))
                    as Arc<dyn BlockRangeSource>
            })
            .collect();
        self.anchor_with_sources(&sources, &validated, h).await
    }

    async fn anchor_with_sources(
        &self,
        sources: &[Arc<dyn BlockRangeSource>],
        validated: &ValidatedTip,
        height: u32,
    ) -> Result<bool, Error> {
        let start = height.saturating_sub(64);
        let end = height.saturating_add(31);
        'sources: for source in sources {
            let mut span = Vec::new();
            let mut low = start;
            while low <= end {
                let high = end.min(low.saturating_add(31));
                match source.fetch_range(low, high).await {
                    Ok(blocks)
                        if blocks.len() == (high - low + 1) as usize
                            && blocks
                                .iter()
                                .enumerate()
                                .all(|(index, block)| block.height() == low + index as u32) =>
                    {
                        span.extend(blocks)
                    }
                    _ => continue 'sources,
                }
                if high == end {
                    break;
                }
                low = high + 1;
            }
            let headers: Vec<_> = span
                .iter()
                .map(dg_xch_node::header_block_from_full_block)
                .collect();
            let mut chaser = self.chaser.lock().await;
            let schedule = chaser.epoch_schedule(&validated.summaries);
            if let Err(error) = chaser
                .sync_headers(&headers, &schedule, &validated.summaries)
                .await
            {
                warn!(
                    "sync-from anchor rejected peer={} error={error}",
                    source.peer_id()
                );
                continue;
            }
            chaser.seed_summary_chain(validated.summaries.to_vec());
            if let Err(error) = chaser
                .backfill_epoch_depth(sources, &validated.summaries, start)
                .await
            {
                warn!("sync-from epoch-depth backfill failed error={error}");
                return Ok(false);
            }
            if let Err(error) = chaser.warm_engine_cache().await {
                warn!("sync-from cache warm failed error={error}");
            }
            *self.sync_from_anchor.write().await = Some(start);
            info!("sync-from anchor established anchor={start} target={height}");
            return Ok(true);
        }
        warn!("sync-from anchor: no peer served a valid span start={start} end={end}");
        Ok(false)
    }

    pub(in crate::node) async fn local_peak_weight(&self) -> Option<u128> {
        let (hash, _) = self.store.get_peak().await.ok().flatten()?;
        self.store
            .get_block_record(&hash)
            .await
            .ok()
            .flatten()
            .map(|rec| rec.weight)
    }

    /// The sync target: the HEAVIEST live peer claim (`sync_store.get_heaviest_peak`, already
    /// net of quarantined peaks and retracted/stale claims), gated to claims strictly heavier than
    /// our confirmed peak — lighter announcements drop at `new_peak` ("Not interested in less
    /// heavy peaks") and `request_validate_wp` refuses a target not heavier than the local peak
    /// ("already caught up"). `None` means caught up or nothing (heavier) claimed: a longer-but-
    /// LIGHTER fork is not a target, and every consumer band (tip_follower / FOLLOW fill / bulk)
    /// idles instead of grinding toward it.
    pub(crate) async fn sync_target(&self) -> Option<PeakClaim> {
        let heaviest = self.peak_book.heaviest()?;
        match self.local_peak_weight().await {
            Some(local_weight) if heaviest.weight <= local_weight => None,
            _ => Some(heaviest),
        }
    }
}
