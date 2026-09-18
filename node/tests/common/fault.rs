// A real store with armable fault points, shared by the at-scale reorg suites (the
// `tests/reorg.rs` FaultStore precedent, lifted into common so the deep-fork/bulk-entry and
// long-reorg-scale suites inject the same crash seams without re-implementing the trait
// surface). Every method delegates to the wrapped REAL backend; exactly two seams can be armed
// to fail: `apply_block`/`apply_block_in` (the branch re-apply right after the fork rollback)
// and `set_peak`/`set_peak_in` (the pointer flip after the branch re-applied). The provided
// trait defaults that carry real backend behavior (`ensure_reorg_indexes`,
// `rolled_back_coin_states`, `get_block_records_by_hash`, `near_tip`, `shed_service_indexes`)
// are forwarded too, so a FaultStore-wrapped engine takes the identical store path as an
// unwrapped one.

use async_trait::async_trait;
use dg_xch_core::blockchain::block_record::BlockRecord;
use dg_xch_core::blockchain::coin_record::CoinRecord;
use dg_xch_core::blockchain::full_block::FullBlock;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::clvm::program::SerializedProgram;
use dg_xch_stores::{BatchHandle, BlockStatus, BlockStore, CoinStore, Savepoint, StoreError};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub struct FaultStore<S> {
    inner: S,
    fail_apply: Arc<AtomicBool>,
    fail_set_peak: Arc<AtomicBool>,
    fail_apply_at: Option<u32>,
}

impl<S> FaultStore<S> {
    pub fn new(inner: S) -> (Self, Arc<AtomicBool>, Arc<AtomicBool>) {
        let fail_apply = Arc::new(AtomicBool::new(false));
        let fail_set_peak = Arc::new(AtomicBool::new(false));
        (
            Self {
                inner,
                fail_apply: fail_apply.clone(),
                fail_set_peak: fail_set_peak.clone(),
                fail_apply_at: None,
            },
            fail_apply,
            fail_set_peak,
        )
    }

    fn injected(site: &str) -> StoreError {
        StoreError::Corrupt(format!("injected {site} fault"))
    }

    pub fn with_apply_failure_at(mut self, height: u32) -> Self {
        self.fail_apply_at = Some(height);
        self
    }
}

#[async_trait]
impl<S: CoinStore + Send + Sync> CoinStore for FaultStore<S> {
    async fn get_coin_record(&self, coin_name: &Bytes32) -> Result<Option<CoinRecord>, StoreError> {
        self.inner.get_coin_record(coin_name).await
    }
    async fn get_unspent_by_puzzle_hash(
        &self,
        ph: &Bytes32,
    ) -> Result<Vec<CoinRecord>, StoreError> {
        self.inner.get_unspent_by_puzzle_hash(ph).await
    }
    async fn get_coins_by_parent(&self, parent: &Bytes32) -> Result<Vec<CoinRecord>, StoreError> {
        self.inner.get_coins_by_parent(parent).await
    }
    async fn get_coins_added_at_height(&self, height: u32) -> Result<Vec<CoinRecord>, StoreError> {
        self.inner.get_coins_added_at_height(height).await
    }
    async fn get_coins_removed_at_height(
        &self,
        height: u32,
    ) -> Result<Vec<CoinRecord>, StoreError> {
        self.inner.get_coins_removed_at_height(height).await
    }
    async fn rolled_back_coin_states(
        &self,
        fork_height: u32,
        peak_height: u32,
    ) -> Result<Vec<CoinRecord>, StoreError> {
        self.inner
            .rolled_back_coin_states(fork_height, peak_height)
            .await
    }
    async fn get_coin_states_by_puzzle_hashes(
        &self,
        puzzle_hashes: &[Bytes32],
        min_height: u32,
        include_spent: bool,
        max_items: usize,
    ) -> Result<Vec<dg_xch_core::protocols::wallet::CoinState>, StoreError> {
        self.inner
            .get_coin_states_by_puzzle_hashes(puzzle_hashes, min_height, include_spent, max_items)
            .await
    }
    async fn batch_coin_states_by_puzzle_hashes(
        &self,
        puzzle_hashes: &[Bytes32],
        min_height: u32,
        filters: &dg_xch_core::protocols::wallet::CoinStateFilters,
        max_items: usize,
    ) -> Result<(Vec<dg_xch_core::protocols::wallet::CoinState>, Option<u32>), StoreError> {
        self.inner
            .batch_coin_states_by_puzzle_hashes(puzzle_hashes, min_height, filters, max_items)
            .await
    }
    async fn get_coin_records(&self, names: &[Bytes32]) -> Result<Vec<CoinRecord>, StoreError> {
        self.inner.get_coin_records(names).await
    }
    async fn apply_block(
        &self,
        height: u32,
        timestamp: u64,
        additions: &[CoinRecord],
        removals: &[Bytes32],
    ) -> Result<(), StoreError> {
        if self.fail_apply.load(Ordering::Relaxed) || self.fail_apply_at == Some(height) {
            return Err(Self::injected("apply_block"));
        }
        self.inner
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
        if self.fail_apply.load(Ordering::Relaxed) || self.fail_apply_at == Some(height) {
            return Err(Self::injected("apply_block_in"));
        }
        self.inner
            .apply_block_in(batch, height, timestamp, additions, removals)
            .await
    }
    async fn rollback_to(&self, fork_height: u32) -> Result<u64, StoreError> {
        self.inner.rollback_to(fork_height).await
    }
    async fn rollback_to_in(
        &self,
        batch: &mut BatchHandle,
        fork_height: u32,
    ) -> Result<u64, StoreError> {
        self.inner.rollback_to_in(batch, fork_height).await
    }
    async fn ensure_reorg_indexes(&self) -> Result<(), StoreError> {
        self.inner.ensure_reorg_indexes().await
    }
    async fn apply_hints_in(
        &self,
        batch: &mut BatchHandle,
        pairs: &[(Bytes32, Bytes32)],
    ) -> Result<(), StoreError> {
        self.inner.apply_hints_in(batch, pairs).await
    }
    async fn apply_hints(&self, pairs: &[(Bytes32, Bytes32)]) -> Result<(), StoreError> {
        self.inner.apply_hints(pairs).await
    }
    #[cfg(feature = "hint")]
    async fn get_coins_for_hint(
        &self,
        hint: &Bytes32,
        max_items: usize,
    ) -> Result<Vec<Bytes32>, StoreError> {
        self.inner.get_coins_for_hint(hint, max_items).await
    }
}

#[async_trait]
impl<S: BlockStore + Send + Sync> BlockStore for FaultStore<S> {
    async fn get_block_record(&self, hh: &Bytes32) -> Result<Option<BlockRecord>, StoreError> {
        self.inner.get_block_record(hh).await
    }
    async fn get_block_records_by_hash(
        &self,
        hashes: &[Bytes32],
    ) -> Result<Vec<BlockRecord>, StoreError> {
        self.inner.get_block_records_by_hash(hashes).await
    }
    async fn get_block_record_by_height(&self, h: u32) -> Result<Option<BlockRecord>, StoreError> {
        self.inner.get_block_record_by_height(h).await
    }
    async fn get_peak(&self) -> Result<Option<(Bytes32, u32)>, StoreError> {
        self.inner.get_peak().await
    }
    async fn min_record_height(&self) -> Result<Option<u32>, StoreError> {
        self.inner.min_record_height().await
    }
    async fn get_block(&self, hh: &Bytes32) -> Result<Option<FullBlock>, StoreError> {
        self.inner.get_block(hh).await
    }
    async fn get_generator_at_height(
        &self,
        h: u32,
    ) -> Result<Option<SerializedProgram>, StoreError> {
        self.inner.get_generator_at_height(h).await
    }
    async fn add_block_records(&self, records: &[BlockRecord]) -> Result<(), StoreError> {
        self.inner.add_block_records(records).await
    }
    async fn add_block_records_in(
        &self,
        batch: &mut BatchHandle,
        records: &[BlockRecord],
    ) -> Result<(), StoreError> {
        self.inner.add_block_records_in(batch, records).await
    }
    async fn begin(&self) -> Result<BatchHandle, StoreError> {
        self.inner.begin().await
    }
    async fn append_many(
        &self,
        batch: &mut BatchHandle,
        blocks: &[FullBlock],
    ) -> Result<(), StoreError> {
        self.inner.append_many(batch, blocks).await
    }
    async fn commit(&self, batch: BatchHandle) -> Result<(), StoreError> {
        self.inner.commit(batch).await
    }
    async fn get_unassociated(&self, limit: usize) -> Result<Vec<u32>, StoreError> {
        self.inner.get_unassociated(limit).await
    }
    async fn set_peak(&self, new_peak: &Bytes32) -> Result<u64, StoreError> {
        if self.fail_set_peak.load(Ordering::Relaxed) {
            return Err(Self::injected("set_peak"));
        }
        self.inner.set_peak(new_peak).await
    }
    async fn set_peak_in(
        &self,
        batch: &mut BatchHandle,
        new_peak: &Bytes32,
    ) -> Result<u64, StoreError> {
        if self.fail_set_peak.load(Ordering::Relaxed) {
            return Err(Self::injected("set_peak_in"));
        }
        self.inner.set_peak_in(batch, new_peak).await
    }
    fn near_tip(&self) -> bool {
        self.inner.near_tip()
    }
    fn set_near_tip(&self, near_tip: bool) {
        self.inner.set_near_tip(near_tip);
    }
    async fn get_status(&self, hh: &Bytes32) -> Result<BlockStatus, StoreError> {
        self.inner.get_status(hh).await
    }
    async fn set_status(&self, hh: &Bytes32, s: BlockStatus) -> Result<(), StoreError> {
        self.inner.set_status(hh, s).await
    }
    async fn set_status_in(
        &self,
        batch: &mut BatchHandle,
        hh: &Bytes32,
        s: BlockStatus,
    ) -> Result<(), StoreError> {
        self.inner.set_status_in(batch, hh, s).await
    }
    async fn savepoint(&self) -> Result<Savepoint, StoreError> {
        self.inner.savepoint().await
    }
    async fn rollback(&self, sp: Savepoint) -> Result<u64, StoreError> {
        self.inner.rollback(sp).await
    }
    async fn get_sub_epoch_segments(
        &self,
        ses_hash: &Bytes32,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        self.inner.get_sub_epoch_segments(ses_hash).await
    }
    async fn persist_sub_epoch_segments(
        &self,
        ses_hash: &Bytes32,
        bytes: &[u8],
    ) -> Result<(), StoreError> {
        self.inner.persist_sub_epoch_segments(ses_hash, bytes).await
    }
    async fn build_indexes(&self) -> Result<(), StoreError> {
        self.inner.build_indexes().await
    }
    async fn shed_service_indexes(&self) -> Result<(), StoreError> {
        self.inner.shed_service_indexes().await
    }
}
