use crate::error::StoreError;
use crate::traits::{BlockStore, CoinStore};
use crate::types::{BatchHandle, BlockStatus, Savepoint};
use async_trait::async_trait;
use dg_xch_core::blockchain::block_record::BlockRecord;
use dg_xch_core::blockchain::coin_record::CoinRecord;
use dg_xch_core::blockchain::full_block::FullBlock;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::clvm::program::SerializedProgram;
use std::sync::Arc;

// Sharing seam: the sync pipeline hands cloned store handles to its parallel download tasks while the engine
// holds one too. Forwarding the store traits through `Arc<T>` lets one concrete backend fan out with no extra
// wrapper — the single-writer/WAL-reader concurrency lives inside the backend, unchanged.
#[async_trait]
impl<T: CoinStore + Send + Sync> CoinStore for Arc<T> {
    async fn prepare_coin_window(
        &self,
        changes: Vec<crate::types::OwnedCoinChanges>,
    ) -> Result<crate::types::PreparedCoinWindow, StoreError> {
        (**self).prepare_coin_window(changes).await
    }

    async fn apply_prepared_coin_window_in(
        &self,
        batch: &mut BatchHandle,
        prepared: crate::types::PreparedCoinWindow,
    ) -> Result<(), StoreError> {
        (**self)
            .apply_prepared_coin_window_in(batch, prepared)
            .await
    }

    async fn apply_coin_window_in(
        &self,
        batch: &mut BatchHandle,
        changes: &[crate::types::CoinChanges<'_>],
    ) -> Result<(), StoreError> {
        (**self).apply_coin_window_in(batch, changes).await
    }

    async fn get_coin_record(&self, coin_name: &Bytes32) -> Result<Option<CoinRecord>, StoreError> {
        (**self).get_coin_record(coin_name).await
    }
    async fn get_coin_records(&self, names: &[Bytes32]) -> Result<Vec<CoinRecord>, StoreError> {
        (**self).get_coin_records(names).await
    }
    async fn apply_block(
        &self,
        height: u32,
        timestamp: u64,
        additions: &[CoinRecord],
        removals: &[Bytes32],
    ) -> Result<(), StoreError> {
        (**self)
            .apply_block(height, timestamp, additions, removals)
            .await
    }
    async fn apply_block_in(
        &self,
        batch: &mut BatchHandle,
        height: u32,
        timestamp: u64,
        additions: &[CoinRecord],
        removals: &[Bytes32],
    ) -> Result<(), StoreError> {
        (**self)
            .apply_block_in(batch, height, timestamp, additions, removals)
            .await
    }
    async fn rollback_to(&self, fork_height: u32) -> Result<u64, StoreError> {
        (**self).rollback_to(fork_height).await
    }
    async fn rollback_to_in(
        &self,
        batch: &mut BatchHandle,
        fork_height: u32,
    ) -> Result<u64, StoreError> {
        (**self).rollback_to_in(batch, fork_height).await
    }
    // Must delegate: the trait default is a no-op, so without this Arc<PostgresStore> would silently
    // skip the reorg-index build (the engine's store is Arc-wrapped on the live path).
    async fn ensure_reorg_indexes(&self) -> Result<(), StoreError> {
        (**self).ensure_reorg_indexes().await
    }

    #[cfg(feature = "coin-index")]
    async fn get_unspent_by_puzzle_hash(
        &self,
        ph: &Bytes32,
    ) -> Result<Vec<CoinRecord>, StoreError> {
        (**self).get_unspent_by_puzzle_hash(ph).await
    }
    #[cfg(feature = "coin-index")]
    async fn get_coins_by_parent(&self, parent: &Bytes32) -> Result<Vec<CoinRecord>, StoreError> {
        (**self).get_coins_by_parent(parent).await
    }
    // Must forward: get_coin_states_by_puzzle_hashes has no default (the wallet subscription
    // initial-state read), so an `Arc<S>` store handle must delegate to the concrete backend.
    #[cfg(feature = "coin-index")]
    async fn get_coin_states_by_puzzle_hashes(
        &self,
        puzzle_hashes: &[Bytes32],
        min_height: u32,
        include_spent: bool,
        max_items: usize,
    ) -> Result<Vec<dg_xch_core::protocols::wallet::CoinState>, StoreError> {
        (**self)
            .get_coin_states_by_puzzle_hashes(puzzle_hashes, min_height, include_spent, max_items)
            .await
    }
    // Must forward: batch_coin_states_by_puzzle_hashes has no default (the RequestPuzzleState
    // paged read), so an `Arc<S>` store handle must delegate to the concrete backend.
    #[cfg(feature = "coin-index")]
    async fn batch_coin_states_by_puzzle_hashes(
        &self,
        puzzle_hashes: &[Bytes32],
        min_height: u32,
        filters: &dg_xch_core::protocols::wallet::CoinStateFilters,
        max_items: usize,
    ) -> Result<(Vec<dg_xch_core::protocols::wallet::CoinState>, Option<u32>), StoreError> {
        (**self)
            .batch_coin_states_by_puzzle_hashes(puzzle_hashes, min_height, filters, max_items)
            .await
    }

    #[cfg(feature = "coin-index")]
    async fn get_coins_added_at_height(&self, height: u32) -> Result<Vec<CoinRecord>, StoreError> {
        (**self).get_coins_added_at_height(height).await
    }
    #[cfg(feature = "coin-index")]
    async fn get_coins_removed_at_height(
        &self,
        height: u32,
    ) -> Result<Vec<CoinRecord>, StoreError> {
        (**self).get_coins_removed_at_height(height).await
    }
    // Must forward: the trait's provided default is a no-op, so without these delegations an
    // `Arc<SqliteStore>` (the engine's store handle) would silently drop every hint.
    async fn apply_hints_in(
        &self,
        batch: &mut BatchHandle,
        pairs: &[(Bytes32, Bytes32)],
    ) -> Result<(), StoreError> {
        (**self).apply_hints_in(batch, pairs).await
    }
    async fn apply_hints(&self, pairs: &[(Bytes32, Bytes32)]) -> Result<(), StoreError> {
        (**self).apply_hints(pairs).await
    }
    #[cfg(feature = "hint")]
    async fn get_coins_for_hint(
        &self,
        hint: &Bytes32,
        max_items: usize,
    ) -> Result<Vec<Bytes32>, StoreError> {
        (**self).get_coins_for_hint(hint, max_items).await
    }
}

#[async_trait]
impl<T: BlockStore + Send + Sync> BlockStore for Arc<T> {
    async fn prepare_archive(
        &self,
        records: Vec<(BlockRecord, BlockStatus)>,
        blocks: Vec<FullBlock>,
    ) -> Result<crate::types::PreparedArchive, StoreError> {
        (**self).prepare_archive(records, blocks).await
    }

    async fn persist_prepared_archive_in(
        &self,
        batch: &mut BatchHandle,
        prepared: crate::types::PreparedArchive,
    ) -> Result<(), StoreError> {
        (**self).persist_prepared_archive_in(batch, prepared).await
    }

    async fn get_block_record(&self, hh: &Bytes32) -> Result<Option<BlockRecord>, StoreError> {
        (**self).get_block_record(hh).await
    }
    async fn get_block_record_by_height(&self, h: u32) -> Result<Option<BlockRecord>, StoreError> {
        (**self).get_block_record_by_height(h).await
    }
    async fn get_block_records_by_hash(
        &self,
        hashes: &[Bytes32],
    ) -> Result<Vec<BlockRecord>, StoreError> {
        (**self).get_block_records_by_hash(hashes).await
    }
    async fn get_peak(&self) -> Result<Option<(Bytes32, u32)>, StoreError> {
        (**self).get_peak().await
    }
    async fn get_block(&self, hh: &Bytes32) -> Result<Option<FullBlock>, StoreError> {
        (**self).get_block(hh).await
    }
    async fn get_generator_at_height(
        &self,
        h: u32,
    ) -> Result<Option<SerializedProgram>, StoreError> {
        (**self).get_generator_at_height(h).await
    }
    async fn add_block_records(&self, records: &[BlockRecord]) -> Result<(), StoreError> {
        (**self).add_block_records(records).await
    }
    async fn add_block_records_in(
        &self,
        batch: &mut BatchHandle,
        records: &[BlockRecord],
    ) -> Result<(), StoreError> {
        (**self).add_block_records_in(batch, records).await
    }
    async fn begin(&self) -> Result<BatchHandle, StoreError> {
        (**self).begin().await
    }
    async fn append_many(
        &self,
        batch: &mut BatchHandle,
        blocks: &[FullBlock],
    ) -> Result<(), StoreError> {
        (**self).append_many(batch, blocks).await
    }
    async fn commit(&self, batch: BatchHandle) -> Result<(), StoreError> {
        (**self).commit(batch).await
    }
    fn near_tip(&self) -> bool {
        (**self).near_tip()
    }
    fn set_near_tip(&self, near_tip: bool) {
        (**self).set_near_tip(near_tip);
    }
    async fn min_record_height(&self) -> Result<Option<u32>, StoreError> {
        (**self).min_record_height().await
    }
    fn telemetry(&self) -> Option<Arc<crate::telemetry::StoreTelemetry>> {
        (**self).telemetry()
    }
    fn wal_bytes(&self) -> u64 {
        (**self).wal_bytes()
    }
    async fn get_unassociated(&self, limit: usize) -> Result<Vec<u32>, StoreError> {
        (**self).get_unassociated(limit).await
    }
    async fn set_peak(&self, new_peak: &Bytes32) -> Result<u64, StoreError> {
        (**self).set_peak(new_peak).await
    }
    async fn set_peak_in(
        &self,
        batch: &mut BatchHandle,
        new_peak: &Bytes32,
    ) -> Result<u64, StoreError> {
        (**self).set_peak_in(batch, new_peak).await
    }
    async fn extend_peak_in(
        &self,
        batch: &mut BatchHandle,
        extension: &[Bytes32],
        new_height: u32,
    ) -> Result<u64, StoreError> {
        (**self).extend_peak_in(batch, extension, new_height).await
    }
    async fn get_status(&self, hh: &Bytes32) -> Result<BlockStatus, StoreError> {
        (**self).get_status(hh).await
    }
    async fn set_status(&self, hh: &Bytes32, s: BlockStatus) -> Result<(), StoreError> {
        (**self).set_status(hh, s).await
    }
    async fn set_status_in(
        &self,
        batch: &mut BatchHandle,
        hh: &Bytes32,
        s: BlockStatus,
    ) -> Result<(), StoreError> {
        (**self).set_status_in(batch, hh, s).await
    }
    async fn set_status_many_in(
        &self,
        batch: &mut BatchHandle,
        hashes: &[Bytes32],
        status: BlockStatus,
    ) -> Result<(), StoreError> {
        (**self).set_status_many_in(batch, hashes, status).await
    }
    async fn savepoint(&self) -> Result<Savepoint, StoreError> {
        (**self).savepoint().await
    }
    async fn rollback(&self, sp: Savepoint) -> Result<u64, StoreError> {
        (**self).rollback(sp).await
    }
    async fn get_sub_epoch_segments(
        &self,
        ses_hash: &Bytes32,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        (**self).get_sub_epoch_segments(ses_hash).await
    }
    async fn persist_sub_epoch_segments(
        &self,
        ses_hash: &Bytes32,
        bytes: &[u8],
    ) -> Result<(), StoreError> {
        (**self).persist_sub_epoch_segments(ses_hash, bytes).await
    }
    async fn build_indexes(&self) -> Result<(), StoreError> {
        (**self).build_indexes().await
    }
    async fn shed_service_indexes(&self) -> Result<(), StoreError> {
        (**self).shed_service_indexes().await
    }
}
