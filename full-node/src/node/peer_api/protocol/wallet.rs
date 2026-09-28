use super::*;

mod queries;
mod subscriptions;

impl<S: BlockStore + CoinStore + Send + Sync + 'static> StoreApi<S> {
    // on_connect: the current peak as the wallet greeting —
    // fork_point_with_previous_peak is the peak HEIGHT on connect. None while the store has no
    // peak (`peak_full is None`).
    pub(super) async fn wallet_peak(&self) -> Option<NewPeakWallet> {
        let (hash, height) = self.store.get_peak().await.ok().flatten()?;
        let rec = self.store.get_block_record(&hash).await.ok().flatten()?;
        Some(NewPeakWallet {
            header_hash: hash,
            height,
            weight: rec.weight,
            fork_point_with_previous_peak: height,
        })
    }

    // NODE→FULL_NODE greeting (on_connect): the confirmed peak as
    // `NewPeak(header_hash, height, weight, peak.height, unfinished_reward_block_hash)` — fork
    // point = the peak height itself on connect, and the unfinished hash committed exactly as
    // the commitment is `reward_chain_block.get_unfinished().get_hash()`.
    pub(super) async fn full_node_peak(&self) -> Option<NewPeak> {
        on_connect_new_peak(self.store.as_ref()).await
    }

    // NODE→TIMELORD greeting (on_connect send_peak_to_timelords):
    // the same construction the peak broadcast runs, over the shared record window. Fails closed
    // (None → nothing sent) when the peak's ancestry cannot ground the difficulty/challenge
    // walks — the timelord then syncs on the next peak advance instead of receiving an
    // approximate message.
    pub(super) async fn timelord_peak(&self) -> Option<Box<NewPeakTimelord>> {
        let (hash, _) = self.store.get_peak().await.ok().flatten()?;
        build_new_peak_timelord(
            self.store.as_ref(),
            &self.constants,
            &self.record_window,
            &self.sync_metrics,
            hash,
        )
        .await
    }

    pub(super) async fn timelord_genesis(&self) -> Option<NewGenesisTimelord> {
        if !self.allow_chain_bootstrap || !self.production_ready().await {
            return None;
        }
        if self.store.get_peak().await.ok()?.is_some() {
            return None;
        }
        Some(NewGenesisTimelord {
            genesis_challenge: self.constants.genesis_challenge,
            difficulty: self.constants.difficulty_starting,
            sub_slot_iters: self.constants.sub_slot_iters_starting,
            discriminant_size_bits: self.constants.discriminant_size_bits,
        })
    }

    pub(super) async fn mempool_sync_filter(&self) -> Option<Vec<u8>> {
        on_connect_mempool_filter(&self.synced, &self.mempool).await
    }
}
