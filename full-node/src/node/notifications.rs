use super::*;

impl<S> FullNode<S>
where
    S: BlockStore + CoinStore + Send + Sync + 'static,
{
    /// Record the peer a gossiped transaction arrived FROM — its dispatch peer id AND its remote
    /// host — so the `NewTransaction` re-broadcast excludes it (`broadcast_added_tx`'s
    /// `current_peer`). The host is what excludes an OUTBOUND origin, whose
    /// dispatch id is our own shared client-cert hash. Bounded: entries
    /// older than 60s are pruned on insert and the map is capped — an unconsumed entry (failed
    /// admission) cannot accumulate.
    pub async fn note_tx_origin(&self, txid: Bytes32, peer: Bytes32, host: Option<IpAddr>) {
        record_tx_origin(
            &self.tx_origin,
            txid,
            TxOrigin {
                peer_id: peer,
                host,
            },
        )
        .await;
    }

    /// Drain the queued `NewTransaction` announcements to every connected FULL_NODE peer —
    /// inbound and outbound — excluding each transaction's origin peer. Public so
    /// the integration suite can drive the drain the driver loop normally runs.
    pub async fn drain_tx_announcements(self: &Arc<Self>, registry: &Arc<dyn OutboundPeers>) {
        broadcast_transactions(self, registry).await;
    }

    /// The sync-end transition — `_finish_sync`. After a bulk/
    /// fast-sync band lands its recent-chain peak and exits to the follow driver, fire peak-post-
    /// processing ONCE against the final peak, with EMPTY coin deltas:
    ///   - mempool revalidation against the new (transaction) peak — the empty spent set
    ///     expires time-locked items and re-admits parked ones without dropping for spends (the
    ///     batch path already advanced the coin set);
    ///   - NewPeak to full-node peers + slot-state advance + NewPeakTimelord to timelords;
    ///   - NewPeakWallet to wallet peers.
    ///
    /// There is NO per-coin `CoinStateUpdate`: `lookup_coin_ids` is EMPTY at `_finish_sync`
    /// (the empty StateChangeSummary), so subscribers get only the peak announcement — the bounded
    /// push the sync-end owes, NEVER an unbounded per-block replay of the synced span. A wallet
    /// re-pages the coin state it missed via RequestPuzzleState/RequestCoinState anchored on the
    /// announced peak. Called by the driver on the fast-sync landing (the band-exit seam that the
    /// per-block follow side effects never covered).
    pub async fn finish_sync_transition(
        self: &Arc<Self>,
        registry: &Arc<dyn OutboundPeers>,
        inbound_peers: &PeerMap,
    ) {
        let Ok(Some((hash, height))) = self.store.get_peak().await else {
            return;
        };
        // The next follow block chains onto this landed peak as a plain extension — seed the
        // reorg-chain tracker so it is not mis-read as a hash-break reorg.
        *self.last_delta_hash.lock().await = Some(hash);
        // Mempool revalidation against the transaction peak framing the new tip.
        if let Some((tx_height, tx_ts)) = self.tx_peak_frame(hash).await
            && let Err(e) = self
                .mempool
                .lock()
                .await
                .new_peak(self.store.as_ref(), tx_height, tx_ts, &[])
                .await
        {
            warn!("sync-end mempool revalidation failed error={}", e);
        }
        // NewPeak to full-node peers + slot-state advance + NewPeakTimelord to timelords.
        broadcast_new_peak(self, registry, hash, height).await;
        update_slot_state_on_peak(self, hash).await;
        broadcast_new_peak_timelord(self, inbound_peers, hash).await;
        // NewPeakWallet to wallet peers (no per-coin CoinStateUpdate — an empty summary).
        // fork_point = max(height - 1, 0): `_finish_sync` builds `StateChangeSummary(peak,
        // fork_point, …)` with `fork_point = max(peak.height - 1, 0)` and update_wallets carries
        // that same fork height into NewPeakWallet — the height-1 default the
        // NewPeak broadcast above also uses, NOT the on-connect greeting's peak-height convention.
        let wallets = wallet_peers(inbound_peers).await;
        if !wallets.is_empty()
            && let Ok(Some(rec)) = self.store.get_block_record(&hash).await
        {
            let announce = NewPeakWallet {
                header_hash: hash,
                height,
                weight: rec.weight,
                fork_point_with_previous_peak: height.saturating_sub(1),
            };
            broadcast_new_peak_wallet(&self.net, &wallets, &announce).await;
        }
        info!("sync-end transition fired height={}", height);
    }

    // The transaction block framing a peak: walk from the peak to the nearest record carrying a
    // timestamp (`get_tx_peak`). Bounded like `chain_is_current`; `None` if none within the
    // window (a from-genesis peak with no transaction block yet).
    pub(super) async fn tx_peak_frame(&self, peak_hash: Bytes32) -> Option<(u32, u64)> {
        let mut curr = self
            .store
            .get_block_record(&peak_hash)
            .await
            .ok()
            .flatten()?;
        for _ in 0..512 {
            if let Some(ts) = curr.timestamp {
                return Some((curr.height, ts));
            }
            curr = self
                .store
                .get_block_record(&curr.prev_hash)
                .await
                .ok()
                .flatten()?;
        }
        None
    }

    // One confirmed block's side effects: drop mempool items the block spent, then emit wallet coin-state
    // updates for the coins it created/spent. Bounded work — proportional to the block's coin delta.
    // `reorg` is Some on the first re-applied block of a landed reorg (the chaser's
    // [`ConfirmedDelta`] feed): the rolled-back coin states are pushed to subscribers and the true
    // fork height replaces the height-1 simplification.
    /// Apply a locally-produced peak's wallet-facing effects: revalidate the mempool at the new
    /// peak, roll wallet subscriptions forward (or back on a reorg), and push `CoinStateUpdate` +
    /// `NewPeakWallet` to every subscribed wallet peer. A simulator that produces blocks out of band
    /// drives this directly, since it does not run the follow loop that normally calls it.
    pub async fn notify_new_peak(
        &self,
        d: &BlockDelta,
        reorg: Option<&ReorgWalletDelta>,
    ) -> Result<(), Error> {
        // Reorg detection by hash-chain break: the engine may surface a deep reorg as just the
        // new tip's delta, so height monotonicity can't be trusted — a delta whose prev_hash
        // isn't the last delta we processed means blocks were rolled back. The mempool takes the
        // slow path there (the full pool rebuild):
        // `Mempool::revalidate_for_reorg` — drop items whose removals ceased to exist
        // (UNKNOWN_UNSPENT) or were spent on the winning branch, rebase surviving FF spends.
        // The threaded reorg delta forces the same path even when the branch's first block
        // happens to chain onto the last processed delta's hash.
        let reorged = {
            let mut last = self.last_delta_hash.lock().await;
            let broke_chain = last.is_some_and(|h| h != d.prev_hash);
            *last = Some(d.header_hash);
            broke_chain || reorg.is_some()
        };
        if reorged {
            let dropped = self
                .mempool
                .lock()
                .await
                .revalidate_for_reorg(self.store.as_ref())
                .await
                .map_err(|e| Error::other(e.to_string()))?;
            info!(
                "reorg landing: mempool revalidated on the slow path height={} dropped={}",
                d.height, dropped
            );
        }
        // `mempool_manager.new_peak`: "we're only interested in transaction blocks" — the
        // mempool peak must always be the most recent TRANSACTION block, whose height + timestamp
        // are the reference frame every time-lock admission checks against. A non-transaction
        // delta (timestamp 0, no foliage_transaction_block, no coin activity) leaves the pool
        // untouched.
        if d.timestamp != 0 {
            let peak_result = self
                .mempool
                .lock()
                .await
                .new_peak(self.store.as_ref(), d.height, d.timestamp, &d.removals)
                .await
                .map_err(|e| Error::other(e.to_string()))?;
            // Parked bundles that became admissible at this peak re-gossip like fresh admissions.
            if !peak_result.admitted.is_empty() {
                let mut announces = self.tx_announce.lock().await;
                for (name, cost, fees) in peak_result.admitted {
                    announces.push(NewTransaction {
                        transaction_id: name,
                        cost,
                        fees,
                    });
                }
            }
        }
        // The true fork height when this delta lands a reorg (threaded into every wallet
        // push); height-1 IS the fork point for every plain extension.
        let fork_height = reorg.map_or_else(|| d.height.saturating_sub(1), |r| r.fork_height);
        // Rolled-back states FIRST, then the branch's own delta — the same final state as one
        // combined `rolled_back_records + new_states` push.
        if let Some(r) = reorg
            && !r.rolled_back.is_empty()
        {
            self.wallet
                .notify_coin_states(d.header_hash, d.height, fork_height, &r.rolled_back)
                .await;
        }
        self.wallet
            .on_new_peak(
                self.store.as_ref(),
                crate::wallet::WalletUpdate {
                    peak_hash: d.header_hash,
                    height: d.height,
                    fork_height,
                    created: &d.additions,
                    spent_ids: &d.removals,
                    // The block's create-coin (hint, coin_id) pairs: a hint equal to a subscribed
                    // puzzle hash matches like the puzzle hash itself.
                    hints: &d.hints,
                },
            )
            .await
            .map_err(|e| Error::other(e.to_string()))?;
        // AFTER the per-subscriber CoinStateUpdate deltas, EVERY wallet-type peer gets the peak
        // as NewPeakWallet — subscribed or not (Sage
        // tracks the network peak from this push, and its delta sync anchors on it). Snapshot the
        // wallet peers first: with none connected (the common case, and every bulk-sync block)
        // this is one cheap read-lock and no store read. fork_point is the true fork height on a
        // threaded reorg delta and height-1 for every plain extension.
        let wallets = wallet_peers(&self.inbound_peers).await;
        if !wallets.is_empty()
            && let Ok(Some(rec)) = self.store.get_block_record(&d.header_hash).await
        {
            let announce = NewPeakWallet {
                header_hash: d.header_hash,
                height: d.height,
                weight: rec.weight,
                fork_point_with_previous_peak: fork_height,
            };
            broadcast_new_peak_wallet(&self.net, &wallets, &announce).await;
        }
        Ok(())
    }
}
