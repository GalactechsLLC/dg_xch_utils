use super::*;

impl<S> FullNode<S>
where
    S: BlockStore + CoinStore + Send + Sync + 'static,
{
    /// Repair consensus ancestry after restarting over an existing chain.
    ///
    /// `deepen` is set when an ordinary floor repair did not cover a stage walk, and extends the
    /// backfill by another epoch instead of repeatedly warming the same insufficient span.
    ///
    /// # Errors
    /// Returns an I/O error on a store failure or failed proof fetch/validation.
    pub(in crate::node) async fn resume_repair(
        &self,
        registry: &Arc<dyn OutboundPeers>,
        deepen: bool,
    ) -> Result<bool, Error> {
        let Some((peak_hash, local)) = self
            .store
            .get_peak()
            .await
            .map_err(|e| Error::other(e.to_string()))?
        else {
            return Ok(true); // Empty store: fast-sync owns the from-zero path.
        };
        {
            let mut chaser = self.chaser.lock().await;
            match chaser.warm_engine_cache().await {
                Ok(n) => {
                    info!("engine walk cache warmed (resume) records={}", n);
                    // mm-OOM visibility: the resume path is the one the OOMing node takes on every
                    // restart — this self-report is the allocation evidence its 8-second life lacked.
                    crate::metrics::log_startup_memory("resume", n);
                }
                Err(e) => warn!("engine cache warm failed (resume) error={}", e),
            }
        }
        // The deepest record the next possible epoch retarget can read from this peak — the
        // PENDING boundary's previous-surpass depth (`epoch_backfill_low`), NOT the boundary
        // rounded up from the peak: that rounding concluded "nothing to backfill" on every
        // restart while the peak sat 13 blocks past boundary 4,575,744 with its retarget still
        // pending, leaving the follow loop walled on "block record not found" forever.
        let needed_low = dg_xch_node::sync::epoch_backfill_low(
            local,
            self.constants.epoch_blocks,
            self.constants.sub_epoch_blocks,
        );
        // The record floor is measured by PREV-HASH WALK from the peak (crate::resume_floor), not
        // by height. By-height lookups are main-chain-only on every backend, so epoch-backfill
        // CANDIDATE records are invisible to them, and a by-height binary search assumes hole-free
        // monotone presence, so a mid-span record hole above the floor reads as "nothing to
        // repair". The hash walk sees candidates and breaks exactly at a hole.
        let outcome = crate::resume_floor::measure_record_floor(
            self.store.as_ref(),
            peak_hash,
            local,
            needed_low,
        )
        .await
        .map_err(|e| Error::other(e.to_string()))?;
        let anchor = match outcome {
            crate::resume_floor::RecordFloor::Reached { floor } if !deepen || floor == 0 => {
                return Ok(true);
            }
            // Deepened repair (see the doc comment): backfill anchored at the reach point, one
            // epoch below the standard floor.
            crate::resume_floor::RecordFloor::Reached { floor } => floor,
            crate::resume_floor::RecordFloor::Broken { stop } => stop,
        };
        let peers = registry.live_peers().await;
        let Some(validated) = self.validated_proof(&peers).await? else {
            return Ok(false);
        };
        let sources: Vec<Arc<dyn BlockRangeSource>> = peers
            .iter()
            .map(|p| {
                Arc::new(OutboundPeerSource::new(p.clone(), REQUEST_TIMEOUT))
                    as Arc<dyn BlockRangeSource>
            })
            .collect();
        let mut chaser = self.chaser.lock().await;
        match chaser
            .backfill_epoch_depth(&sources, &validated.summaries, anchor)
            .await
        {
            Ok(n) => info!(
                "resume epoch-depth backfill complete records={} anchor={}",
                n, anchor
            ),
            Err(e) => {
                warn!("resume backfill incomplete, retrying next tick error={}", e);
                return Ok(false);
            }
        }
        match chaser.warm_engine_cache().await {
            Ok(n) => info!("engine walk cache re-warmed after backfill records={}", n),
            Err(e) => warn!("engine cache warm failed after backfill error={}", e),
        }
        Ok(true)
    }
}
