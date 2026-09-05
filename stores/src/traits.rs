use crate::error::StoreError;
use crate::types::{BatchHandle, BlockStatus, Savepoint};
use async_trait::async_trait;
use dg_xch_core::blockchain::block_record::BlockRecord;
use dg_xch_core::blockchain::coin_record::{CoinRecord, UnspentLineageInfo};
use dg_xch_core::blockchain::full_block::FullBlock;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::clvm::program::SerializedProgram;
use dg_xch_core::protocols::wallet::CoinState;
#[cfg(feature = "coin-index")]
use dg_xch_core::protocols::wallet::CoinStateFilters;

/// The unspent/spent coin set. Point-gets and one-atomic-batch-per-block state transitions; reorg reverts by
/// streamed range delete/update returning a count, never a materialized changed-coin set.
#[async_trait]
pub trait CoinStore {
    async fn apply_coin_window_in(
        &self,
        batch: &mut BatchHandle,
        changes: &[crate::types::CoinChanges<'_>],
    ) -> Result<(), StoreError> {
        for change in changes {
            self.apply_block_in(
                batch,
                change.height,
                change.timestamp,
                change.additions,
                change.removals,
            )
            .await?;
            self.apply_hints_in(batch, change.hints).await?;
        }
        Ok(())
    }

    /// Point-get a coin record by coin id.
    ///
    /// # Errors
    /// Returns [`StoreError::Backend`] on a query failure.
    async fn get_coin_record(&self, coin_name: &Bytes32) -> Result<Option<CoinRecord>, StoreError>;

    /// Batched multi-get by coin id.
    ///
    /// # Errors
    /// Returns [`StoreError::Backend`] on a query failure.
    async fn get_coin_records(&self, names: &[Bytes32]) -> Result<Vec<CoinRecord>, StoreError>;

    /// Apply one validated block's additions + removals as one atomic batch: `removals` set
    /// `spent_index = height`; `additions` insert unspent.
    ///
    /// # Errors
    /// Returns [`StoreError::Backend`] if the batch fails to commit.
    async fn apply_block(
        &self,
        height: u32,
        timestamp: u64,
        additions: &[CoinRecord],
        removals: &[Bytes32],
    ) -> Result<(), StoreError>;

    /// [`CoinStore::apply_block`] executed inside an open write batch: the coin deltas become durable
    /// with everything else in the batch on its single `commit` (the one-fsync-per-block apply path).
    ///
    /// # Errors
    /// Returns [`StoreError::Corrupt`] if the batch was opened by a different backend, otherwise as
    /// [`CoinStore::apply_block`].
    async fn apply_block_in(
        &self,
        batch: &mut BatchHandle,
        height: u32,
        timestamp: u64,
        additions: &[CoinRecord],
        removals: &[Bytes32],
    ) -> Result<(), StoreError>;

    /// Revert every coin change strictly above `fork_height` (delete additions, un-spend removals). Streamed:
    /// returns the count reverted, not a materialized set (RAM O(1)).
    ///
    /// # Errors
    /// Returns [`StoreError::Backend`] if the reorg batch fails.
    async fn rollback_to(&self, fork_height: u32) -> Result<u64, StoreError>;

    /// [`CoinStore::rollback_to`] executed inside an OPEN write batch — the first statement of the
    /// engine's single-transaction reorg. The whole reorg (rollback, per-block coin re-applies,
    /// main-chain flips and the peak) must commit as one transaction, so that a crash anywhere
    /// means the reorg never happened. Committing the rollback on its own leaves a crash window
    /// where coins are reverted above the fork while the peak still points at the old branch.
    ///
    /// Required (not a provided default) so the engine's reorg future can call it without an
    /// `async_trait` `Self: Sync` bound leaking onto every `Engine<S, P>` method (the
    /// [`CoinStore::apply_hints_in`] precedent).
    ///
    /// # Errors
    /// Returns [`StoreError::Corrupt`] if the batch was opened by a different backend, otherwise as
    /// [`CoinStore::rollback_to`].
    async fn rollback_to_in(
        &self,
        batch: &mut BatchHandle,
        fork_height: u32,
    ) -> Result<u64, StoreError>;

    /// Ensure the reorg-speed coin indexes (`confirmed_index`, `spent_index`) exist BEFORE a reorg
    /// runs its rollback / rolled-back-state range queries. These back
    /// [`CoinStore::rollback_to`]'s `confirmed_index > $1` / `spent_index > $1` predicates and the
    /// [`CoinStore::rolled_back_coin_states`] per-height `= $1` lookups; without them the reorg
    /// seq-scans the ENTIRE coin table.
    ///
    /// The SQL backends otherwise DEFER these indexes to [`BlockStore::build_indexes`] at the
    /// sync->tip transition (they are pure write-amplification during forward-only bulk sync, which
    /// never reads them). But a reorg can land BELOW tip — a node stuck on a minority equal-weight
    /// tie-break branch MUST reorg to rejoin the heavier chain, and that reorg happens long before
    /// `build_indexes` would fire — so the reorg path ensures them here, idempotently. Rollback
    /// therefore works at any sync depth without paying the write-amp on the forward path. Called
    /// once at the top of the engine's reorg; a cheap catalog check once the index exists.
    ///
    /// Default: no-op — a backend with its own by-height structures (the mmap store) needs nothing.
    ///
    /// # Errors
    /// Returns [`StoreError::Backend`] on a DDL failure.
    async fn ensure_reorg_indexes(&self) -> Result<(), StoreError>
    where
        Self: Sync,
    {
        Ok(())
    }

    #[cfg(feature = "coin-index")]
    /// # Errors
    /// Returns [`StoreError::Backend`] on a query failure.
    async fn get_unspent_by_puzzle_hash(&self, ph: &Bytes32)
    -> Result<Vec<CoinRecord>, StoreError>;

    /// The newest unspent version of the singleton whose full puzzle hash is `puzzle_hash`: the
    /// SINGLE unspent coin with this puzzle hash whose parent is SPENT and shares the same puzzle
    /// hash and amount. Anything else (zero candidates, several, an unspent parent, a launcher
    /// parent with a different hash) is `None`, and the spend falls back to non-FF treatment.
    ///
    /// The answer is derived live from the unspent-by-puzzle-hash index plus the parent records,
    /// so reorgs need no marker maintenance. Provided (not required): without the `coin-index`
    /// tier there is no puzzle-hash index and every FF spend degrades to a normal spend, which is
    /// the conservative outcome.
    ///
    /// # Errors
    /// Returns [`StoreError::Backend`] on a query failure.
    async fn get_unspent_lineage_info(
        &self,
        puzzle_hash: &Bytes32,
    ) -> Result<Option<UnspentLineageInfo>, StoreError>
    where
        Self: Sync,
    {
        #[cfg(feature = "coin-index")]
        {
            let candidates = self.get_unspent_by_puzzle_hash(puzzle_hash).await?;
            // A well-formed singleton has at most one unspent version; a hot (non-singleton)
            // puzzle hash with many unspent coins can never qualify — bail before N parent
            // fetches.
            if candidates.is_empty() || candidates.len() > 32 {
                return Ok(None);
            }
            let mut matches: Vec<UnspentLineageInfo> = Vec::new();
            for candidate in &candidates {
                let Some(parent) = self
                    .get_coin_record(&candidate.coin.parent_coin_info)
                    .await?
                else {
                    continue;
                };
                if parent.spent
                    && parent.coin.puzzle_hash == *puzzle_hash
                    && parent.coin.amount == candidate.coin.amount
                {
                    matches.push(UnspentLineageInfo {
                        coin_id: candidate.coin.name(),
                        parent_id: parent.coin.name(),
                        parent_parent_id: parent.coin.parent_coin_info,
                    });
                }
            }
            if matches.len() == 1 {
                return Ok(matches.pop());
            }
            Ok(None)
        }
        #[cfg(not(feature = "coin-index"))]
        {
            let _ = puzzle_hash;
            Ok(None)
        }
    }

    #[cfg(feature = "coin-index")]
    /// # Errors
    /// Returns [`StoreError::Backend`] on a query failure.
    async fn get_coins_by_parent(&self, parent: &Bytes32) -> Result<Vec<CoinRecord>, StoreError>;

    /// Coins CREATED at `height` — the additions half of `get_additions_and_removals`. Reads the `confirmed_index` secondary index (built at the
    /// sync→tip transition); a pure validating node without it falls back to a table scan.
    ///
    /// # Errors
    /// Returns [`StoreError::Backend`] on a query failure.
    #[cfg(feature = "coin-index")]
    async fn get_coins_added_at_height(&self, height: u32) -> Result<Vec<CoinRecord>, StoreError>;

    /// Coins SPENT at `height` — the removals half of `get_additions_and_removals`. Reads the
    /// `spent_index` secondary index.
    ///
    /// # Errors
    /// Returns [`StoreError::Backend`] on a query failure.
    #[cfg(feature = "coin-index")]
    async fn get_coins_removed_at_height(&self, height: u32)
    -> Result<Vec<CoinRecord>, StoreError>;

    /// The wallet-visible coin states a rollback to `fork_height` would surface. Computed from the
    /// STILL-CURRENT store, so it MUST be called BEFORE the matching
    /// [`CoinStore::rollback_to_in`]. For every height in `fork_height+1..=peak_height`: a coin
    /// CONFIRMED there reverts to not-on-chain (`confirmed_block_index = 0`, `timestamp = 0`,
    /// carrying its spent index); a coin SPENT there — and not already collected as
    /// created-above-the-fork — reverts to unspent (`spent_block_index = 0`). The engine threads
    /// these to wallet subscribers as the rolled-back records of the reorg. Provided (not
    /// required): without the `coin-index` tier there is no by-height index, so this returns empty
    /// and the reorg wallet push carries only the new branch.
    ///
    /// # Errors
    /// Returns [`StoreError::Backend`] on a query failure.
    async fn rolled_back_coin_states(
        &self,
        fork_height: u32,
        peak_height: u32,
    ) -> Result<Vec<CoinRecord>, StoreError>
    where
        Self: Sync,
    {
        #[cfg(feature = "coin-index")]
        {
            use std::collections::HashSet;
            let mut out: Vec<CoinRecord> = Vec::new();
            let mut seen: HashSet<Bytes32> = HashSet::new();
            for h in (fork_height + 1)..=peak_height {
                for rec in self.get_coins_added_at_height(h).await? {
                    if seen.insert(rec.coin.name()) {
                        out.push(CoinRecord {
                            coin: rec.coin,
                            confirmed_block_index: 0,
                            spent_block_index: rec.spent_block_index,
                            coinbase: rec.coinbase,
                            timestamp: 0,
                            spent: rec.spent,
                        });
                    }
                }
                for rec in self.get_coins_removed_at_height(h).await? {
                    if seen.insert(rec.coin.name()) {
                        out.push(CoinRecord {
                            coin: rec.coin,
                            confirmed_block_index: rec.confirmed_block_index,
                            spent_block_index: 0,
                            coinbase: rec.coinbase,
                            timestamp: rec.timestamp,
                            spent: false,
                        });
                    }
                }
            }
            Ok(out)
        }
        #[cfg(not(feature = "coin-index"))]
        {
            let _ = (fork_height, peak_height);
            Ok(Vec::new())
        }
    }

    /// Write one block's create-coin hints `(hint, coin_id)` into an OPEN write batch, so the
    /// `coin_hint` index becomes durable in the SAME transaction as the block's coin deltas. A
    /// no-op in a build without the `hint` service tier.
    ///
    /// Required (not a provided default) so the engine's confirm/reorg futures can call it without
    /// an `async_trait` `Self: Sync` bound leaking onto every `Engine<S, P>` method.
    ///
    /// # Errors
    /// Returns [`StoreError`] if the batch write fails.
    async fn apply_hints_in(
        &self,
        batch: &mut BatchHandle,
        pairs: &[(Bytes32, Bytes32)],
    ) -> Result<(), StoreError>;

    /// [`CoinStore::apply_hints_in`] in its own transaction — the standalone variant for callers
    /// outside an open batch (the engine's apply AND reorg paths both write hints through
    /// [`CoinStore::apply_hints_in`], inside the block's / the reorg's single batch). A no-op in a
    /// build without the `hint` tier.
    ///
    /// # Errors
    /// Returns [`StoreError`] if the write fails.
    async fn apply_hints(&self, pairs: &[(Bytes32, Bytes32)]) -> Result<(), StoreError>;

    /// Coin ids a 32-byte hint points at, at most `max_items` of them — the primitive the
    /// wallet/explorer hint lookup and the light-wallet p2p path both read. The register
    /// initial-state path passes its REMAINING `max_subscribe_response_items` budget, so a
    /// dust-storm hint cannot materialize an unbounded id list; other callers pass
    /// [`MAX_COIN_STATES`]. The limit lives IN the query (`LIMIT` / scan cut-off), never
    /// fetch-then-truncate.
    ///
    /// # Errors
    /// Returns [`StoreError::Backend`] on a query failure.
    #[cfg(feature = "hint")]
    async fn get_coins_for_hint(
        &self,
        hint: &Bytes32,
        max_items: usize,
    ) -> Result<Vec<Bytes32>, StoreError>;

    /// Coin records a hint points at (unspent-only by default, `include_spent = false`).
    /// A provided composition over [`CoinStore::get_coins_for_hint`] +
    /// [`CoinStore::get_coin_records`], so every backend (and the light-wallet p2p handler in #2)
    /// shares one query shape.
    ///
    /// # Errors
    /// Returns [`StoreError::Backend`] on a query failure.
    #[cfg(feature = "hint")]
    async fn get_coin_records_by_hint(
        &self,
        hint: &Bytes32,
        include_spent: bool,
    ) -> Result<Vec<CoinRecord>, StoreError> {
        let names = self.get_coins_for_hint(hint, MAX_COIN_STATES).await?;
        let records = self.get_coin_records(&names).await?;
        Ok(if include_spent {
            records
        } else {
            records.into_iter().filter(|cr| !cr.spent).collect()
        })
    }

    /// Spent AND unspent coin states whose puzzle hash is in `puzzle_hashes` and which were created
    /// OR spent at height `>= min_height`. `include_spent = false` filters to unspent
    /// (`spent_index <= 0`). This is the light-wallet subscription initial-state read: a wallet
    /// needs full history (spent + unspent) from its birth height to reconstruct its transactions,
    /// so unspent-only would be a broken answer. Bounded to `max_items` rows (store default
    /// [`MAX_COIN_STATES`]); the register path passes its `max_subscribe_response_items` budget.
    /// The limit lives IN the query (a running `LIMIT` budget / scan cut-off), never
    /// fetch-then-truncate. Reads the `puzzle_hash` secondary index (service tier).
    ///
    /// # Errors
    /// Returns [`StoreError::Backend`] on a query failure.
    #[cfg(feature = "coin-index")]
    async fn get_coin_states_by_puzzle_hashes(
        &self,
        puzzle_hashes: &[Bytes32],
        min_height: u32,
        include_spent: bool,
        max_items: usize,
    ) -> Result<Vec<CoinState>, StoreError>;

    /// Spent AND unspent coin states for `coin_ids`, created OR spent at height `>= min_height`.
    /// A provided composition over [`CoinStore::get_coin_records`] (point-gets by id, available on
    /// every backend without the service tier), so the coin-id subscription initial-state and the
    /// puzzle-hash hint join share one query shape. Bounded to `max_items` — the register path
    /// passes its `max_subscribe_response_items` budget, other callers [`MAX_COIN_STATES`].
    ///
    /// # Errors
    /// Returns [`StoreError::Backend`] on a query failure.
    async fn get_coin_states_by_ids(
        &self,
        coin_ids: &[Bytes32],
        min_height: u32,
        include_spent: bool,
        max_items: usize,
    ) -> Result<Vec<CoinState>, StoreError> {
        let records = self.get_coin_records(coin_ids).await?;
        Ok(records
            .into_iter()
            .filter(|cr| {
                (cr.confirmed_block_index >= min_height || cr.spent_block_index >= min_height)
                    && (include_spent || cr.spent_block_index == 0)
            })
            .take(max_items)
            .map(|cr| coin_state_from_record(&cr))
            .collect())
    }

    /// The PAGED wallet-sync read behind `RequestPuzzleState`: coin states whose puzzle hash is in
    /// `puzzle_hashes` (plus, under `filters.include_hinted`, coins a HINT equal to one of the
    /// hashes points at — the CAT/NFT discovery join), created OR spent at height `>= min_height`,
    /// filtered by `include_spent`/`include_unspent` (`spent_index > 0` / `spent_index <= 0`) and
    /// `min_amount`, ordered ascending by `max(created, spent)` height.
    ///
    /// Returns `(coin_states, next_min_height)`: `None` = finished (everything fit in
    /// `max_items`); `Some(h)` = the next page starts at `min_height = h`, and NO state from
    /// height `h` itself is included in this page, so one block's states are never split across
    /// pages (the client's is_finished/height contract). Both filters false short-circuits to
    /// `([], None)`. Backends fetch at most `max_items + 1` ordered rows per puzzle-hash/hint probe
    /// and keep only the smallest `max_items + 1` by height while merging (bounded memory; the SQL
    /// `LIMIT`/scan cut-off is in the query, never fetch-then-truncate).
    ///
    /// # Errors
    /// Returns [`StoreError::Backend`] on a query failure.
    #[cfg(feature = "coin-index")]
    async fn batch_coin_states_by_puzzle_hashes(
        &self,
        puzzle_hashes: &[Bytes32],
        min_height: u32,
        filters: &CoinStateFilters,
        max_items: usize,
    ) -> Result<(Vec<CoinState>, Option<u32>), StoreError>;
}

/// The wallet-subscription result cap (`max_items`): a single puzzle-hash / coin-id initial-state
/// read cannot return an unbounded set.
pub const MAX_COIN_STATES: usize = 50_000;

/// The most puzzle hashes one `batch_coin_states_by_puzzle_hashes` call may carry: the SQLite
/// bound-variable ceiling (32 700 on any modern build) less headroom for the query's own
/// parameters. The `RequestPuzzleState` handler truncates the request list to this before querying.
pub const MAX_PUZZLE_HASH_BATCH_SIZE: usize = 32_700 - 10;

/// The paging sort key: a coin state's activity height, `max(created_height or 0,
/// spent_height or 0)`.
#[cfg(feature = "coin-index")]
#[must_use]
pub(crate) fn coin_state_height(cs: &CoinState) -> u32 {
    cs.created_height
        .unwrap_or(0)
        .max(cs.spent_height.unwrap_or(0))
}

/// Merge one probe's rows into the dedup map (keyed by coin id) and
/// trim it back to the smallest `cap` states by (height, coin id) whenever it overflows —
/// bounded-memory across an arbitrary number of per-hash probes. Dropping the largest is safe:
/// the final page keeps only the smallest `cap` overall, and a dropped state can only ever
/// re-enter BEHIND states that are still resident, where it is dropped again.
#[cfg(feature = "coin-index")]
pub(crate) fn merge_coin_states_bounded(
    merged: &mut std::collections::HashMap<Bytes32, CoinState>,
    states: impl IntoIterator<Item = CoinState>,
    cap: usize,
) {
    for cs in states {
        merged.insert(cs.coin.name(), cs);
    }
    if merged.len() > cap {
        use dg_xch_core::traits::SizedBytes;
        let mut keys: Vec<(u32, [u8; 32], Bytes32)> = merged
            .iter()
            .map(|(name, cs)| (coin_state_height(cs), name.bytes(), *name))
            .collect();
        keys.sort_unstable_by_key(|k| (k.0, k.1));
        for (_, _, name) in keys.drain(cap..) {
            merged.remove(&name);
        }
    }
}

/// Order the merged states and apply the page cut: ascending by activity height, ties broken by
/// coin id so the wire answer is deterministic (within-height order is not part of the protocol,
/// and heights are never split across pages, so the page SET is unaffected). `<= max_items` ⇒
/// finished (`None`); otherwise pop the `max_items + 1`-th state as the next page's floor and drop
/// every trailing state at that same height, so no block is split.
#[cfg(feature = "coin-index")]
#[must_use]
pub(crate) fn page_coin_states(
    merged: std::collections::HashMap<Bytes32, CoinState>,
    max_items: usize,
) -> (Vec<CoinState>, Option<u32>) {
    use dg_xch_core::traits::SizedBytes;
    let mut states: Vec<CoinState> = merged.into_values().collect();
    states.sort_unstable_by_key(|cs| (coin_state_height(cs), cs.coin.name().bytes()));
    states.truncate(max_items + 1);
    if states.len() <= max_items {
        return (states, None);
    }
    let next = states.pop().expect("len > max_items >= 0");
    let next_height = coin_state_height(&next);
    while states
        .last()
        .is_some_and(|last| coin_state_height(last) == next_height)
    {
        states.pop();
    }
    (states, Some(next_height))
}

/// Project a [`CoinRecord`] to the wallet-protocol [`CoinState`]: the coin, its created height
/// (always set for a stored coin), and its spent height (`None` while unspent).
#[must_use]
pub(crate) fn coin_state_from_record(cr: &CoinRecord) -> CoinState {
    CoinState {
        coin: cr.coin,
        created_height: Some(cr.confirmed_block_index),
        spent_height: (cr.spent_block_index != 0).then_some(cr.spent_block_index),
    }
}

/// The block archive: a narrow hot `block_record` split from a cold zstd `block_body`, with out-of-order
/// body bulk-append, a candidate-heights-lacking-a-body query, durable per-block status, and reorg by
/// confirmation-pointer flip. Point reads are lock-free during an open write batch (WAL snapshot).
#[async_trait]
pub trait BlockStore {
    async fn prepare_archive(
        &self,
        records: Vec<(BlockRecord, BlockStatus)>,
        blocks: Vec<FullBlock>,
    ) -> Result<crate::types::PreparedArchive, StoreError> {
        Ok(crate::types::PreparedArchive::Native { records, blocks })
    }

    async fn persist_prepared_archive_in(
        &self,
        batch: &mut BatchHandle,
        prepared: crate::types::PreparedArchive,
    ) -> Result<(), StoreError> {
        let crate::types::PreparedArchive::Native { records, blocks } = prepared else {
            return Err(StoreError::Batch(
                "archive prepared by another backend".into(),
            ));
        };
        let plain: Vec<_> = records.iter().map(|(record, _)| record.clone()).collect();
        self.add_block_records_in(batch, &plain).await?;
        self.append_many(batch, &blocks).await?;
        for status in [
            BlockStatus::Bypass,
            BlockStatus::Validated,
            BlockStatus::Unvalidated,
        ] {
            let hashes: Vec<_> = records
                .iter()
                .filter(|(_, value)| *value == status)
                .map(|(record, _)| record.header_hash)
                .collect();
            self.set_status_many_in(batch, &hashes, status).await?;
        }
        Ok(())
    }

    /// # Errors
    /// Returns [`StoreError`] on a query or decode failure.
    async fn get_block_record(&self, hh: &Bytes32) -> Result<Option<BlockRecord>, StoreError>;

    /// # Errors
    /// Returns [`StoreError`] on a query or decode failure.
    async fn get_block_record_by_height(&self, h: u32) -> Result<Option<BlockRecord>, StoreError>;

    /// Multi-get: the records for `hashes` (absent hashes are simply missing from the result; no
    /// order guarantee) — the fork-walk / wallet batch read. The staging preload fetches a whole
    /// sync window's candidate records through this in one call instead of one point read per
    /// staged block. Default:
    /// per-hash point reads (semantics-exact for every backend); backends override with a
    /// single-round-trip batch.
    ///
    /// # Errors
    /// Returns [`StoreError`] on a query or decode failure.
    async fn get_block_records_by_hash(
        &self,
        hashes: &[Bytes32],
    ) -> Result<Vec<BlockRecord>, StoreError> {
        let mut out = Vec::with_capacity(hashes.len());
        for hh in hashes {
            if let Some(r) = self.get_block_record(hh).await? {
                out.push(r);
            }
        }
        Ok(out)
    }

    /// # Errors
    /// Returns [`StoreError::Backend`] on a query failure.
    async fn get_peak(&self) -> Result<Option<(Bytes32, u32)>, StoreError>;

    async fn min_record_height(&self) -> Result<Option<u32>, StoreError>;

    /// Decompress the cold body back to the exact `FullBlock`.
    ///
    /// # Errors
    /// Returns [`StoreError`] on a query, decompress, or decode failure.
    async fn get_block(&self, hh: &Bytes32) -> Result<Option<FullBlock>, StoreError>;

    /// Header-first path: upsert narrow `block_record` rows (no body). A full `BlockRecord` cannot be
    /// derived from a `FullBlock` without consensus, and `get_unassociated` needs records to exist before
    /// bodies — so records are written here and bodies stream in later out of order.
    ///
    /// # Errors
    /// Returns [`StoreError`] if a record fails to serialize or the batch fails.
    async fn add_block_records(&self, records: &[BlockRecord]) -> Result<(), StoreError>;

    /// [`BlockStore::add_block_records`] executed inside an open write batch (one commit = one fsync
    /// for the whole per-block apply).
    ///
    /// # Errors
    /// Returns [`StoreError::Corrupt`] if the batch was opened by a different backend, otherwise as
    /// [`BlockStore::add_block_records`].
    async fn add_block_records_in(
        &self,
        batch: &mut BatchHandle,
        records: &[BlockRecord],
    ) -> Result<(), StoreError>;

    /// Open a batch; bodies appended before `commit` are durable together on commit (one fsync).
    ///
    /// # Errors
    /// Returns [`StoreError::Backend`] if the batch cannot begin.
    async fn begin(&self) -> Result<BatchHandle, StoreError>;

    /// Out-of-order write-through of bodies, keyed by each block's own header hash.
    ///
    /// # Errors
    /// Returns [`StoreError`] if a body fails to serialize, compress, or write.
    async fn append_many(
        &self,
        batch: &mut BatchHandle,
        blocks: &[FullBlock],
    ) -> Result<(), StoreError>;

    /// # Errors
    /// Returns [`StoreError::Backend`] if the commit fails.
    async fn commit(&self, batch: BatchHandle) -> Result<(), StoreError>;

    /// Whether the confirm pipeline should run in NEAR-TIP mode -- per-block commits plus an active
    /// WAL checkpointer -- rather than CATCH-UP mode -- one big batch commit per window with the
    /// checkpointer quiet. Default catch-up (false). Only the WAL backend (sqlite) acts on this; the
    /// Postgres and mmap backends have no WAL checkpointer and ignore it.
    fn near_tip(&self) -> bool {
        false
    }

    /// Set the near-tip phase (see [`Self::near_tip`]). The follow driver sets it from the
    /// near-tip-band signal: false while bulk-catching-up, true once within a few blocks of the tip.
    fn set_near_tip(&self, _near_tip: bool) {}

    /// The backend's recorded [`StoreTelemetry`] (phase-labelled commit latency, WAL checkpoint
    /// activity), for the `/metrics` responder to render. `None` (the default) for backends that do
    /// not record it — the renderer then skips the store series rather than exporting zeros that
    /// would read as "commits observed, all instant".
    fn telemetry(&self) -> Option<std::sync::Arc<crate::telemetry::StoreTelemetry>> {
        None
    }

    /// Current size in bytes of the backend's write-ahead-log file (0 for backends without one).
    /// The bounded-WAL witness: the number otherwise watched by hand by listing the `-wal` file size.
    fn wal_bytes(&self) -> u64 {
        0
    }

    /// Next N candidate heights that have a record but no body yet, ordered, limited — the reservation
    /// window feed.
    ///
    /// # Errors
    /// Returns [`StoreError::Backend`] on a query failure.
    async fn get_unassociated(&self, limit: usize) -> Result<Vec<u32>, StoreError>;

    /// Flip confirmation pointers to make `new_peak` the confirmed tip; returns links touched.
    ///
    /// # Errors
    /// Returns [`StoreError`] if the walk hits a missing record or the batch fails.
    async fn set_peak(&self, new_peak: &Bytes32) -> Result<u64, StoreError>;

    /// [`BlockStore::set_peak`] executed inside an open write batch — the pointer flip becomes durable
    /// with the rest of the block's writes on the batch's single commit.
    ///
    /// # Errors
    /// Returns [`StoreError::Corrupt`] if the batch was opened by a different backend, otherwise as
    /// [`BlockStore::set_peak`].
    async fn set_peak_in(
        &self,
        batch: &mut BatchHandle,
        new_peak: &Bytes32,
    ) -> Result<u64, StoreError>;

    /// Mark an already-validated plain extension as the main chain and move the peak inside an
    /// open batch. The engine calls this only after proving that every hash extends the current
    /// peak in the supplied order. Backends may override this to avoid rediscovering that ancestry;
    /// the fallback preserves the ordinary fork-aware walk.
    ///
    /// # Errors
    /// As [`BlockStore::set_peak_in`].
    async fn extend_peak_in(
        &self,
        batch: &mut BatchHandle,
        extension: &[Bytes32],
        new_height: u32,
    ) -> Result<u64, StoreError> {
        let Some(new_peak) = extension.last() else {
            return Ok(0);
        };
        let _ = new_height;
        self.set_peak_in(batch, new_peak).await
    }

    /// # Errors
    /// Returns [`StoreError::Backend`] on a query failure.
    async fn get_status(&self, hh: &Bytes32) -> Result<BlockStatus, StoreError>;

    /// # Errors
    /// Returns [`StoreError::Backend`] on a query failure.
    async fn set_status(&self, hh: &Bytes32, s: BlockStatus) -> Result<(), StoreError>;

    /// [`BlockStore::set_status`] executed inside an open write batch.
    ///
    /// # Errors
    /// Returns [`StoreError::Corrupt`] if the batch was opened by a different backend, otherwise as
    /// [`BlockStore::set_status`].
    async fn set_status_in(
        &self,
        batch: &mut BatchHandle,
        hh: &Bytes32,
        s: BlockStatus,
    ) -> Result<(), StoreError>;

    /// Set one status for several records inside an open batch. SQL backends override this to
    /// issue a bounded set update; the fallback preserves compatibility for custom stores.
    ///
    /// # Errors
    /// As [`BlockStore::set_status_in`].
    async fn set_status_many_in(
        &self,
        batch: &mut BatchHandle,
        hashes: &[Bytes32],
        status: BlockStatus,
    ) -> Result<(), StoreError> {
        for hash in hashes {
            self.set_status_in(batch, hash, status).await?;
        }
        Ok(())
    }

    /// # Errors
    /// Returns [`StoreError::Backend`] on a query failure.
    async fn savepoint(&self) -> Result<Savepoint, StoreError>;

    /// Streamed rollback to a savepoint: returns links touched, not a full changed dict (RAM O(1)).
    ///
    /// # Errors
    /// Returns [`StoreError::Backend`] if the reorg batch fails.
    async fn rollback(&self, sp: Savepoint) -> Result<u64, StoreError>;

    /// Resolve a confirmed prior block's `transactions_generator` by height — the storage side of
    /// block-level generator back-reference compression; only main-chain blocks qualify. `None`
    /// when no confirmed block occupies the height or that block carries no generator.
    ///
    /// # Errors
    /// Returns [`StoreError`] on a query, decompress, or decode failure.
    async fn get_generator_at_height(
        &self,
        h: u32,
    ) -> Result<Option<SerializedProgram>, StoreError>;

    /// Persisted weight-proof challenge segments for one sub-epoch, keyed by the ses-carrying
    /// block's header hash, over the two-column `sub_epoch_segments_v3` table. The value is the
    /// opaque `ChiaSerialize` bytes of a `SubEpochSegments` wrapper; encode/decode belongs to the
    /// weight-proof builder, not the store.
    ///
    /// # Errors
    /// Returns [`StoreError::Backend`] on a query failure.
    async fn get_sub_epoch_segments(
        &self,
        ses_hash: &Bytes32,
    ) -> Result<Option<Vec<u8>>, StoreError>;

    /// Upsert the persisted segment bytes for `ses_hash`. Built once per sampled sub-epoch, read on
    /// every later weight-proof build (the segments below a served tip never change).
    ///
    /// # Errors
    /// Returns [`StoreError`] if the write fails.
    async fn persist_sub_epoch_segments(
        &self,
        ses_hash: &Bytes32,
        bytes: &[u8],
    ) -> Result<(), StoreError>;

    /// Deferred bulk secondary-index build, run once after the sync firehose (not per-insert).
    ///
    /// # Errors
    /// Returns [`StoreError::Backend`] if index creation fails.
    async fn build_indexes(&self) -> Result<(), StoreError>;

    /// Drop the deferred secondary `coin_record` indexes for a deep re-catch-up — the falling
    /// edge of the [`Self::build_indexes`] rising edge.
    ///
    /// A node that reached tip (built the full index set) and then fell far behind re-applies
    /// settled history with every secondary index still present: each coin insert maintains
    /// them all, and each spend-update is non-HOT because `spent_index` is indexed, so the
    /// confirm window degenerates into index/heap random reads.
    /// Deep catch-up is applying canonical, settled blocks — reorgs are a tip phenomenon — so
    /// the service tier (`puzzle_hash`, `coin_parent`, `unspent_by_ph`, the `coin_hint`
    /// secondary) AND the `spent_index` reorg btree are all safe to shed: shedding
    /// `spent_index` is what re-enables HOT spend-updates. If a reorg is nonetheless requested
    /// while shed, [`CoinStore::ensure_reorg_indexes`] rebuilds the reorg tier on demand before
    /// the rollback runs (slower, correct). The rising edge (`build_indexes`) restores
    /// everything before the node re-enters the reorg-exposed tip zone.
    ///
    /// Idempotent (`DROP INDEX IF EXISTS`); a shed interrupted mid-way leaves a subset absent,
    /// which the `IF NOT EXISTS` rebuild handles. Consensus indexes (the `coin_record` pkey,
    /// `block_record` pkey/height, `coin_hint` pkey) are never touched.
    ///
    /// Default: no-op — a backend with no deferred SQL indexes (the mmap store) sheds nothing.
    ///
    /// # Errors
    /// Returns [`StoreError::Backend`] if an index drop fails.
    async fn shed_service_indexes(&self) -> Result<(), StoreError> {
        Ok(())
    }
}
