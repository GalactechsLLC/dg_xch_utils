use super::*;

impl<S> FullNode<S>
where
    S: BlockStore + CoinStore + Send + Sync + 'static,
{
    /// Follow a peer's tip: pull `from..=to`, confirm each block through the engine, and drive the per-peak
    /// side effects (mempool revalidation + wallet coin-state updates). Returns the confirmed peak.
    ///
    /// # Errors
    /// Returns an I/O error if the peer cannot serve the range, a block fails validation, or a store/notify
    /// step fails.
    pub async fn sync_follow(
        &self,
        source: &Arc<dyn BlockRangeSource>,
        from: u32,
        to: u32,
    ) -> Result<Option<(Bytes32, u32)>, Error> {
        self.follow_step(source, from, to)
            .await
            .map_err(|e| Error::other(e.to_string()))
    }

    /// [`FullNode::sync_follow`] over pre-fetched, height-sorted blocks — the driver's prefetch
    /// overlap feeds this so the next window's download runs during this window's validation.
    ///
    /// # Errors
    /// Returns an error if a block fails validation or the store errors.
    pub async fn sync_follow_blocks(
        &self,
        blocks: &[dg_xch_core::blockchain::full_block::FullBlock],
    ) -> Result<Option<(Bytes32, u32)>, Error> {
        self.follow_step_blocks(blocks)
            .await
            .map_err(|e| Error::other(e.to_string()))
    }

    pub(in crate::node) async fn follow_step(
        &self,
        source: &Arc<dyn BlockRangeSource>,
        from: u32,
        to: u32,
    ) -> Result<Option<(Bytes32, u32)>, SyncError> {
        let (peak, deltas) = {
            let mut chaser = self.chaser.lock().await;
            chaser.follow_to_reporting(source, from, to).await?
        };
        self.finish_follow_step(peak, &deltas).await
    }

    // Typed core of [`FullNode::sync_follow_blocks`].
    pub(in crate::node) async fn follow_step_blocks(
        &self,
        blocks: &[dg_xch_core::blockchain::full_block::FullBlock],
    ) -> Result<Option<(Bytes32, u32)>, SyncError> {
        self.follow_step_blocks_pre(blocks, None).await
    }

    // [`FullNode::follow_step_blocks`] with driver-precomputed window bodies (the cross-window body
    // pipeline: window N+1's CLVM/BLS precompute ran while window N validated).
    pub(in crate::node) async fn follow_step_blocks_pre(
        &self,
        blocks: &[dg_xch_core::blockchain::full_block::FullBlock],
        pre: Option<std::collections::HashMap<u32, dg_xch_node::engine::PrecomputedBody>>,
    ) -> Result<Option<(Bytes32, u32)>, SyncError> {
        let (peak, deltas) = {
            let mut chaser = self.chaser.lock().await;
            chaser.follow_blocks_reporting_pre(blocks, pre).await?
        };
        self.finish_follow_step(peak, &deltas).await
    }

    // Stage half of the stage-ahead pipeline: window into the overlay, no writer, no drains —
    // callable while the PREVIOUS window's spawned drain still owns the CPU. An `Err` here has
    // NOT cleared the overlay (see `Chaser::stage_window_pre`); the caller confirms its pending
    // window first, then clears.
    pub(in crate::node) async fn stage_step_window(
        &self,
        blocks: Vec<dg_xch_core::blockchain::full_block::FullBlock>,
        pre: Option<std::collections::HashMap<u32, dg_xch_node::engine::PrecomputedBody>>,
    ) -> Result<dg_xch_node::sync::StagedWindow, SyncError> {
        let mut chaser = self.chaser.lock().await;
        chaser.stage_window_pre(blocks, pre).await
    }

    // Confirm half: the drain's verdict lands the window (archive + coins + peak, one
    // transaction) and the per-peak side effects fire exactly as in the serial step.
    pub(in crate::node) async fn confirm_step_window(
        &self,
        window: dg_xch_node::sync::StagedWindow,
        verdict: dg_xch_node::sync::WindowVerdict,
    ) -> Result<Option<(Bytes32, u32)>, SyncError> {
        let (peak, deltas) = {
            let mut chaser = self.chaser.lock().await;
            chaser.confirm_window_pre(window, verdict).await?
        };
        self.finish_follow_step(peak, &deltas).await
    }

    /// The mirrored `short_sync_backtrack` step, driven when a follow
    /// window fails with the unknown-parent orphan: the chain reorged at/below our stored tip, so
    /// the fork point is fetched backward from the same peer and the collected branch resubmitted
    /// through the ordinary follow pipeline (the engine's existing fork choice performs the reorg).
    /// The per-peak side effects (wallet coin-state + mempool revalidation) fire for every newly
    /// confirmed block exactly as in [`FullNode::sync_follow`].
    ///
    /// # Errors
    /// [`SyncError::DeepFork`] when the fork is deeper than the backtrack cap — the caller must fall
    /// back to the weight-proof long sync instead of retrying; any
    /// fetch/validation/store error otherwise.
    pub async fn sync_backtrack(
        &self,
        source: &Arc<dyn BlockRangeSource>,
        from: u32,
        to: u32,
    ) -> Result<Option<(Bytes32, u32)>, SyncError> {
        // Mark the fetch+confirm in flight for the /health stall dump (cleared on EVERY exit path —
        // a wedged request is exactly when the dump needs its age).
        self.follow_inflight_since.store(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
            Ordering::Relaxed,
        );
        let stepped = {
            let mut chaser = self.chaser.lock().await;
            chaser.follow_backtrack_reporting(source, from, to).await
        };
        self.follow_inflight_since.store(0, Ordering::Relaxed);
        let (peak, deltas) = stepped?;
        self.finish_follow_step(peak, &deltas).await
    }

    /// One near-tip follow step via `new_peak` ladder: forward-extend first, backtrack only on
    /// the unknown-parent orphan ([`Chaser::follow_tip_step_reporting`]). This is the near-tip band's
    /// entry — a direct child of the peak confirms with a single forward `[from, to]` fetch, so the
    /// confirmed peak pins the network tip at lag 0-1 instead of paying a backward peak-refetch per
    /// block. A real reorg at/below the tip still resolves through the same backtrack recovery arm.
    pub async fn sync_tip_step(
        &self,
        source: &Arc<dyn BlockRangeSource>,
        from: u32,
        to: u32,
    ) -> Result<Option<(Bytes32, u32)>, SyncError> {
        self.follow_inflight_since.store(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
            Ordering::Relaxed,
        );
        let stepped = {
            let mut chaser = self.chaser.lock().await;
            chaser.follow_tip_step_reporting(source, from, to).await
        };
        self.follow_inflight_since.store(0, Ordering::Relaxed);
        let (peak, deltas) = stepped?;
        self.finish_follow_step(peak, &deltas).await
    }

    // Shared tail of every follow-shaped step: per-peak side effects + the synced flag.
    pub(in crate::node) async fn finish_follow_step(
        &self,
        peak: Option<(Bytes32, u32)>,
        deltas: &[ConfirmedDelta],
    ) -> Result<Option<(Bytes32, u32)>, SyncError> {
        let started = std::time::Instant::now();
        for cd in deltas {
            let d = &cd.delta;
            // S8 — terminal PASS: a confirmed block whose header hash matches one WE farmed. The farmed
            // foliage hash recorded at splice time (S5) IS the FullBlock header hash, so a match means
            // our unfinished block was infused and completed. Take it out of the FIFO so we count once.
            {
                let mut farmed = self.farmed_headers.lock().await;
                if let Some(pos) = farmed.iter().position(|h| *h == d.header_hash) {
                    farmed.remove(pos);
                    drop(farmed);
                    self.producer.full_block();
                    info!(
                        "full block confirmed from OUR farmed unfinished block event={} height={} header={}",
                        "producer.full_block.added", d.height, d.header_hash
                    );
                }
            }
            self.notify_new_peak(d, cd.reorg.as_ref())
                .await
                .map_err(SyncError::Io)?;
        }
        if peak.is_some() {
            self.update_synced().await;
        }
        self.sync_metrics.post_confirm_total_micros.fetch_add(started.elapsed().as_micros() as u64, Ordering::Relaxed);
        Ok(peak)
    }

    pub async fn update_synced(&self) {
        let peak = self.store.get_peak().await.ok().flatten();
        let synced = match peak {
            Some((hash, _)) => self.chain_is_current(&hash).await,
            None => false,
        };
        let was = self.synced.swap(synced, Ordering::Relaxed);
        // The not-synced -> synced rising edge is the sync->tip transition: fire the deferred
        // secondary-index build exactly once (during bulk sync the coin_record secondary
        // indexes are pure write-amplification; a
        // tip-serving node needs them). Off the follow path: Postgres builds CONCURRENTLY and
        // SQLite yields the writer between statements, so confirms continue underneath. CREATE
        // INDEX IF NOT EXISTS makes a restart-at-tip re-run a cheap no-op; on failure the latch
        // resets so the next rising edge (or a restart) retries. A successful build re-arms the
        // falling-edge shed below — the two latches alternate, so the drop/rebuild cycle can
        // only run once per genuine fall-behind/re-catch-up excursion.
        if synced && !was && !self.deferred_indexes_started.swap(true, Ordering::Relaxed) {
            let store = self.store.clone();
            let latch = self.deferred_indexes_started.clone();
            let shed_latch = self.service_indexes_shed.clone();
            self.maintenance_tasks.lock().await.spawn(async move {
                let started = std::time::Instant::now();
                match store.build_indexes().await {
                    Ok(()) => {
                        shed_latch.store(false, Ordering::Relaxed);
                        info!(
                            "deferred secondary indexes built at tip elapsed_ms={}",
                            started.elapsed().as_millis() as u64
                        );
                    }
                    Err(e) => {
                        latch.store(false, Ordering::Relaxed);
                        warn!(
                            "deferred index build failed; retrying on the next sync edge error={}",
                            e
                        );
                    }
                }
            });
        }
        // The FALLING edge: a node that reached tip (full index set built) and then fell DEEP
        // behind re-applies settled history while maintaining every secondary index, which turns
        // the confirm window into coin_record index/heap random reads with no HOT spend-updates.
        // Shed the secondary indexes once, off the follow path, and re-arm the build latch so the
        // rising edge rebuilds them at the next sync->tip transition, BEFORE the node re-enters
        // the reorg-exposed zone. Keyed on tip_lag depth, not the raw synced bit: `synced` flips
        // on a 1-block dip and would churn a multi-GB drop/rebuild. Arming by depth alone means a
        // restart mid-deep-catch-up re-derives the shed from the live phase and re-enters it with
        // a cheap idempotent re-drop instead of carrying stale state.
        if !synced {
            let local = peak.map_or(0, |(_, h)| h);
            let tip_lag = self
                .claimed_peak
                .load(Ordering::Relaxed)
                .saturating_sub(local);
            if local > 0
                && tip_lag > SHED_TIP_LAG_BLOCKS
                && !self.service_indexes_shed.swap(true, Ordering::Relaxed)
            {
                let store = self.store.clone();
                let build_latch = self.deferred_indexes_started.clone();
                let latch = self.service_indexes_shed.clone();
                self.maintenance_tasks.lock().await.spawn(async move {
                    let started = std::time::Instant::now();
                    match store.shed_service_indexes().await {
                        Ok(()) => {
                            build_latch.store(false, Ordering::Relaxed);
                            info!(
                                "secondary indexes shed for deep re-catch-up elapsed_ms={} tip_lag={}",
                                started.elapsed().as_millis() as u64,
                                tip_lag
                            );
                        }
                        Err(e) => {
                            latch.store(false, Ordering::Relaxed);
                            warn!(
                                "index shed failed; retrying while still deep behind error={}",
                                e
                            );
                        }
                    }
                });
            }
        }
    }

    // — walk from the peak to the last transaction block and compare its
    // timestamp against now - 7 minutes. The walk is bounded generously: mainnet guarantees a
    // transaction block within far fewer records, and a missing/older-than-window record is simply
    // "not synced" — fail-closed on a stale chain.
    pub(in crate::node) async fn chain_is_current(&self, peak_hash: &Bytes32) -> bool {
        let mut curr = self.store.get_block_record(peak_hash).await.ok().flatten();
        for _ in 0..512 {
            match &curr {
                None => return false,
                Some(rec) if rec.timestamp.is_some() => break,
                Some(rec) => {
                    curr = self
                        .store
                        .get_block_record(&rec.prev_hash)
                        .await
                        .ok()
                        .flatten();
                }
            }
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        curr.and_then(|r| r.timestamp)
            .is_some_and(|ts| ts >= now.saturating_sub(60 * 7))
    }
}
