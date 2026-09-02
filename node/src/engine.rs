use crate::cache::BlockRecordCache;
use crate::error::NodeError;
use crate::primitives::ConsensusPrimitives;
use dg_xch_core::blockchain::block_record::BlockRecord;
use dg_xch_core::blockchain::coin::Coin;
use dg_xch_core::blockchain::coin_record::CoinRecord;
use dg_xch_core::blockchain::end_of_subslot_bundle::EndOfSubSlotBundle;
use dg_xch_core::blockchain::full_block::FullBlock;
use dg_xch_core::blockchain::header_block::HeaderBlock;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::blockchain::spend_bundle_conditions::SpendBundleConditions;
use dg_xch_core::blockchain::subslot_bundle::SubSlotBundle;
use dg_xch_core::blockchain::unfinished_block::UnfinishedBlock;
use dg_xch_core::blockchain::unsized_bytes::UnsizedBytes;
use dg_xch_core::clvm::parser::is_canonical_serialization;
use dg_xch_core::consensus::block_filter::chia_block_filter;
use dg_xch_core::consensus::block_generator::{
    BlockGeneratorFlags, BlockGeneratorInput, CoinSpendContext, ConditionValidationContext,
    GeneratorReference, MAX_SPENDS_PER_BLOCK, additions_for_conditions, canonical_additions_root,
    canonical_removals_root, hints_for_conditions, removals_for_conditions,
    transactions_generator_refs_root, transactions_generator_root, transactions_info_hash,
    validate_block_conditions,
};
use dg_xch_core::consensus::block_header_validation::{
    ValidationState, validate_pospace_and_get_required_iters,
};
use dg_xch_core::consensus::block_rewards::{calculate_base_farmer_reward, calculate_pool_reward};
use dg_xch_core::consensus::coinbase::{create_farmer_coin, create_pool_coin};
use dg_xch_core::consensus::constants::ConsensusConstants;
use dg_xch_core::consensus::deficit::calculate_deficit;
use dg_xch_core::consensus::difficulty_adjustment::{
    consensus_walk_window, difficulty_record_depth, get_next_sub_slot_iters_and_difficulty,
};
use dg_xch_core::consensus::full_block_to_block_record::header_block_to_sub_block_record;
use dg_xch_core::consensus::make_sub_epoch_summary::make_sub_epoch_summary;
use dg_xch_core::consensus::pot_iterations::is_overflow_block;
use dg_xch_core::errors::ChiaError;
use dg_xch_core::traits::SizedBytes;
use dg_xch_core::utils::hash_256;
use dg_xch_stores::types::BlockStatus;
use dg_xch_stores::{BatchHandle, BlockStore, CoinStore};
use log::debug;
use std::collections::{HashMap, HashSet};

// Out-of-span generator seeds live in a plain `HashMap<u32, SerializedProgram>` on the engine
// (below), with a PER-WINDOW lifecycle: `Chaser::clear_seed_generators` wipes it at the start of
// every `seed_missing_refs` pass, so it only ever holds the current window's out-of-span refs
// (a handful) — bounded independent of sync length, with no capacity cap that could evict a ref
// the current window still needs. A `--sync-from` window's compression back-references reach below
// the anchor span; the daemon fetches each such generator from a peer and seeds it so the window's
// body validation (`resolve_generator_refs`, which consults this map) can resolve the ref. These
// heights are never confirmed, so they have no confirm-time drain — the per-window clear is their
// removal point. (A prior FIFO-cap design bounded retention but could evict a still-needed ref
// mid-window, walling the node with GeneratorRefHasNoGenerator; the per-window clear cannot.)

/// The expensive PURE half of body validation (CLVM generator run + BLS aggregate verify),
/// precomputed off-thread by the window pipeline. `refs_digest` binds the precompute to the
/// exact inputs it ran with — height, signature decision, and every resolved ref generator's
/// bytes — and the engine recomputes it against its OWN resolution at stage time: a mismatch
/// drops the precompute and runs inline, so a stale one degrades, never changes a verdict.
pub struct PrecomputedBody {
    pub conds: SpendBundleConditions,
    pub agg_sig_verified: bool,
    pub refs_digest: Bytes32,
}

/// The binding digest for a body precompute: the block height, the signature-verification
/// decision, and each resolved ref's (height, generator-bytes hash) in list order.
#[must_use]
pub fn precompute_refs_digest(
    height: u32,
    refs: &[GeneratorReference],
    verify_sig: bool,
) -> Bytes32 {
    let mut buf = Vec::with_capacity(5 + refs.len() * 36);
    buf.extend_from_slice(&height.to_le_bytes());
    buf.push(u8::from(verify_sig));
    for r in refs {
        buf.extend_from_slice(&r.height.to_le_bytes());
        buf.extend_from_slice(&hash_256(r.generator.to_bytes()));
    }
    Bytes32::new(hash_256(buf))
}

/// Run the pure expensive body ops for one transaction block: build the generator input exactly as
/// `validate_body` does, execute the CLVM generator, and (optionally) verify the aggregate
/// signature. Shared by the inline path and the window pipeline's parallel precompute — one
/// construction site, no drift.
///
/// # Errors
/// As `ConsensusPrimitives::run_block_generator` / `verify_block_aggregate_signature`.
pub fn run_body_expensive<P: ConsensusPrimitives>(
    primitives: &P,
    constants: &ConsensusConstants,
    block: &FullBlock,
    generator_refs: &[GeneratorReference],
    verify_sig: bool,
) -> Result<(SpendBundleConditions, bool), NodeError> {
    let Some(generator) = block.transactions_generator.clone() else {
        return Err(NodeError::Invalid("no generator to precompute".into()));
    };
    let ti = block
        .transactions_info
        .as_ref()
        .ok_or_else(|| NodeError::Invalid("transaction block missing transactions_info".into()))?;
    let input = BlockGeneratorInput {
        transactions_generator: generator,
        generator_refs: generator_refs.to_vec(),
        constants: *constants,
        height: block.height(),
        // The CLVM flag ladder keys off the block's OWN height — mainnet 5,496,002 validates
        // with post-fork cost accounting while its prev tx block is pre-fork.
        flags: BlockGeneratorFlags::for_height(constants, block.height()),
    };
    let conds = primitives.run_block_generator(&input)?;
    let mut verified = false;
    if verify_sig {
        primitives.verify_block_aggregate_signature(&conds, &ti.aggregated_signature, constants)?;
        verified = true;
    }
    Ok((conds, verified))
}

pub fn validate_unfinished_block_body<P: ConsensusPrimitives>(
    primitives: &P,
    constants: &ConsensusConstants,
    block: &UnfinishedBlock,
    generator_refs: &[GeneratorReference],
    height: u32,
    prev_tx_height: u32,
) -> Result<Option<SpendBundleConditions>, NodeError> {
    let Some(ti) = block.transactions_info.as_ref() else {
        // Non-transaction unfinished block: reject a generator or a foliage transaction block
        // that should not be there.
        if block.transactions_generator.is_some() {
            return Err(ChiaError::InvalidTransactionsGeneratorHash.into());
        }
        if block.foliage_transaction_block.is_some() {
            return Err(ChiaError::InvalidTransactionsInfoHash.into());
        }
        return Ok(None);
    };
    // A transactions_info without a foliage transaction block, or one whose foliage does not
    // commit to it, is INVALID_TRANSACTIONS_INFO_HASH.
    let ftb = block
        .foliage_transaction_block
        .as_ref()
        .ok_or(ChiaError::InvalidTransactionsInfoHash)?;
    if ftb.transactions_info_hash != transactions_info_hash(ti)? {
        return Err(ChiaError::InvalidTransactionsInfoHash.into());
    }
    // SF9 body rules key on the PREVIOUS transaction block's height, the OPPOSITE keying from
    // the CLVM flag ladder's own-height rule.
    let sf9 = prev_tx_height >= constants.soft_fork9_height;
    let Some(generator) = block.transactions_generator.clone() else {
        // Empty transaction block: zeroed generator root, empty ref list, zero cost, and the
        // aggregate signature verifies over the empty spend set. conds stays None.
        if ti.generator_root != Bytes32::default() {
            return Err(ChiaError::InvalidTransactionsGeneratorHash.into());
        }
        if !block.transactions_generator_ref_list.is_empty() {
            return Err(ChiaError::InvalidTransactionsGeneratorHash.into());
        }
        let refs_root = transactions_generator_refs_root(&block.transactions_generator_ref_list)?;
        if refs_root != ti.generator_refs_root {
            return Err(ChiaError::InvalidTransactionsGeneratorHash.into());
        }
        if ti.cost != 0 {
            return Err(ChiaError::InvalidBlockCost.into());
        }
        let empty = SpendBundleConditions::default();
        primitives.verify_block_aggregate_signature(&empty, &ti.aggregated_signature, constants)?;
        return Ok(None);
    };
    // TOO_MANY_GENERATOR_REFS: generator back-reference lists are banned past SF9, checked
    // before the generator runs.
    if sf9 && !block.transactions_generator_ref_list.is_empty() {
        return Err(ChiaError::TooManyGeneratorRefs.into());
    }
    // Structural identity BEFORE the expensive run: the generator and ref-list roots must match
    // the transactions_info the foliage committed to.
    if transactions_generator_root(&generator) != ti.generator_root {
        return Err(ChiaError::InvalidTransactionsGeneratorHash.into());
    }
    let refs_root = transactions_generator_refs_root(&block.transactions_generator_ref_list)?;
    if refs_root != ti.generator_refs_root {
        return Err(ChiaError::InvalidTransactionsGeneratorHash.into());
    }
    // The run budget: min(MAX_BLOCK_COST_CLVM, claimed cost) — see the doc comment.
    let mut clamped = *constants;
    clamped.max_block_cost_clvm = std::cmp::min(clamped.max_block_cost_clvm, ti.cost);
    let input = BlockGeneratorInput {
        transactions_generator: generator.clone(),
        generator_refs: generator_refs.to_vec(),
        constants: clamped,
        height,
        // The CLVM flag ladder keys on the block's OWN height — from the
        // UNCLAMPED constants' fork heights (the clamp only narrows the cost budget).
        flags: BlockGeneratorFlags::for_height(constants, height),
    };
    let conds = primitives.run_block_generator(&input)?;
    // Rule 9 (INVALID_BLOCK_COST): the claimed cost must be exact. Rule 7
    // (BLOCK_COST_EXCEEDS_MAX) is enforced inside the run by the clamped budget.
    if conds.cost != ti.cost {
        return Err(ChiaError::InvalidBlockCost.into());
    }
    // SF9 (INVALID_TRANSACTIONS_GENERATOR_ENCODING): canonical CLVM serialization, checked after
    // the cost comparison.
    if sf9 && !is_canonical_serialization(generator.as_ref()) {
        return Err(ChiaError::ComplexGeneratorReceived.into());
    }
    // SF9 (TOO_MANY_SPENDS): at most 6,000 spends per block.
    if sf9 && conds.spends.len() > MAX_SPENDS_PER_BLOCK {
        return Err(ChiaError::TooManySpends.into());
    }
    // The signature set is aggregate-verified here as well.
    primitives.verify_block_aggregate_signature(&conds, &ti.aggregated_signature, constants)?;
    Ok(Some(conds))
}

// A validated block's confirmed contribution: fork-choice fields + coin deltas + the persisted record. The
// reorg path re-confirms these without re-validating, so the derived deltas are retained.
#[derive(Clone, Debug)]
pub struct BlockDelta {
    pub header_hash: Bytes32,
    pub prev_hash: Bytes32,
    pub height: u32,
    pub weight: u128,
    pub timestamp: u64,
    pub record: BlockRecord,
    pub additions: Vec<CoinRecord>,
    pub removals: Vec<Bytes32>,
    // Create-coin hints `(hint, created_coin_id)` for this block's spends — the `coin_hint`
    // index feed. Derived from the same conditions as `additions`; written into the
    // coin store in the SAME batch as the deltas (`CoinStore::apply_hints_in`). Empty for a
    // non-transaction / hintless block.
    pub hints: Vec<(Bytes32, Bytes32)>,
}

// The fork view for coin-store body validation: the coin additions/removals of every UNAPPLIED
// ancestor between the coin store's confirmed state and the block being validated — staged
// window blocks (their coins commit at window confirm) and pending orphan-branch blocks (their
// coins commit only on reorg). `fork_height` is the height of the last ancestor whose coins ARE
// in the store (-1 while validating genesis); main-chain spends above it do not count against
// this branch.
struct ForkView {
    fork_height: i64,
    additions: HashMap<Bytes32, CoinRecord>,
    removals: HashSet<Bytes32>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AddBlockOutcome {
    NewPeak { height: u32 },
    Extended { height: u32 },
    Reorg { fork_height: u32, links: u64 },
    Orphan { height: u32 },
    AlreadyHave,
}

/// One landed reorg's wallet-visible summary: the fork height, the rolled-back records, and the
/// re-applied branch whose additions/removals become the "new states". Queued by [`Engine`]'s
/// reorg arm and drained by the chaser's reporting confirm loop, which expands `reapplied` into
/// per-block confirmed deltas and attaches `rolled_back` to the first of them.
#[derive(Debug)]
pub struct ReorgReport {
    /// The last common height — coins above it on the losing branch were reverted.
    pub fork_height: u32,
    /// Post-rollback coin records of the abandoned span: a coin CREATED above the fork reverts
    /// to `confirmed_block_index = 0` / `timestamp = 0` ("no longer on chain"); a coin SPENT
    /// above the fork (created at or below it) reverts to unspent.
    pub rolled_back: Vec<CoinRecord>,
    /// The winning branch's already-validated deltas, fork+1..=tip in height order.
    pub reapplied: Vec<BlockDelta>,
}

// Undrained-report bound: reports are only consumed by the reporting confirm path; non-reporting
// paths (fast-sync's per-block adds) must never grow the queue without limit. Reorgs are rare and
// the wallet push is best-effort — dropping the oldest report loses notifications, never state.
const REORG_REPORT_CAP: usize = 8;

// Bound on the prev-transaction-block cache walk (`prev_tx_context`): non-transaction runs on
// mainnet are dozens of blocks at the extreme (a sub-slot burst); past this the walk falls back
// to the store's by-height read rather than hopping unbounded.
const MAX_PREV_TX_WALK: usize = 512;

// The block-validation engine: binds to the CoinStore/BlockStore traits and the ConsensusPrimitives seam,
// never a concrete backend. Confirms in weight order; a heavier fork triggers a pointer-flip reorg over the
// archive. `pending` holds derived deltas for non-confirmed branches, bounded to a reorg horizon.
// The batched read context for one staging window — see the `Engine::stage_preload` field note.
struct StagePreload {
    // Every header hash of the window (the coverage set): a hash NOT in here always reads the
    // store, so nothing outside the preloaded window can consult stale context.
    covered: std::collections::HashSet<Bytes32>,
    // The window hashes' existing records (headers-first candidates / already-persisted rows),
    // fetched in one `get_block_records_by_hash` call. Absent = no row, definitively (covered set).
    candidates: HashMap<Bytes32, BlockRecord>,
    // Confirmed peak height at window start. No in_main_chain row exists above it (`set_peak`
    // bounds the flips), so a covered block above it is not-confirmed with ZERO reads.
    peak_height: Option<u32>,
}

pub struct Engine<S, P> {
    store: S,
    primitives: P,
    constants: ConsensusConstants,
    cache: BlockRecordCache,
    pending: HashMap<Bytes32, BlockDelta>,
    // Generators of STAGED (not yet confirmed) window blocks, by height — the ref-resolution
    // overlay for in-window generator back-references. Inserted at stage, drained at confirm;
    // bounded by the window size.
    staged_generators: HashMap<u32, dg_xch_core::clvm::program::SerializedProgram>,
    // Weight-proof-attested sub-epoch summary chain (index = sub-epoch), seeded by a mid-chain
    // anchor: the LAST-RESORT source for an included SES whose sub-epoch lies below the anchor
    // span — the declared subepoch_summary_hash (authenticated by the challenge-chain VDFs)
    // still gates every use, so a wrong entry can only reject, never mis-validate.
    summary_chain: Vec<dg_xch_core::blockchain::sub_epoch_summary::SubEpochSummary>,
    // Out-of-span generator seeds (`--sync-from` compression refs below the anchor). Unlike
    // `staged_generators` these heights never confirm, so the cache is capacity-bounded FIFO to
    // keep sync-length-independent retention — see [`SEED_GENERATOR_CACHE_CAP`].
    seed_generators: HashMap<u32, dg_xch_core::clvm::program::SerializedProgram>,
    // Deltas of STAGED (not yet confirmed) window blocks, by header hash — the fork-view overlay
    // for coin-store body validation: a window block's removals must validate against the coin
    // set of earlier blocks of the SAME window, whose coins are not applied to the store until
    // the window confirms. Inserted at stage, drained at confirm; bounded by the window size.
    staged_deltas: HashMap<Bytes32, BlockDelta>,
    stage_preload: Option<StagePreload>,
    horizon: u32,
    // assume-valid seam: below this height script/sig validation is bypassed but the block is
    // still confirmed and its PoW header still validated. 0 = off (fresh genesis default).
    assume_valid: u32,
    // Whether the coin-store-backed body rules (rules 5 and 15-21) are enforced. A full-history
    // node runs them on every block. A `--sync-from`
    // anchored store has no coin set (and no records) below its anchor, so those rules are
    // undefined there; the pure structural rules (3, 10-14) still run on every transaction
    // block. `None` = undecided; resolved lazily per transaction block from the store's
    // main-chain floor (`min_record_height == 0` ⇒ full history) or, on an empty store, from
    // whether the first block is genesis. Only the positive answer is cached (see
    // `coin_rules_enforced`); `Some(true)` forced by [`Engine::with_enforced_coin_rules`].
    full_history: Option<bool>,
    // Landed-reorg summaries awaiting the reporting confirm loop ([`ReorgReport`]); FIFO, bounded
    // by REORG_REPORT_CAP (oldest dropped — a lost report loses wallet pushes, never chain state).
    reorg_reports: std::collections::VecDeque<ReorgReport>,
}

impl<S, P> Engine<S, P>
where
    S: CoinStore + BlockStore + Sync,
    P: ConsensusPrimitives + Sync,
{
    #[must_use]
    pub fn new(store: S, primitives: P, constants: ConsensusConstants) -> Self {
        Self {
            store,
            primitives,
            // Sized from the constants so the deepest consensus walk (the first-sub-epoch
            // retarget, 5,503 records on mainnet) always fits — the flat 5,120 default left
            // negative margin at the worst anchor alignment (the observed restart-resume stall).
            cache: BlockRecordCache::new(consensus_walk_window(&constants)),
            constants,
            pending: HashMap::new(),
            staged_generators: HashMap::new(),
            summary_chain: Vec::new(),
            seed_generators: HashMap::new(),
            staged_deltas: HashMap::new(),
            stage_preload: None,
            horizon: crate::cache::BLOCK_RECORD_WINDOW as u32,
            assume_valid: 0,
            full_history: None,
            reorg_reports: std::collections::VecDeque::new(),
        }
    }

    /// Drain the oldest landed-reorg summary (FIFO — one per `Reorg` outcome, in confirm order).
    pub fn pop_reorg_report(&mut self) -> Option<ReorgReport> {
        self.reorg_reports.pop_front()
    }

    /// Drop any undrained reorg reports — the non-reporting confirm paths (fast-sync) call this
    /// so a stale report can never mis-attach to a later reporting window's outcome.
    pub fn clear_reorg_reports(&mut self) {
        self.reorg_reports.clear();
    }

    /// Force the coin-store-backed body rules ON regardless of the store's history floor — a
    /// strictness override for a caller that asserts the coin set is complete (tests; a full
    /// archive node). The auto-detected default enforces exactly when the chain is held from
    /// genesis (`min_record_height == 0`, or an empty store whose first block is height 0).
    #[must_use]
    pub fn with_enforced_coin_rules(mut self) -> Self {
        self.full_history = Some(true);
        self
    }

    // Set the assume-valid milestone. Blocks strictly below it confirm without script/sig validation.
    #[must_use]
    pub fn with_assume_valid(mut self, milestone: u32) -> Self {
        self.assume_valid = milestone;
        self
    }

    #[must_use]
    pub fn assume_valid(&self) -> u32 {
        self.assume_valid
    }

    #[must_use]
    pub fn store(&self) -> &S {
        &self.store
    }

    #[must_use]
    pub fn cache(&self) -> &BlockRecordCache {
        &self.cache
    }

    /// Load the recent record ancestry from the store into the walk cache. The cache normally fills
    /// only as blocks CONFIRM, so after a process restart — or after a backfill wrote records the
    /// running process never confirmed — the strict validation walks (which read the cache first)
    /// would fall off the cache edge mid-window. Walks back from the peak along `prev_hash`
    /// (candidate records included, hole-detecting: the walk stops at the first record the store
    /// does not hold) up to the cache capacity — the constants-derived walk window, not the flat
    /// legacy 5,120 (which re-warmed LESS than the deepest retarget walk reads). Returns the
    /// number of records loaded.
    ///
    /// # Errors
    /// Returns [`NodeError::Store`] on a store read failure.
    pub async fn warm_cache_from_store(&mut self) -> Result<usize, NodeError> {
        let Some((peak_hash, _)) = self.store.get_peak().await? else {
            return Ok(0);
        };
        let mut loaded = 0usize;
        let mut hash = peak_hash;
        while loaded < self.cache.capacity() {
            let Some(record) = self.store.get_block_record(&hash).await? else {
                break;
            };
            let prev = record.prev_hash;
            let at_genesis = record.height == 0;
            self.cache.insert(record);
            loaded += 1;
            if at_genesis {
                break;
            }
            hash = prev;
        }
        Ok(loaded)
    }

    /// Validate a full block and, if it wins fork choice, confirm it (coins + peak).
    ///
    /// Block validity is fully established before any state commit: the body is validated (through the
    /// primitive seam) and the delta derived first; only then are the coins applied.
    ///
    /// # Errors
    /// Returns [`NodeError::Consensus`] if body validation rejects the block, [`NodeError::Orphan`] if the
    /// parent is unknown, or [`NodeError::Store`] on a persistence failure.
    pub async fn add_block(&mut self, block: &FullBlock) -> Result<AddBlockOutcome, NodeError> {
        Ok(self.add_block_with_delta(block).await?.0)
    }

    /// Same as [`Engine::add_block`], but also returns the confirmed block's derived delta (coin
    /// additions/removals + record) so a caller can drive per-peak side effects — the wallet coin-state
    /// subscription and the mempool new-peak revalidation — without re-deriving them. `None` when the block
    /// was already confirmed (`AlreadyHave`); the tip delta of a reorg is the new tip's own change (a deep
    /// reorg's full reapplied branch is available through the store).
    ///
    /// # Errors
    /// Returns [`NodeError::Consensus`] if body validation rejects the block, [`NodeError::Orphan`] if the
    /// parent is unknown, or [`NodeError::Store`] on a persistence failure.
    pub async fn add_block_with_delta(
        &mut self,
        block: &FullBlock,
    ) -> Result<(AddBlockOutcome, Option<BlockDelta>), NodeError> {
        // Await-safe instrumentation: an Entered guard (`span.enter()`) held across an `.await`
        // races the sharded registry under work-stealing (clone-after-close panic). Attach the
        // span to the future instead — it is entered on each poll and exited on every yield.
        log::debug!("block.apply height={}", block.height());
        async move {
            let Some(delta) = self.prepare_delta(block, None, None).await? else {
                return Ok((AddBlockOutcome::AlreadyHave, None));
            };
            let batch = self.persist_archive(block, &delta).await?;
            let reported = delta.clone();
            let outcome = self.confirm(batch, delta).await?;
            Ok((outcome, Some(reported)))
        }
        .await
    }

    /// Stage one block of a sync window for the cross-block pipeline: full validation with
    /// every VDF proof deferred into `vdf_sink` (all other gates run and reject inline), the archive
    /// writes (record + body + status) committed, and the record inserted into the walk cache so the
    /// NEXT block's derivation walks real ancestry before this one confirms. The caller drains the
    /// sink across all cores once per window ([`Engine::verify_vdf_window`]) and then confirms each
    /// staged delta in order ([`Engine::confirm_staged`]). `None` = the block was already confirmed.
    ///
    /// # Errors
    /// As [`Engine::add_block_with_delta`], minus VDF rejections (those surface at the window drain).
    pub async fn stage_block(
        &mut self,
        block: &FullBlock,
        vdf_sink: &crate::header::HeaderSink,
    ) -> Result<Option<BlockDelta>, NodeError> {
        self.stage_block_pre(block, vdf_sink, None).await
    }

    /// [`Engine::stage_block`] with an optional precomputed expensive-body result from the
    /// window pipeline's parallel CLVM+agg-sig pass.
    ///
    /// # Errors
    /// As [`Engine::stage_block`].
    pub async fn stage_block_pre(
        &mut self,
        block: &FullBlock,
        vdf_sink: &crate::header::HeaderSink,
        pre: Option<PrecomputedBody>,
    ) -> Result<Option<BlockDelta>, NodeError> {
        // Await-safe instrumentation (see `add_block_with_delta`): NEVER hold an Entered guard
        // across the `.await`s below — that is the sharded-registry clone-after-close panic that
        // silently stalled the node at a `block.stage` boundary. Instrument the future instead.
        log::debug!("block.stage height={}", block.height());
        async move {
            let Some(delta) = self.prepare_delta(block, Some(vdf_sink), pre).await? else {
                return Ok(None);
            };
            let batch = self.persist_archive(block, &delta).await?;
            self.store.commit(batch).await?;
            Ok(Some(self.finish_stage(block, delta)))
        }
        .await
    }

    /// [`Engine::stage_block_pre`] with the archive writes DEFERRED entirely — no batch opens and
    /// no writer is touched; the caller persists the archive rows later (inside the confirm
    /// transaction, via [`Engine::persist_archive_window`]). This is the staging half the
    /// stage-ahead pipeline uses: window N+1 stages against the overlay while window N's drain
    /// still owns the CPU and window N's confirm still owns the writer. The overlay inserts
    /// ([`Engine::finish_stage`]'s walk-cache/staged-delta entries) happen exactly as in the
    /// writing variants, so in-window and cross-window staged reads are unchanged.
    ///
    /// # Errors
    /// As [`Engine::stage_block_pre`].
    pub async fn stage_block_pre_dry(
        &mut self,
        block: &FullBlock,
        vdf_sink: &crate::header::HeaderSink,
        pre: Option<PrecomputedBody>,
    ) -> Result<Option<BlockDelta>, NodeError> {
        // Await-safe instrumentation: see `stage_block_pre`.
        log::debug!("block.stage height={}", block.height());
        async move {
            let Some(delta) = self.prepare_delta(block, Some(vdf_sink), pre).await? else {
                return Ok(None);
            };
            Ok(Some(self.finish_stage(block, delta)))
        }
        .await
    }

    /// Persist the archive rows for a dry-staged window into an OPEN batch — the deferred half of
    /// [`Engine::stage_block_pre_dry`], called from the confirm with its transaction so archive
    /// rows still land before `set_peak` inside the same commit.
    ///
    /// # Errors
    /// Returns [`NodeError::Store`] on a persistence failure.
    pub async fn persist_archive_window(
        &self,
        rows: &[(&FullBlock, &BlockDelta)],
        batch: &mut BatchHandle,
    ) -> Result<(), NodeError> {
        for (block, delta) in rows {
            self.persist_archive_in(block, delta, batch).await?;
        }
        Ok(())
    }

    /// [`Engine::stage_block_pre`] with the archive writes threaded into a caller-owned WINDOW
    /// staging batch instead of a per-block commit — the CATCH-UP half of the phase-aware commit
    /// granularity [`Engine::confirm_staged_batch`] already applies on the confirm side (batch
    /// during bulk sync, one transaction per block near the tip). On the SQLite
    /// backend every commit is an fsync serialized on the single writer connection, so per-block
    /// staging commits cost a full write round-trip per block of pure dead time between the
    /// parallel body precompute and the confirm. The batch opens lazily at the first block that
    /// actually stages (an all-`AlreadyHave` window opens no transaction); the caller commits it
    /// once for the window — BEFORE any other `begin()` (the open batch holds the single writer),
    /// and even on a mid-window stage error, because the already-staged prefix still confirms
    /// (`set_peak` walks the archive rows this batch carries). A crash before the window commit
    /// loses only candidate archive rows — the durable peak is untouched and the resume path
    /// re-fetches the window, exactly as it re-fetches a window whose confirm batch was lost.
    ///
    /// # Errors
    /// As [`Engine::stage_block_pre`].
    pub async fn stage_block_pre_in(
        &mut self,
        block: &FullBlock,
        vdf_sink: &crate::header::HeaderSink,
        pre: Option<PrecomputedBody>,
        batch: &mut Option<BatchHandle>,
    ) -> Result<Option<BlockDelta>, NodeError> {
        // Await-safe instrumentation: see `stage_block_pre`.
        log::debug!("block.stage height={}", block.height());
        async move {
            let Some(delta) = self.prepare_delta(block, Some(vdf_sink), pre).await? else {
                return Ok(None);
            };
            if batch.is_none() {
                *batch = Some(self.store.begin().await?);
            }
            let b = batch.as_mut().expect("window staging batch just opened");
            self.persist_archive_in(block, &delta, b).await?;
            Ok(Some(self.finish_stage(block, delta)))
        }
        .await
    }

    // The persistence-independent tail of staging: the walk-cache/overlay inserts that let the
    // NEXT block of the window validate against this one before anything is committed — which is
    // exactly why the window staging batch can stay open across the whole loop: no staging read
    // goes back to the store for in-window state.
    fn finish_stage(&mut self, block: &FullBlock, delta: BlockDelta) -> BlockDelta {
        self.cache.insert(delta.record.clone());
        if let Some(g) = &block.transactions_generator {
            self.staged_generators.insert(delta.height, g.clone());
        }
        // Fork-view overlay: the NEXT window blocks validate their removals against this
        // block's coin delta before it is applied to the store (see `staged_deltas`).
        self.staged_deltas.insert(delta.header_hash, delta.clone());
        delta
    }

    /// Confirm a whole staged window with PHASE-AWARE commit granularity, keyed on the store
    /// near-tip flag the follow driver sets (in_near_tip_band). NEAR-TIP: ONE atomic transaction per
    /// block -- begin, apply coins, set peak, commit -- so the DURABLE peak advances every block (the
    /// 300s liveness clock keeps resetting) and the WAL stays ~one block for the active checkpointer.
    /// CATCH-UP: the whole window in ONE transaction, so the full slow-disk write budget goes to the
    /// writer while the checkpointer stays quiet (the WAL grows toward the autocheckpoint failsafe).
    /// Batch during bulk sync, one db transaction per block near the tip. The fast
    /// path applies every delta that plainly extends the running peak; a non-extension stops it and
    /// the remaining deltas take the [`Self::confirm_staged`] path, so fork choice / reorg semantics
    /// stay byte-identical. Reorg-safe: a mid-window failure leaves peak = last committed block
    /// (per-block: K-1; batch: the pre-window peak), re-fetched from there; begin() rolls a dropped
    /// batch back so a failure never wedges the writer.
    ///
    /// # Errors
    /// Returns [`NodeError::Store`] on a persistence failure.
    pub async fn confirm_staged_batch(
        &mut self,
        deltas: Vec<BlockDelta>,
    ) -> Result<Vec<AddBlockOutcome>, NodeError> {
        self.confirm_staged_batch_in(deltas, None).await
    }

    /// [`Engine::confirm_staged_batch`] continuing in a CARRIED open batch — the window staging
    /// transaction (`stage_block_pre_in`'s archive rows), handed over uncommitted so the whole
    /// catch-up window costs ONE writer transaction and ONE fsync: archive + coins + peak commit
    /// together (the archive-before-peak ordering constraint is satisfied inside the single
    /// transaction, so the staging commit is not a second serialized writer round-trip per
    /// window). When nothing confirms (an empty `deltas` — e.g. the whole window failed its VDF
    /// drain) the carried batch is DROPPED, rolling the staged archive rows back — the window
    /// retries and re-stages wholesale (`begin()`'s rollback guard clears the dangling
    /// transaction). A carried batch whose FIRST delta does not extend the peak is committed
    /// archive-only (no `set_peak`) before the sequential fork-choice path runs, which needs
    /// those rows durable.
    ///
    /// # Errors
    /// Returns [`NodeError::Store`] on a persistence failure.
    pub async fn confirm_staged_batch_in(
        &mut self,
        deltas: Vec<BlockDelta>,
        carry: Option<BatchHandle>,
    ) -> Result<Vec<AddBlockOutcome>, NodeError> {
        let mut outcomes = Vec::with_capacity(deltas.len());
        if deltas.is_empty() {
            // Nothing to confirm: roll the carried staging rows back (see the doc note).
            drop(carry);
            return Ok(outcomes);
        }
        // Running peak: one store read for the window, then tracked in memory.
        let mut running = match self.store.get_peak().await? {
            Some((hash, _)) => {
                let weight = self.record_weight(&hash).await?;
                Some((hash, weight))
            }
            None => None,
        };
        // Phase-aware commit granularity: near the tip, commit ONE transaction per block so the
        // durable peak advances every block (the 300s liveness clock keeps resetting) and the WAL stays
        // ~one block for the active checkpointer; during bulk catch-up, accumulate the whole window into
        // ONE transaction so the full slow-disk write budget goes to the writer while the checkpointer is
        // quiet. The store carries the phase (near-tip band), set by the follow driver: batch
        // during long/bulk sync, one db transaction per block near the tip.
        let per_block = self.store.near_tip();
        // Catch-up mode reuses ONE batch across the window — seeded with the CARRIED staging
        // transaction when the window loop handed one over; near-tip mode commits each block
        // inline (the first near-tip block folds any carried rows into its own commit).
        let mut batch: Option<BatchHandle> = carry;
        let mut last_applied: Option<Bytes32> = None;
        let mut idx = 0usize;
        while idx < deltas.len() {
            let delta = &deltas[idx];
            let fresh_chain = running.is_none();
            let extends = match &running {
                None => idx == 0,
                Some((peak_hash, peak_weight)) => {
                    delta.prev_hash == *peak_hash && delta.weight > *peak_weight
                }
            };
            if !extends {
                break;
            }
            let mut b = match batch.take() {
                Some(b) => b,
                None => self.store.begin().await?,
            };
            self.store
                .apply_block_in(
                    &mut b,
                    delta.height,
                    delta.timestamp,
                    &delta.additions,
                    &delta.removals,
                )
                .await?;
            // coin_hint rows join this block's batch (per-block near tip, or the window batch
            // during catch-up) so they commit atomically with the coins; no-op without the hint tier.
            self.store.apply_hints_in(&mut b, &delta.hints).await?;
            if per_block {
                // NEAR-TIP: commit this block atomically (peak = this block). A crash/error leaves the
                // store at the last committed block -- 0..K-1 with peak = K-1, block K rolled back, the
                // follow path re-fetches from K; begin()'s rollback guard clears a dropped batch so a
                // mid-window failure never wedges the writer.
                self.store.set_peak_in(&mut b, &delta.header_hash).await?;
                self.store.commit(b).await?;
            } else {
                // CATCH-UP: keep accumulating into the one window transaction; set_peak + commit once below.
                batch = Some(b);
            }
            self.staged_generators.remove(&delta.height);
            self.staged_deltas.remove(&delta.header_hash);
            self.cache.insert(delta.record.clone());
            self.pending.remove(&delta.header_hash);
            last_applied = Some(delta.header_hash);
            running = Some((delta.header_hash, delta.weight));
            outcomes.push(if fresh_chain {
                AddBlockOutcome::NewPeak {
                    height: delta.height,
                }
            } else {
                AddBlockOutcome::Extended {
                    height: delta.height,
                }
            });
            idx += 1;
        }
        // CATCH-UP tail: one set_peak (to the window tip) + one commit for the accumulated batch. On a
        // mid-window failure the batch is dropped (whole window rolled back, peak unchanged) and re-fetched;
        // on a break at a non-extension the committed prefix stands with peak = the last extending block.
        // A batch with NO applied delta (a carried staging transaction whose first delta did not
        // extend) commits archive-only — the sequential path below reads those rows.
        if let Some(mut b) = batch {
            if let Some(tip) = last_applied {
                self.store.set_peak_in(&mut b, &tip).await?;
            }
            self.store.commit(b).await?;
        }
        if let Some(last) = deltas.get(idx.saturating_sub(1)) {
            self.prune_pending(last.height);
        }
        // Anything that wasn't a plain extension goes through the sequential path unchanged.
        for delta in deltas.into_iter().skip(idx) {
            outcomes.push(self.confirm_staged(delta).await?);
        }
        Ok(outcomes)
    }

    /// Confirm a staged delta (fork choice, coins, peak) after its window's VDF drain passed.
    ///
    /// # Errors
    /// Returns [`NodeError::Store`] on a persistence failure.
    pub async fn confirm_staged(
        &mut self,
        delta: BlockDelta,
    ) -> Result<AddBlockOutcome, NodeError> {
        // The block is on (or rejected from) the confirmed chain now — the store serves its
        // generator; the staged overlay entries retire.
        self.staged_generators.remove(&delta.height);
        self.staged_deltas.remove(&delta.header_hash);
        let batch = self.store.begin().await?;
        self.confirm(batch, delta).await
    }

    /// Drop staged-generator overlay entries for blocks that will NOT be confirmed (a failed
    /// window drain or a mid-window stage rejection) — the retry re-stages and re-inserts them.
    /// Also drops the out-of-span seed cache: the retry's `missing_ref_heights` scan re-detects
    /// and re-seeds whatever the next attempt actually needs.
    /// Seed the weight-proof-attested summary chain (see the `summary_chain` field) — called
    /// once by the mid-chain anchor after the proof validates.
    pub fn seed_summary_chain(
        &mut self,
        summaries: Vec<dg_xch_core::blockchain::sub_epoch_summary::SubEpochSummary>,
    ) {
        self.summary_chain = summaries;
    }

    pub fn clear_staged_overlay(&mut self) {
        self.staged_generators.clear();
        self.seed_generators.clear();
        self.staged_deltas.clear();
        self.stage_preload = None;
    }

    pub async fn preload_stage_context(&mut self, blocks: &[FullBlock]) -> Result<(), NodeError> {
        let mut covered = std::collections::HashSet::with_capacity(blocks.len());
        let mut hashes = Vec::with_capacity(blocks.len());
        for b in blocks {
            let hh = b.header_hash()?;
            covered.insert(hh);
            hashes.push(hh);
        }
        let candidates = self
            .store
            .get_block_records_by_hash(&hashes)
            .await?
            .into_iter()
            .map(|r| (r.header_hash, r))
            .collect();
        let peak_height = self.store.get_peak().await?.map(|(_, h)| h);
        self.stage_preload = Some(StagePreload {
            covered,
            candidates,
            peak_height,
        });
        Ok(())
    }

    /// Drop the window staging read context (see [`Self::preload_stage_context`]).
    pub fn clear_stage_preload(&mut self) {
        self.stage_preload = None;
    }

    /// Wipe only the out-of-span seed cache (leaving in-window staged entries). The daemon calls
    /// this at the START of each `seed_missing_refs` pass so the cache holds exactly the current
    /// window's out-of-span refs — bounded independent of sync length, with no eviction that could
    /// drop a ref the window still needs.
    pub fn clear_seed_generators(&mut self) {
        self.seed_generators.clear();
    }

    /// Seed the generator overlay with a generator fetched out-of-band. A mid-chain anchor
    /// (`--sync-from`) stores no bodies below the anchor span, so a block whose compression
    /// back-ref points below it can only resolve through a peer fetch. These heights are NEVER
    /// confirmed, so they have no confirm-time drain — [`Self::clear_seed_generators`] (invoked
    /// per window) is their removal point.
    pub fn seed_generator(
        &mut self,
        height: u32,
        generator: dg_xch_core::clvm::program::SerializedProgram,
    ) {
        self.seed_generators.insert(height, generator);
    }

    /// The overlay generator at `height`, when present: an in-window staged block first, then the
    /// out-of-span seed cache.
    #[must_use]
    pub fn staged_generator(
        &self,
        height: u32,
    ) -> Option<&dg_xch_core::clvm::program::SerializedProgram> {
        self.staged_generators
            .get(&height)
            .or_else(|| self.seed_generators.get(&height))
    }

    /// The previous-TRANSACTION-block height for `block`, from the confirmed store — the
    /// flag-ladder key the window pipeline's parallel
    /// body precompute uses. The checkpoint-anchor candidate subtlety is deliberately ignored
    /// here: on any mismatch with the engine's own stage-time derivation the precompute is
    /// discarded and validation runs inline.
    ///
    /// # Errors
    /// Returns [`NodeError::Store`] on a store read failure.
    pub async fn prev_tx_height_for(&self, block: &FullBlock) -> Result<u32, NodeError> {
        let Some(prev) = self
            .store
            .get_block_record(&block.prev_header_hash())
            .await?
        else {
            return Ok(0);
        };
        if prev.is_transaction_block() {
            Ok(prev.height)
        } else {
            Ok(prev.prev_transaction_block_height)
        }
    }

    #[must_use]
    pub fn primitives(&self) -> &P {
        &self.primitives
    }

    /// Collection-size gauges for memory diagnosis: (walk cache, pending orphans,
    /// generator overlay). The overlay figure is the TOTAL generator retention — in-window staged
    /// entries plus the bounded out-of-span seed cache — so the retention gauge cannot hide growth
    /// in either half.
    #[must_use]
    pub fn collection_sizes(&self) -> (usize, usize, usize) {
        (
            self.cache.len(),
            self.pending.len(),
            self.staged_generators.len() + self.seed_generators.len(),
        )
    }

    /// Drain a window's deferred VDF queue across every available core; `true` = all proofs valid.
    #[must_use]
    pub fn verify_vdf_window(&self, queue: Vec<crate::header::QueuedVdf>) -> bool {
        crate::header::verify_vdf_batch(&self.primitives, &self.constants, queue)
    }

    /// Verify a whole window's deferred header BLS signatures across all cores. `true` iff every
    /// signature verifies (byte-identical to the inline gates). The window pipeline uses this as the
    /// fast-path check; on a `false`, [`crate::header::first_failing_sig`] over the failing block's
    /// slice attributes the exact height and rejection string.
    #[must_use]
    pub fn verify_sig_window(&self, queue: &[crate::header::QueuedSig]) -> bool {
        crate::header::verify_sig_batch(queue)
    }

    // The validation half shared by the single-block and staged paths: AlreadyHave check, ancestor
    // and generator-ref resolution, prev-transaction context, and full delta derivation (with the
    // VDF proofs deferred to `vdf_sink` when the window pipeline is driving).
    async fn prepare_delta(
        &mut self,
        block: &FullBlock,
        vdf_sink: Option<&crate::header::HeaderSink>,
        pre: Option<PrecomputedBody>,
    ) -> Result<Option<BlockDelta>, NodeError> {
        let header_hash = block.header_hash()?;
        let preloaded_candidate = match &self.stage_preload {
            Some(p) if p.covered.contains(&header_hash) => {
                Some(p.candidates.get(&header_hash).cloned())
            }
            _ => None,
        };
        // AlreadyHave means already confirmed on the main chain — not merely that a candidate record exists.
        // The headers-first pass stores candidate records ahead of their bodies (in_main_chain = 0); the body
        // pipeline must still confirm those, so a bare record is not a short-circuit.
        let confirmed = match (&preloaded_candidate, &self.stage_preload) {
            // Preloaded, no record row at all: cannot be on the main chain (a confirmed block
            // always has its record row). Zero reads.
            (Some(None), _) => false,
            // Preloaded with a row ABOVE the confirmed peak: no in_main_chain row can exist there
            // (`set_peak` bounds the flips at the peak). Zero reads — the forward-window case.
            (Some(Some(r)), Some(p)) if p.peak_height.is_none_or(|ph| r.height > ph) => false,
            // Preloaded with a row at-or-below the peak (a replayed window): the main-chain row at
            // that height decides, exactly as the unpreloaded path (one read, replay-only).
            (Some(Some(r)), _) => self
                .store
                .get_block_record_by_height(r.height)
                .await?
                .is_some_and(|c| c.header_hash == header_hash),
            _ => self.is_confirmed(&header_hash).await?,
        };
        if self.pending.contains_key(&header_hash) || confirmed {
            return Ok(None);
        }
        let prev = self.prev_record(block).await?;
        // Resolve block-level generator back-references: each referenced height's generator is
        // fetched from the confirmed chain, in ref-list order — empty for a block with no
        // ref-list. Body validation stays sync (derive_delta), so resolution — the only async,
        // store-touching step — happens here and the resolved refs are threaded down.
        // ONLY for the inline body run: when the window pipeline hands a `PrecomputedBody`, the
        // precompute already resolved these refs and `run_body_expensive` consumed them; the
        // precomputed branch of `validate_body` never reads them again (the `generator_refs_root`
        // identity keys on the raw ref-list HEIGHTS, not the resolved generators). Re-resolving
        // here would re-read every referenced generator body from the store per staged block —
        // dead sequential reads on the sync hot path.
        let generator_refs = self
            .resolve_generator_refs(&block.transactions_generator_ref_list)
            .await?;
        // Previous-TRANSACTION-block context for the time-lock conditions: ASSERT_HEIGHT/SECONDS
        // validate against the previous transaction block's height/timestamp, never this block's
        // own.
        // The headers-first candidate record for THIS block (if the fast-sync header pass stored
        // one) carries the weight-proof-attested epoch context — sub_slot_iters and, at a
        // sub-epoch boundary, the included sub-epoch summary — the checkpoint's ground truth when
        // the local ancestry is too shallow for the epoch walk. Without it the anchor confirm
        // would fabricate
        // sub_slot_iters_starting into the record and every descendant would inherit the poisoned value
        // (the required_iters-over-sp-interval rejections just after sub-epoch boundaries).
        let candidate = match preloaded_candidate {
            Some(c) => c,
            None => self.store.get_block_record(&header_hash).await?,
        };
        let prev_tx = self
            .prev_tx_context(prev.as_ref(), candidate.as_ref())
            .await?;
        // Height/weight continuity against the parent record (before any body work).
        if let Some(p) = prev.as_ref() {
            let height = block.height();
            if height != p.height + 1 {
                return Err(NodeError::Invalid(format!(
                    "height {height} does not extend parent {}",
                    p.height
                )));
            }
            if block.reward_chain_block.weight <= p.weight {
                return Err(NodeError::Invalid(
                    "weight does not strictly increase over parent".to_string(),
                ));
            }
        }
        if let Some(ftb) = block.foliage_transaction_block.as_ref() {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            if ftb.timestamp > now.saturating_add(self.constants.max_future_time2) {
                return Err(NodeError::Invalid(format!(
                    "TIMESTAMP_TOO_FAR_IN_FUTURE at height {}: {} > now {now} + {}",
                    block.height(),
                    ftb.timestamp,
                    self.constants.max_future_time2
                )));
            }
        }
        // Body validation: the pure half (generator/sig/roots-of-generator identity) in
        // `validate_body`, then the coin-store-backed half (body rules 3, 5, 10-21) — both
        // BEFORE record derivation, keeping the engine's body-first rejection order.
        let (conds, additions, removals) = if block.is_transaction_block() {
            let out = self.validate_body(block, &generator_refs, prev_tx, pre)?;
            self.validate_body_coin_rules(block, out.0.as_ref(), &out.1, &out.2, prev_tx)
                .await?;
            out
        } else {
            (None, Vec::new(), Vec::new())
        };
        // Record derivation runs the consensus ancestry walks (retarget/SES/header validation)
        // over the walk cache, falling back to the store on a cache miss: a NotFound from a walk
        // triggers ONE store-backed ancestry repair + retry, so a cache/store misalignment (a
        // restart warm
        // that stopped at a mid-span hole since backfilled, records a driver repair landed
        // without a re-warm) resolves from the store instead of livelocking the MissingRecord
        // recovery on the identical window. The hot path is untouched — the fallback runs only
        // after a miss. The VDF/sig sink is rewound to its pre-attempt checkpoint before the
        // retry so a mid-validate miss cannot double-queue deferred proofs.
        let sink_checkpoint = vdf_sink.map(crate::header::HeaderSink::checkpoint);
        let record = match self.compute_record(block, prev.as_ref(), candidate.as_ref(), vdf_sink) {
            Ok(record) => record,
            Err(e) if Self::is_record_miss(&e) => {
                let loaded = self
                    .repair_walk_ancestry(block.prev_header_hash(), block.height())
                    .await?;
                if loaded == 0 {
                    // The store does not hold the missing ancestry either — a genuine record
                    // gap. Surface the original NotFound so the driver's MissingRecord
                    // recovery (header backfill + re-warm) can repair the STORE.
                    return Err(e);
                }
                debug!(
                    "walk cache miss repaired from store; retrying record derivation loaded={} height={}",
                    loaded,
                    block.height()
                );
                if let (Some(sink), Some(cp)) = (vdf_sink, sink_checkpoint) {
                    sink.truncate(cp);
                }
                self.compute_record(block, prev.as_ref(), candidate.as_ref(), vdf_sink)?
            }
            Err(e) => return Err(e),
        };
        let delta = self.derive_delta(block, record, conds, additions, removals)?;
        Ok(Some(delta))
    }

    fn is_record_miss(e: &NodeError) -> bool {
        matches!(e, NodeError::Io(io) if io.kind() == std::io::ErrorKind::NotFound)
    }

    /// Store fallback for the consensus ancestry walks (cache miss → DB read).
    /// Walks back from `tip` (the staging block's parent hash) along `prev_hash`, pulling every
    /// record the cache lacks from the store into the cache, down to the walk depth the block at
    /// `height` can read (`difficulty_record_depth` + trailing slack, capped at the cache
    /// capacity). Hole-detecting: stops at the first record the store does not hold. Returns the
    /// number of records loaded FROM THE STORE (0 = the cache already covered everything
    /// reachable, or the store is missing the same ancestry).
    ///
    /// # Errors
    /// Returns [`NodeError::Store`] on a store read failure.
    pub async fn repair_walk_ancestry(
        &mut self,
        tip: Bytes32,
        height: u32,
    ) -> Result<usize, NodeError> {
        let depth = difficulty_record_depth(&self.constants, height.saturating_sub(1)) as usize;
        let slack = (2 * self.constants.max_sub_slot_blocks) as usize;
        let walk = depth.saturating_add(slack).min(self.cache.capacity());
        let mut fetched: Vec<BlockRecord> = Vec::new();
        let mut hash = tip;
        let mut steps = 0usize;
        while steps < walk {
            let (prev, at_genesis) = match self.cache.get(&hash) {
                Some(r) => (r.prev_hash, r.height == 0),
                None => match self.store.get_block_record(&hash).await? {
                    Some(r) => {
                        let step = (r.prev_hash, r.height == 0);
                        fetched.push(r);
                        step
                    }
                    None => break,
                },
            };
            steps += 1;
            if at_genesis {
                break;
            }
            hash = prev;
        }
        let loaded = fetched.len();
        // Ascending insertion keeps `BlockRecordCache`'s lowest-first eviction from dropping the
        // very records this repair fetched (a descending insert into a full cache evicts itself).
        for r in fetched.into_iter().rev() {
            self.cache.insert(r);
        }
        Ok(loaded)
    }

    // Persist the archive (record + cold body + status) into an OPEN batch; the record enters as a
    // candidate (in_main_chain flips only under set_peak). The single-block path folds confirm into
    // the same batch (one commit/fsync per block); the per-block staged path commits it immediately;
    // the catch-up window staging path threads one shared batch across the whole window.
    async fn persist_archive(
        &self,
        block: &FullBlock,
        delta: &BlockDelta,
    ) -> Result<BatchHandle, NodeError> {
        let mut batch = self.store.begin().await?;
        self.persist_archive_in(block, delta, &mut batch).await?;
        Ok(batch)
    }

    // The batch-threading half of [`Self::persist_archive`]: the archive writes into a caller-owned
    // open batch. The caller owns the commit — per block, or once per staged window.
    async fn persist_archive_in(
        &self,
        block: &FullBlock,
        delta: &BlockDelta,
        batch: &mut BatchHandle,
    ) -> Result<(), NodeError> {
        // Durable per-block status: Bypass below the assume-valid milestone, Validated otherwise.
        let status = if block.height() < self.assume_valid {
            BlockStatus::Bypass
        } else {
            BlockStatus::Validated
        };
        async {
            self.store
                .add_block_records_in(&mut *batch, std::slice::from_ref(&delta.record))
                .await?;
            self.store
                .append_many(&mut *batch, std::slice::from_ref(block))
                .await?;
            self.store
                .set_status_in(&mut *batch, &delta.header_hash, status)
                .await
        }
        .await?;
        Ok(())
    }

    /// Persist a pre-derived, already-validated delta and run fork choice. Used by the confirm/reorg replay
    /// path (record only, no body).
    ///
    /// # Errors
    /// Returns [`NodeError::Store`] on a persistence failure.
    pub async fn add_delta(&mut self, delta: BlockDelta) -> Result<AddBlockOutcome, NodeError> {
        let mut batch = self.store.begin().await?;
        self.store
            .add_block_records_in(&mut batch, std::slice::from_ref(&delta.record))
            .await?;
        self.confirm(batch, delta).await
    }

    async fn prev_record(&self, block: &FullBlock) -> Result<Option<BlockRecord>, NodeError> {
        self.prev_record_by(block.prev_header_hash(), block.height())
            .await
    }

    async fn prev_record_by(
        &self,
        prev_hash: Bytes32,
        height: u32,
    ) -> Result<Option<BlockRecord>, NodeError> {
        if height == 0 {
            return Ok(None);
        }
        if let Some(r) = self.cache.get(&prev_hash) {
            return Ok(Some(r.clone()));
        }
        if let Some(r) = self.store.get_block_record(&prev_hash).await? {
            return Ok(Some(r));
        }
        // Bootstrap: a fresh store (no peak) accepts a base block whose ancestors are not yet synced (a
        // checkpoint entry point). With a peak established, an unknown parent is a real orphan.
        if self.store.get_peak().await?.is_none() {
            return Ok(None);
        }
        Err(NodeError::Orphan(format!(
            "unknown parent {prev_hash} for block at height {height}"
        )))
    }

    /// Resolve a block's `transactions_generator_ref_list` into ordered [`GeneratorReference`]s by fetching
    /// each referenced prior block's generator from the confirmed main chain — the storage side of
    /// block-level back-reference compression. Order follows the ref-list; a referenced height
    /// with no confirmed generator is a validation failure (`GENERATOR_REF_HAS_NO_GENERATOR`),
    /// never a silent pass.
    ///
    /// # Errors
    /// [`NodeError::Consensus`] ([`ChiaError::GeneratorRefHasNoGenerator`]) if a referenced height has no
    /// confirmed generator; [`NodeError::Store`] on a lookup failure.
    pub async fn resolve_generator_refs(
        &self,
        ref_list: &[u32],
    ) -> Result<Vec<GeneratorReference>, NodeError> {
        // The consensus cap gates the list BEFORE any per-entry resolution: the list is
        // peer-controlled, and each entry below costs a store point-read.
        if ref_list.len() > self.constants.max_generator_ref_list_size as usize {
            return Err(ChiaError::TooManyGeneratorRefs.into());
        }
        let mut refs = Vec::with_capacity(ref_list.len());
        for (index, &height) in ref_list.iter().enumerate() {
            // Overlay first (`staged_generator` = in-window staged block, THEN the out-of-span seed
            // cache), store second. Two reasons the store alone is not enough: (1) under the window
            // pipeline a block may reference a generator from an EARLIER BLOCK OF THE SAME WINDOW —
            // staged but not yet confirmed, so the store's in-main-chain query misses it (the live
            // GeneratorRefHasNoGenerator wall at mainnet 290,487); (2) a `--sync-from` block may
            // reference a generator BELOW THE ANCHOR — never in this node's store, only in the
            // peer-fetched seed cache. Using the raw `staged_generators` map here (instead of the
            // seed-aware `staged_generator`) missed case (2) and walled every anchored node at the
            // first deep back-ref.
            let generator = match self.staged_generator(height) {
                Some(g) => g.clone(),
                None => self
                    .store
                    .get_generator_at_height(height)
                    .await?
                    .ok_or(ChiaError::GeneratorRefHasNoGenerator)?,
            };
            refs.push(GeneratorReference {
                height,
                index: u32::try_from(index).unwrap_or(u32::MAX),
                generator,
            });
        }
        Ok(refs)
    }

    // Coin-delta assembly for an already-body-validated block whose record `prepare_delta`
    // derived (with the store-fallback retry around the ancestry walks — see the call site).
    fn derive_delta(
        &self,
        block: &FullBlock,
        record: BlockRecord,
        conds: Option<SpendBundleConditions>,
        additions: Vec<CoinRecord>,
        removals: Vec<Bytes32>,
    ) -> Result<BlockDelta, NodeError> {
        let height = block.height();
        let header_hash = block.header_hash()?;
        let prev_hash = block.prev_header_hash();
        let timestamp = block
            .foliage_transaction_block
            .as_ref()
            .map_or(0, |f| f.timestamp);
        // Create-coin hints for the coin_hint index, derived from the same conditions that
        // produced `additions` — `None` (empty) for a non-transaction / empty block.
        let hints = conds.as_ref().map(hints_for_conditions).unwrap_or_default();
        debug!(
            "block.derive_delta coins={} removals={} hints={}",
            additions.len(),
            removals.len(),
            hints.len()
        );
        Ok(BlockDelta {
            header_hash,
            prev_hash,
            height,
            weight: block.reward_chain_block.weight,
            timestamp,
            record,
            additions,
            removals,
            hints,
        })
    }

    #[allow(clippy::type_complexity)]
    fn validate_body(
        &self,
        block: &FullBlock,
        generator_refs: &[GeneratorReference],
        prev_tx: (u32, Option<u64>),
        pre: Option<PrecomputedBody>,
    ) -> Result<(Option<SpendBundleConditions>, Vec<CoinRecord>, Vec<Bytes32>), NodeError> {
        log::debug!("body.validate height={}", block.height());
        let ti = block.transactions_info.as_ref().ok_or_else(|| {
            NodeError::Invalid("transaction block missing transactions_info".into())
        })?;
        let generator = match block.transactions_generator.clone() {
            Some(g) => g,
            None => {
                // A transaction block MAY omit its generator (an empty transaction block): the
                // generator root must be zeroes, the ref list must be empty (refs_root = the
                // empty-list sentinel), the declared cost must be 0, and the aggregated
                // signature must verify over the empty spend set. Additions are the
                // incorporated reward claims only and there are no removals; conds is None.
                if ti.generator_root != Bytes32::default() {
                    return Err(ChiaError::InvalidTransactionsGeneratorHash.into());
                }
                if !block.transactions_generator_ref_list.is_empty() {
                    return Err(ChiaError::InvalidTransactionsGeneratorHash.into());
                }
                let refs_root =
                    transactions_generator_refs_root(&block.transactions_generator_ref_list)?;
                if refs_root != ti.generator_refs_root {
                    return Err(ChiaError::InvalidTransactionsGeneratorHash.into());
                }
                if ti.cost != 0 {
                    return Err(ChiaError::InvalidBlockCost.into());
                }
                if block.height() >= self.assume_valid {
                    let empty = SpendBundleConditions::default();
                    self.primitives.verify_block_aggregate_signature(
                        &empty,
                        &ti.aggregated_signature,
                        &self.constants,
                    )?;
                }
                let timestamp = block
                    .foliage_transaction_block
                    .as_ref()
                    .map_or(0, |f| f.timestamp);
                let additions = ti
                    .reward_claims_incorporated
                    .iter()
                    .map(|reward| coin_record(*reward, block.height(), timestamp, true))
                    .collect();
                return Ok((None, additions, Vec::new()));
            }
        };
        // SF9 body rules key on the PREVIOUS transaction block's height — the OPPOSITE keying
        // from the CLVM flag ladder's own-height rule.
        let sf9 = prev_tx.0 >= self.constants.soft_fork9_height;
        // TOO_MANY_GENERATOR_REFS: generator back-reference lists are banned past SF9, checked
        // before the generator runs.
        if sf9 && !block.transactions_generator_ref_list.is_empty() {
            return Err(ChiaError::TooManyGeneratorRefs.into());
        }
        // The expensive pure half — handed in by the window pipeline's parallel precompute when
        // its binding digest matches the refs THIS staging just resolved, run inline otherwise.
        // A precompute built against different ref bytes (a snapshot the chain moved under) can
        // therefore only degrade to a recompute, never change the verdict.
        let verify_sig = block.height() >= self.assume_valid;
        let pre = pre.filter(|p| {
            p.refs_digest == precompute_refs_digest(block.height(), generator_refs, verify_sig)
        });
        let (conds, sig_already_verified) = match pre {
            Some(p) => (p.conds, p.agg_sig_verified),
            None => run_body_expensive(
                &self.primitives,
                &self.constants,
                block,
                generator_refs,
                false,
            )?,
        };

        // Generator identity + cost against the block's own transactions_info.
        let gen_root = transactions_generator_root(&generator);
        if gen_root != ti.generator_root {
            return Err(ChiaError::InvalidTransactionsGeneratorHash.into());
        }
        // Compute refs_root over the ACTUAL referenced heights. A ti.generator_refs_root that
        // disagrees with the resolved ref-list is a rejection.
        let refs_root = transactions_generator_refs_root(&block.transactions_generator_ref_list)?;
        if refs_root != ti.generator_refs_root {
            return Err(ChiaError::InvalidTransactionsGeneratorHash.into());
        }
        if conds.cost != ti.cost {
            return Err(ChiaError::InvalidBlockCost.into());
        }
        // SF9 (INVALID_TRANSACTIONS_GENERATOR_ENCODING): canonical CLVM serialization, checked
        // after the cost comparison.
        if sf9 && !is_canonical_serialization(generator.as_ref()) {
            return Err(ChiaError::ComplexGeneratorReceived.into());
        }
        // SF9 (TOO_MANY_SPENDS): at most 6,000 spends per block.
        if sf9 && conds.spends.len() > MAX_SPENDS_PER_BLOCK {
            return Err(ChiaError::TooManySpends.into());
        }

        // Assume-valid: below the milestone the coin deltas are still derived from the generator (so
        // state stays exact) but the expensive script/sig checks are skipped. PoW headers are always validated
        // (that path runs in compute_record, not here), and the milestone default is 0 so genesis validates all.
        // The block-level condition checks (announcements, double-spend, time-locks) run in
        // `validate_body_coin_rules`, where the coin context for the spent-coin asserts is available.
        if block.height() >= self.assume_valid {
            // Seam: BLS aggregate verify — skipped only when the precompute already ran it.
            if !sig_already_verified {
                self.primitives.verify_block_aggregate_signature(
                    &conds,
                    &ti.aggregated_signature,
                    &self.constants,
                )?;
            }
        }

        let timestamp = block
            .foliage_transaction_block
            .as_ref()
            .map_or(0, |f| f.timestamp);
        let mut additions = Vec::new();
        for reward in &ti.reward_claims_incorporated {
            additions.push(coin_record(*reward, block.height(), timestamp, true));
        }
        for coin in additions_for_conditions(&conds, &[]) {
            additions.push(coin_record(coin, block.height(), timestamp, false));
        }
        let removals = removals_for_conditions(&conds);
        Ok((Some(conds), additions, removals))
    }

    // ---- Coin-store body validation (body rules 3, 5, 10-21) ----------------------------------
    //
    // These rules run on every transaction block on both the single-block and staged-window
    // paths, before record/header derivation. There is no body-validation skip window.

    // Lazily resolve whether the coin-store-backed rules are enforced (see `full_history`).
    // Only the POSITIVE decision is cached: full history is monotone (records are never
    // un-backfilled), while a negative answer can flip once an anchored store backfills to
    // genesis — so `false` is re-evaluated (one indexed MIN per transaction block).
    async fn coin_rules_enforced(&mut self, height: u32) -> Result<bool, NodeError> {
        if self.full_history == Some(true) {
            return Ok(true);
        }
        let v = match self.store.min_record_height().await? {
            Some(floor) => floor == 0,
            // Empty store: a genesis first block starts a full-history chain; any other first
            // block is a checkpoint/anchor entry point with no coin set below it.
            None => height == 0,
        };
        if v {
            self.full_history = Some(true);
        }
        Ok(v)
    }

    // The block-body rules that read the coin/record store, in rule order. The pure
    // structural rules (3, 10, 11, 12, 13, 14) run on every transaction block; the store-backed
    // rules (15-20 and the rule-21 coin context) require full coin history; rule 5 runs whenever
    // the record walk can complete (strictly under full history). Rule 4
    // (foliage_transaction_block hash binding) lives in header validation
    // (`core/src/consensus/block_header_validation.rs`).
    async fn validate_body_coin_rules(
        &mut self,
        block: &FullBlock,
        conds: Option<&SpendBundleConditions>,
        additions: &[CoinRecord],
        removals: &[Bytes32],
        prev_tx: (u32, Option<u64>),
    ) -> Result<(), NodeError> {
        let height = block.height();
        let ti = block.transactions_info.as_ref().ok_or_else(|| {
            NodeError::Invalid("transaction block missing transactions_info".into())
        })?;
        let ftb = block.foliage_transaction_block.as_ref().ok_or_else(|| {
            NodeError::Invalid("transaction block missing foliage_transaction_block".into())
        })?;

        // Rule 3: the foliage transaction block binds the transactions_info by hash.
        let ti_hash = transactions_info_hash(ti).map_err(NodeError::Consensus)?;
        if ftb.transactions_info_hash != ti_hash {
            return Err(ChiaError::InvalidTransactionsInfoHash.into());
        }

        let enforce = self.coin_rules_enforced(height).await?;

        // Rule 5: the incorporated reward claims are exactly the rewards owed for the previous
        // transaction block (with its fees) and the non-transaction blocks behind it.
        self.validate_reward_claims(block, ti, enforce).await?;

        // Rule 10: coin amount bounds (u64 rules out negative; the cap is a consensus constant).
        for a in additions {
            if a.coin.amount > self.constants.max_coin_amount {
                return Err(ChiaError::CoinAmountExceedsMaximum.into());
            }
        }

        // Rule 11: the foliage addition/removal merkle roots commit to the actual coin delta.
        let coins: Vec<Coin> = additions.iter().map(|a| a.coin).collect();
        let additions_root = canonical_additions_root(&coins)
            .map_err(|_| NodeError::Consensus(ChiaError::BadAdditionRoot))?;
        if ftb.additions_root != additions_root {
            return Err(ChiaError::BadAdditionRoot.into());
        }
        if ftb.removals_root != canonical_removals_root(removals) {
            return Err(ChiaError::BadRemovalRoot.into());
        }

        // Rule 12: the BIP158 transactions filter commits to every addition's puzzle hash and
        // every removal id — the same construction the producer builds (additions incl.
        // coinbase first, then removal names).
        let mut filter_items: Vec<Vec<u8>> = Vec::with_capacity(additions.len() + removals.len());
        for a in additions {
            filter_items.push(a.coin.puzzle_hash.bytes().to_vec());
        }
        for r in removals {
            filter_items.push(r.bytes().to_vec());
        }
        let filter_hash = Bytes32::new(hash_256(chia_block_filter(&filter_items)));
        if ftb.filter_hash != filter_hash {
            return Err(ChiaError::InvalidTransactionsFilterHash.into());
        }

        // Rules 13/14: duplicate outputs (including against coinbase additions) / duplicate spends.
        let mut addition_names: HashMap<Bytes32, &CoinRecord> =
            HashMap::with_capacity(additions.len());
        for a in additions {
            if addition_names.insert(a.coin.name(), a).is_some() {
                return Err(ChiaError::DuplicateOutput.into());
            }
        }
        let mut removal_names: HashSet<Bytes32> = HashSet::with_capacity(removals.len());
        for r in removals {
            if !removal_names.insert(*r) {
                return Err(ChiaError::DoubleSpend.into());
            }
        }

        // Rules 15-20 + the rule-21 coin context: the coin-store-backed half.
        let mut coin_context: HashMap<Bytes32, CoinSpendContext> = HashMap::new();
        if enforce {
            let fork = self.fork_view(block).await?;
            // Rule 15: every removal exists and is unspent on THIS branch (ephemeral coins from
            // this block, unapplied fork/window additions, or unspent store rows below the fork).
            let removal_records = self
                .lookup_removals(height, ftb.timestamp, &addition_names, removals, &fork)
                .await?;

            // Rule 16: no minting — total removed (actual stored amounts) covers total added.
            let removed: u128 = removal_records
                .values()
                .map(|r| u128::from(r.coin.amount))
                .sum();
            let added: u128 = additions
                .iter()
                .filter(|a| !a.coinbase)
                .map(|a| u128::from(a.coin.amount))
                .sum();
            if removed < added {
                return Err(ChiaError::MintingCoin.into());
            }
            let fees = removed - added;
            // Rule 17: the RESERVE_FEE sum must be covered by the actual fees.
            let reserve = conds.map_or(0, |c| c.reserve_fee);
            if fees < u128::from(reserve) {
                return Err(ChiaError::ReserveFeeConditionFailed.into());
            }
            // Rule 18: fees + this block's base farmer reward stays a representable coin amount.
            if fees + u128::from(calculate_base_farmer_reward(height))
                > u128::from(self.constants.max_coin_amount)
            {
                return Err(ChiaError::CoinAmountExceedsMaximum.into());
            }
            // Rule 19: the declared fee amount is the computed one.
            if u128::from(ti.fees) != fees {
                return Err(ChiaError::InvalidBlockFeeAmount.into());
            }
            // Rule 20: each removed coin's stored puzzle hash matches the spend's puzzle reveal.
            // (The coin id commits to the puzzle hash, so this is reachable only through a
            // corrupt store row.)
            if let Some(c) = conds {
                for spend in &c.spends {
                    if let Some(rec) = removal_records.get(&spend.coin_id)
                        && rec.coin.puzzle_hash != spend.puzzle_hash
                    {
                        return Err(ChiaError::WrongPuzzleHash.into());
                    }
                }
            }
            // Rule 21 context: birth height/timestamp of each spent coin, for the birth and
            // relative time-lock asserts (`validate_spend_context`).
            coin_context = removal_records
                .into_iter()
                .map(|(name, rec)| {
                    (
                        name,
                        CoinSpendContext {
                            birth_height: Some(rec.confirmed_block_index),
                            birth_seconds: Some(rec.timestamp),
                            spent_height: Some(rec.confirmed_block_index),
                            spent_seconds: Some(rec.timestamp),
                        },
                    )
                })
                .collect();
        }

        // Rule 21 + block-level condition checks (announcements, ephemeral/concurrent asserts,
        // absolute and relative time-locks). Time-locks validate against the PREVIOUS
        // transaction block's height/timestamp, never this block's own.
        // Assume-valid: below the milestone the script-output checks are skipped.
        if let Some(c) = conds
            && height >= self.assume_valid
        {
            let ctx = ConditionValidationContext {
                block_height: prev_tx.0,
                previous_transaction_block_timestamp: prev_tx.1,
                coin_context,
            };
            validate_block_conditions(c, &ctx)?;
        }
        Ok(())
    }

    // Rule 5 (`INVALID_REWARD_COINS`): walk from the foliage-declared previous transaction
    // block down through the non-transaction blocks behind it, computing the exact reward coins
    // this block must incorporate. `strict` (full history) makes a missing walk record a
    // rejection; otherwise (anchored store, walk reaching below the anchor) the rule is skipped
    // for this block.
    async fn validate_reward_claims(
        &self,
        block: &FullBlock,
        ti: &dg_xch_core::blockchain::transactions_info::TransactionsInfo,
        strict: bool,
    ) -> Result<(), NodeError> {
        let height = block.height();
        let mut expected: HashSet<Bytes32> = HashSet::new();
        if height > 0 {
            let ftb = block.foliage_transaction_block.as_ref().ok_or_else(|| {
                NodeError::Invalid("transaction block missing foliage_transaction_block".into())
            })?;
            let missing = |hash: Bytes32| {
                NodeError::Invalid(format!(
                    "reward-claim walk: missing block record {hash} below height {height}"
                ))
            };
            let Some(prev_tx_block) = self
                .record_for_walk(&ftb.prev_transaction_block_hash)
                .await?
            else {
                if strict {
                    return Err(missing(ftb.prev_transaction_block_hash));
                }
                debug!(
                    "reward-claim walk below the anchor; rule 5 skipped height={}",
                    height
                );
                return Ok(());
            };
            let fees = prev_tx_block.fees.unwrap_or(0);
            let farmer_amount = calculate_base_farmer_reward(prev_tx_block.height)
                .checked_add(fees)
                .ok_or(ChiaError::InvalidRewardCoins)?;
            expected.insert(
                create_pool_coin(
                    prev_tx_block.height,
                    prev_tx_block.pool_puzzle_hash,
                    calculate_pool_reward(prev_tx_block.height),
                    self.constants.genesis_challenge,
                )
                .name(),
            );
            expected.insert(
                create_farmer_coin(
                    prev_tx_block.height,
                    prev_tx_block.farmer_puzzle_hash,
                    farmer_amount,
                    self.constants.genesis_challenge,
                )
                .name(),
            );
            // For the second transaction block in the chain, don't go back further.
            if prev_tx_block.height > 0 {
                let mut cursor = prev_tx_block.prev_hash;
                loop {
                    let Some(curr) = self.record_for_walk(&cursor).await? else {
                        if strict {
                            return Err(missing(cursor));
                        }
                        debug!(
                            "reward-claim walk below the anchor; rule 5 skipped height={}",
                            height
                        );
                        return Ok(());
                    };
                    if curr.is_transaction_block() {
                        break;
                    }
                    expected.insert(
                        create_pool_coin(
                            curr.height,
                            curr.pool_puzzle_hash,
                            calculate_pool_reward(curr.height),
                            self.constants.genesis_challenge,
                        )
                        .name(),
                    );
                    expected.insert(
                        create_farmer_coin(
                            curr.height,
                            curr.farmer_puzzle_hash,
                            calculate_base_farmer_reward(curr.height),
                            self.constants.genesis_challenge,
                        )
                        .name(),
                    );
                    cursor = curr.prev_hash;
                }
            }
        }
        let claims: HashSet<Bytes32> = ti
            .reward_claims_incorporated
            .iter()
            .map(Coin::name)
            .collect();
        if claims != expected || ti.reward_claims_incorporated.len() != expected.len() {
            return Err(ChiaError::InvalidRewardCoins.into());
        }
        Ok(())
    }

    // A record by header hash for the body-validation walks: the walk cache first (staged records
    // are inserted there at stage time), then pending orphan-branch deltas, then the store.
    async fn record_for_walk(&self, hash: &Bytes32) -> Result<Option<BlockRecord>, NodeError> {
        if let Some(r) = self.cache.get(hash) {
            return Ok(Some(r.clone()));
        }
        if let Some(d) = self.pending.get(hash) {
            return Ok(Some(d.record.clone()));
        }
        Ok(self.store.get_block_record(hash).await?)
    }

    // Previous-TRANSACTION-block context (height, timestamp) for the CLVM flag ladder and the
    // time-lock conditions: ASSERT_HEIGHT/SECONDS validate against the previous transaction
    // block's height/timestamp, never this block's own. `candidate` is this block's own
    // headers-first record (if the fast-sync header pass stored one); it grounds the checkpoint
    // anchor when the local ancestor is missing (weight-proof-attested prev-tx height). Shared by
    // `prepare_delta` and the store-backed fork reconstruction (`delta_from_store`).
    async fn prev_tx_context(
        &self,
        prev: Option<&BlockRecord>,
        candidate: Option<&BlockRecord>,
    ) -> Result<(u32, Option<u64>), NodeError> {
        Ok(match prev {
            // Checkpoint anchor: no local ancestor. The candidate record's attested
            // prev-transaction-block height keeps the CLVM flag ladder and time-lock context in
            // the current fork regime instead of collapsing to height 0 (genesis flags).
            None => candidate.map_or((0u32, None), |c| (c.prev_transaction_block_height, None)),
            Some(p) if p.is_transaction_block() => (p.height, p.timestamp),
            Some(p) => {
                // Walk parent-ward through the record cache to the previous transaction block
                // before falling back to the store's main-chain read: in a staging window every
                // ancestor between two transaction blocks is a cache entry (`finish_stage`
                // inserts each staged record; confirm and the resume warm insert the recent
                // confirmed ancestry), so the walk resolves without an awaited store round-trip
                // per non-tx-parented block. The walk follows `prev_hash` (branch ancestry);
                // any cache gap falls back to the store read below.
                let target = p.prev_transaction_block_height;
                let mut walked: Option<(u32, Option<u64>)> = None;
                let mut cur = self.cache.get(&p.prev_hash);
                for _ in 0..MAX_PREV_TX_WALK {
                    match cur {
                        Some(r) if r.height == target => {
                            walked = Some((r.height, r.timestamp));
                            break;
                        }
                        Some(r) if r.height > target && r.height > 0 => {
                            cur = self.cache.get(&r.prev_hash);
                        }
                        _ => break,
                    }
                }
                match walked {
                    Some(ctx) => ctx,
                    None => match self.store.get_block_record_by_height(target).await? {
                        Some(r) => (r.height, r.timestamp),
                        None => (target, None),
                    },
                }
            }
        })
    }

    // Fold one already-validated branch delta into the fork view: its additions become
    // fork-visible unspent coins (keyed by name), its removals become fork-visible spends. Shared
    // by the in-memory-overlay and store-fallback halves of the fork walk.
    fn fold_delta_into_view(view: &mut ForkView, d: &BlockDelta) {
        for a in &d.additions {
            view.additions.insert(a.coin.name(), *a);
        }
        view.removals.extend(d.removals.iter().copied());
    }

    /// Rebuild a stored-but-non-confirmed branch block's already-validated coin delta from the
    /// DURABLE store (its persisted body + record). The in-memory `pending`/`staged_deltas`
    /// caches are bounded and lost on restart; the store is authoritative and unbounded, so a
    /// fork of ANY depth (and one that surfaces only after a process restart) can be
    /// reconstructed. The coin delta is re-derived exactly as the block was first validated, via
    /// the pure body half only, so there is no recursion into the fork view.
    ///
    /// # Errors
    /// [`NodeError::Invalid`] if the record or the body is absent from the store (a branch block
    /// must have been persisted by `persist_archive` when it first arrived); a body-validation
    /// error if the persisted body no longer validates; [`NodeError::Store`] on a store failure.
    async fn delta_from_store(&self, hash: &Bytes32) -> Result<BlockDelta, NodeError> {
        let record = self.store.get_block_record(hash).await?.ok_or_else(|| {
            NodeError::Invalid(format!("fork block {hash} has no record in the store"))
        })?;
        let block = self.store.get_block(hash).await?.ok_or_else(|| {
            NodeError::Invalid(format!(
                "fork block {hash} at height {} has no body in the store; cannot rebuild the fork \
                 coin context",
                record.height
            ))
        })?;
        let (conds, additions, removals) = if block.is_transaction_block() {
            let prev = self
                .prev_record_by(block.prev_header_hash(), block.height())
                .await?;
            let generator_refs = self
                .resolve_generator_refs(&block.transactions_generator_ref_list)
                .await?;
            let prev_tx = self.prev_tx_context(prev.as_ref(), Some(&record)).await?;
            self.validate_body(&block, &generator_refs, prev_tx, None)?
        } else {
            (None, Vec::new(), Vec::new())
        };
        self.derive_delta(&block, record, conds, additions, removals)
    }

    // Build the ForkView for `block`: walk parent-ward from its prev hash to the first CONFIRMED
    // ancestor — the fork point, at ANY depth — folding every unapplied ancestor's coin delta into
    // the view along the way. The delta comes from the in-memory overlay when present (staged
    // window blocks, then pending orphan branches) and otherwise is REBUILT FROM THE DURABLE
    // STORE (`delta_from_store`): walk store block-records to the fork and re-derive each fork
    // block's coin delta. There is no reorg-depth horizon — the in-memory caches are bounded and
    // volatile (lost on restart), the store is not, so a heavier valid branch is reconstructed
    // regardless of fork depth or a restart. A parent absent from both the overlay and the
    // store is the checkpoint-anchor bootstrap (pre-anchor state is trusted wholesale, fork view
    // empty). The walk streams one ancestor at a time (O(1) beyond the accumulated coin delta,
    // which the in-cache walk already accumulated); a non-strictly-decreasing height is a corrupt
    // store, surfaced as such — never a refusal of a valid fork.
    async fn fork_view(&self, block: &FullBlock) -> Result<ForkView, NodeError> {
        let height = block.height();
        let mut view = ForkView {
            fork_height: i64::from(height) - 1,
            additions: HashMap::new(),
            removals: HashSet::new(),
        };
        if height == 0 {
            view.fork_height = -1;
            return Ok(view);
        }
        let mut cursor = block.prev_header_hash();
        let mut last_height = height;
        loop {
            // In-memory overlay first: staged window blocks, then pending orphan-branch deltas.
            // Fold directly from the borrow (no clone on the hot in-window walk); copy out only the
            // parent hash + height before the borrow ends.
            if let Some(d) = self
                .staged_deltas
                .get(&cursor)
                .or_else(|| self.pending.get(&cursor))
            {
                Self::fold_delta_into_view(&mut view, d);
                if d.height == 0 {
                    view.fork_height = -1;
                    return Ok(view);
                }
                let (prev, d_height) = (d.prev_hash, d.height);
                cursor = Self::step_parent(cursor, prev, d_height, &mut last_height)?;
                continue;
            }
            match self.store.get_block_record(&cursor).await? {
                // The first CONFIRMED ancestor is the fork point, at any depth.
                Some(r) if self.is_confirmed(&cursor).await? => {
                    view.fork_height = i64::from(r.height);
                    return Ok(view);
                }
                // Stored but NON-confirmed: a branch block whose delta is not in the in-memory
                // caches (a restart, or a fork deeper than the in-memory window). Rebuild its
                // coin delta from the durable store and keep walking — no horizon.
                Some(_) => {
                    let d = self.delta_from_store(&cursor).await?;
                    Self::fold_delta_into_view(&mut view, &d);
                    if d.height == 0 {
                        view.fork_height = -1;
                        return Ok(view);
                    }
                    cursor = Self::step_parent(cursor, d.prev_hash, d.height, &mut last_height)?;
                }
                None => return Ok(view),
            }
        }
    }

    // One parent-ward step of a fork walk with cycle/corruption detection: the child at
    // `child_height` must have a strictly greater height than its parent, so the walk terminates
    // at the confirmed chain. A non-decreasing height is a corrupt store (a cycle), surfaced as a
    // store-corruption error — NOT a refusal of a valid fork (there is no reorg-depth cap).
    fn step_parent(
        at: Bytes32,
        prev: Bytes32,
        child_height: u32,
        last_height: &mut u32,
    ) -> Result<Bytes32, NodeError> {
        if child_height >= *last_height {
            return Err(NodeError::Invalid(format!(
                "fork coin context walk does not descend at {at} (height {child_height} \
                 >= {last_height}): corrupt store"
            )));
        }
        *last_height = child_height;
        Ok(prev)
    }

    // Rule 15 (`UNKNOWN_UNSPENT` / `DOUBLE_SPEND` / `DOUBLE_SPEND_IN_FORK`): resolve every
    // removal to the coin record it spends, in fixed precedence — ephemeral (created by this
    // very block), spent-in-fork rejection, then ONE batched store lookup interpreted against
    // the fork point, then the fork/window additions.
    async fn lookup_removals(
        &self,
        height: u32,
        timestamp: u64,
        additions_by_name: &HashMap<Bytes32, &CoinRecord>,
        removals: &[Bytes32],
        fork: &ForkView,
    ) -> Result<HashMap<Bytes32, CoinRecord>, NodeError> {
        let mut records: HashMap<Bytes32, CoinRecord> = HashMap::with_capacity(removals.len());
        let mut from_db: Vec<Bytes32> = Vec::new();
        for rem in removals {
            if let Some(created) = additions_by_name.get(rem) {
                // Ephemeral coin: created and spent inside this block.
                records.insert(
                    *rem,
                    CoinRecord {
                        coin: created.coin,
                        confirmed_block_index: height,
                        spent_block_index: height,
                        coinbase: false,
                        timestamp,
                        spent: false,
                    },
                );
            } else if fork.removals.contains(rem) {
                // Already spent by an unapplied ancestor on this branch/window.
                return Err(ChiaError::DoubleSpendInFork.into());
            } else {
                from_db.push(*rem);
            }
        }
        let found = if from_db.is_empty() {
            Vec::new()
        } else {
            self.store.get_coin_records(&from_db).await?
        };
        // Coins the store cannot answer for this branch: unknown rows, and rows confirmed after
        // the fork point (main-chain-only state) — both must come from the fork's own additions.
        let mut look_in_fork: Vec<Bytes32> = Vec::new();
        let mut found_names: HashSet<Bytes32> = HashSet::with_capacity(found.len());
        for rec in found {
            let name = rec.coin.name();
            found_names.insert(name);
            if i64::from(rec.confirmed_block_index) <= fork.fork_height {
                if rec.spent && i64::from(rec.spent_block_index) <= fork.fork_height {
                    // Spent by an ancestor block common to both chains.
                    return Err(ChiaError::DoubleSpend.into());
                }
                records.insert(name, rec);
            } else {
                look_in_fork.push(name);
            }
        }
        for rem in &from_db {
            if !found_names.contains(rem) {
                look_in_fork.push(*rem);
            }
        }
        for name in look_in_fork {
            let Some(created) = fork.additions.get(&name) else {
                return Err(ChiaError::UnknownUnspent.into());
            };
            records.insert(
                name,
                CoinRecord {
                    coin: created.coin,
                    confirmed_block_index: created.confirmed_block_index,
                    spent_block_index: 0,
                    coinbase: created.coinbase,
                    timestamp: created.timestamp,
                    spent: false,
                },
            );
        }
        Ok(records)
    }

    #[must_use]
    pub fn constants(&self) -> &ConsensusConstants {
        &self.constants
    }

    /// Light-path (proof-of-space only, no VDF) `required_iters` for a recent-chain header.
    /// Headers-first seeds each candidate record with this so the next block's full validation
    /// reads a correct `pb.ip_iters`.
    ///
    /// # Errors
    /// Returns [`NodeError::Invalid`] if the proof of space is invalid or an ancestor is absent.
    pub fn light_required_iters(
        &self,
        ancestors: &HashMap<Bytes32, BlockRecord>,
        header: &HeaderBlock,
        challenge: Bytes32,
        prev_challenge: Bytes32,
        overflow: bool,
        difficulty: u64,
    ) -> Result<u64, NodeError> {
        let rcb = &header.reward_chain_block;
        let cc_sp_hash = match &rcb.challenge_chain_sp_vdf {
            None => challenge,
            Some(v) => v.output.hash()?,
        };
        let pre = dg_xch_core::consensus::get_block_challenge::pre_sp_tx_block_height(
            &self.constants,
            ancestors,
            header.prev_header_hash(),
            rcb.signage_point_index,
            header.finished_sub_slots.len(),
        )
        .map_err(NodeError::Io)?;
        dg_xch_core::consensus::block_header_validation::validate_pospace_and_get_required_iters(
            &crate::header::PrimitiveVerifier(&self.primitives),
            &self.constants,
            &rcb.proof_of_space,
            if overflow { prev_challenge } else { challenge },
            cc_sp_hash,
            header.height(),
            difficulty,
            pre,
        )
        .map_err(NodeError::Io)?
        .ok_or_else(|| NodeError::Invalid(format!("invalid pospace at height {}", header.height())))
    }

    /// Full single-block PoW/VDF header validation against `ancestors`, returning `required_iters`.
    ///
    /// # Errors
    /// Returns [`NodeError::Invalid`] if any header check fails or a walked ancestor is absent.
    pub fn validate_header_block(
        &self,
        ancestors: &HashMap<Bytes32, BlockRecord>,
        block: &HeaderBlock,
        vs: ValidationState,
        check_sub_epoch_summary: bool,
    ) -> Result<u64, NodeError> {
        crate::header::validate_finished_header(
            &self.primitives,
            &self.constants,
            ancestors,
            block,
            vs,
            check_sub_epoch_summary,
        )
    }

    // As [`Engine::validate_header_block`], but with the VDF proofs deferred into `sink` when the
    // caller runs the cross-block window pipeline (verify-inline when `None`).
    fn validate_header_block_sinked(
        &self,
        ancestors: &HashMap<Bytes32, BlockRecord>,
        block: &HeaderBlock,
        vs: ValidationState,
        check_sub_epoch_summary: bool,
        sink: Option<&crate::header::HeaderSink>,
    ) -> Result<u64, NodeError> {
        match sink {
            None => self.validate_header_block(ancestors, block, vs, check_sub_epoch_summary),
            Some(sink) => crate::header::validate_finished_header_deferred(
                &self.primitives,
                &self.constants,
                ancestors,
                block,
                vs,
                check_sub_epoch_summary,
                sink,
            ),
        }
    }

    fn warm_ancestry(ancestors: &HashMap<Bytes32, BlockRecord>, p: &BlockRecord) -> bool {
        let mut sub_slots = 0usize;
        let mut tx_blocks = 0usize;
        let mut curr = p;
        loop {
            if let Some(hashes) = curr.finished_challenge_slot_hashes.as_ref() {
                sub_slots += hashes.len();
            }
            if curr.is_transaction_block() {
                tx_blocks += 1;
            }
            if (sub_slots > 2 && tx_blocks > 11) || curr.height == 0 {
                return true;
            }
            match ancestors.get(&curr.prev_hash) {
                Some(r) => curr = r,
                None => return false,
            }
        }
    }

    fn derive_required_iters(
        &self,
        header: &HeaderBlock,
        prev: Option<&BlockRecord>,
        vdf_sink: Option<&crate::header::HeaderSink>,
    ) -> Result<u64, NodeError> {
        let Some(p) = prev else {
            // Genesis: no ancestor, but its proof of space is validated like any other block —
            // the challenge is the block's declared pos_ss_cc_challenge_hash (which
            // get_block_challenge resolves to the genesis challenge) and the difficulty is the
            // genesis weight itself (there is no prev to subtract). A zero placeholder here
            // poisons the stored record: calculate_ip_iters rejects required_iters == 0 the
            // first time a challenge-block walk reads the genesis record (the
            // height-36 genesis-sync wall).
            let difficulty = u64::try_from(header.weight()).map_err(|_| {
                NodeError::Invalid("genesis weight does not fit a difficulty".into())
            })?;
            return self.pospace_required_iters_at(header, difficulty);
        };
        let ancestors = self.cache.records();
        if !ancestors.contains_key(&p.header_hash) || !ancestors.contains_key(&p.prev_hash) {
            return self.pospace_required_iters(header, p);
        }
        if !Self::warm_ancestry(ancestors, p) {
            return self.pospace_required_iters(header, p);
        }
        let is_first_in_sub_slot = !header.finished_sub_slots.is_empty();
        let (ssi, difficulty) = get_next_sub_slot_iters_and_difficulty(
            &self.constants,
            is_first_in_sub_slot,
            Some(p),
            ancestors,
        )
        .map_err(NodeError::Io)?;
        self.validate_header_block_sinked(
            ancestors,
            header,
            ValidationState { ssi, difficulty },
            false,
            vdf_sink,
        )
    }

    // Proof-of-space `required_iters` for a header, independent of the VDF/ancestor chain. The
    // block's difficulty is its weight increment over `prev` (block.weight == prev.weight +
    // difficulty, the INVALID_WEIGHT check), and the pospace challenge is the block's declared
    // pos_ss_cc_challenge_hash — exactly what the full header validator feeds to pospace after
    // confirming get_block_challenge equals it. So the value this returns equals the
    // full-validation path's required_iters for the same valid block, while needing none of its
    // deep ancestor walk. prev_transaction_block_height is unused by
    // validate_pospace_and_get_required_iters (dg_xch's pospace quality is height-based only), so 0 is passed.
    fn pospace_required_iters(
        &self,
        header: &HeaderBlock,
        prev: &BlockRecord,
    ) -> Result<u64, NodeError> {
        let difficulty = header
            .weight()
            .checked_sub(prev.weight)
            .and_then(|d| u64::try_from(d).ok())
            .ok_or_else(|| {
                NodeError::Invalid(format!(
                    "invalid block difficulty at height {} (weight does not increase over prev)",
                    header.height()
                ))
            })?;
        self.pospace_required_iters_at(header, difficulty)
    }

    fn pospace_required_iters_at(
        &self,
        header: &HeaderBlock,
        difficulty: u64,
    ) -> Result<u64, NodeError> {
        log::debug!("pospace height={}", header.height());
        let rcb = &header.reward_chain_block;
        let challenge = rcb.pos_ss_cc_challenge_hash;
        let cc_sp_hash = match &rcb.challenge_chain_sp_vdf {
            None => challenge,
            Some(v) => v.output.hash().map_err(NodeError::Io)?,
        };
        validate_pospace_and_get_required_iters(
            &crate::header::PrimitiveVerifier(&self.primitives),
            &self.constants,
            &rcb.proof_of_space,
            challenge,
            cc_sp_hash,
            header.height(),
            difficulty,
            0,
        )
        .map_err(NodeError::Io)?
        .ok_or_else(|| NodeError::Invalid(format!("invalid pospace at height {}", header.height())))
    }

    // Build the BlockRecord via core's header_block_to_sub_block_record with real deficit + required_iters.
    fn compute_record(
        &self,
        block: &FullBlock,
        prev: Option<&BlockRecord>,
        candidate: Option<&BlockRecord>,
        vdf_sink: Option<&crate::header::HeaderSink>,
    ) -> Result<BlockRecord, NodeError> {
        let header = header_block_from_full_block(block);
        self.compute_record_from_header(&header, prev, candidate, vdf_sink)
    }

    fn compute_record_from_header(
        &self,
        header: &HeaderBlock,
        prev: Option<&BlockRecord>,
        candidate: Option<&BlockRecord>,
        vdf_sink: Option<&crate::header::HeaderSink>,
    ) -> Result<BlockRecord, NodeError> {
        log::debug!("record.derive height={}", header.height());
        let overflow = is_overflow_block(
            &self.constants,
            header.reward_chain_block.signage_point_index,
        )?;
        // The record's sub_slot_iters: records are epoch-adjusted at creation, never
        // blind-inherited. When the local ancestry can run the retarget walk, run it. At a
        // weight-proof checkpoint the deep ancestry is absent; the attested value is the
        // headers-first candidate record's ssi (`candidate_ssi`). Blind inheritance is the
        // between-boundaries fallback; fabricating sub_slot_iters_starting mid-chain would
        // poison the anchor record and every descendant with the genesis constant.
        let ancestors = self.cache.records();
        let new_slot = !header.finished_sub_slots.is_empty();
        let candidate_ssi = candidate.map(|r| r.sub_slot_iters);
        let ssi = match prev {
            Some(p)
                if ancestors.contains_key(&p.header_hash)
                    && ancestors.contains_key(&p.prev_hash)
                    && Self::warm_ancestry(ancestors, p) =>
            {
                get_next_sub_slot_iters_and_difficulty(
                    &self.constants,
                    new_slot,
                    Some(p),
                    ancestors,
                )
                .map_err(NodeError::Io)?
                .0
            }
            Some(p) => candidate_ssi.unwrap_or(p.sub_slot_iters),
            None => candidate_ssi.unwrap_or(self.constants.sub_slot_iters_starting),
        };
        let declared_ses_hash = header
            .finished_sub_slots
            .iter()
            .find_map(|s| s.challenge_chain.subepoch_summary_hash);
        let ses = match declared_ses_hash {
            None => None,
            Some(expected) => {
                let first_cc = &header.finished_sub_slots[0].challenge_chain;
                let constructed = prev
                    .and_then(|p| ancestors.get(&p.prev_hash))
                    .and_then(|pp| {
                        make_sub_epoch_summary(
                            &self.constants,
                            ancestors,
                            header.height(),
                            pp,
                            first_cc.new_difficulty,
                            first_cc.new_sub_slot_iters,
                        )
                        .ok()
                    });
                // Third source, after local construction and the headers-first candidate: the
                // anchor's weight-proof summary chain. Summary i is included at the start of
                // sub-epoch i+1, so the entry for this boundary is at sub_epoch - 1; the
                // declared-hash check below still decides.
                let attested = || {
                    let sub_epoch = header.height() / self.constants.sub_epoch_blocks;
                    sub_epoch
                        .checked_sub(1)
                        .and_then(|i| self.summary_chain.get(i as usize).cloned())
                };
                let ses = match constructed {
                    Some(s) => s,
                    None => candidate
                        .and_then(|r| r.sub_epoch_summary_included)
                        .or_else(attested)
                        .ok_or_else(|| {
                            NodeError::Invalid(format!(
                                "cannot derive the included sub-epoch summary at height {} \
                                 (ancestry below the checkpoint and no attested candidate)",
                                header.height()
                            ))
                        })?,
                };
                if ses.hash().map_err(NodeError::Io)? != expected {
                    return Err(NodeError::Invalid(format!(
                        "INVALID_SUB_EPOCH_SUMMARY at height {}",
                        header.height()
                    )));
                }
                Some(ses)
            }
        };
        let required_iters = self.derive_required_iters(header, prev, vdf_sink)?;
        let deficit = calculate_deficit(
            &self.constants,
            header.height(),
            prev,
            overflow,
            header.finished_sub_slots.len(),
        );
        // At the checkpoint anchor (no local ancestor) the candidate record carries the
        // weight-proof-attested prev-transaction-block chain — without it the anchor stores
        // prev_transaction_block_height=0 and every seam derivation (CLVM flag ladder,
        // time-lock context) collapses to the genesis regime.
        let prev_tx_height = prev.map_or_else(
            || candidate.map_or(0, |r| r.prev_transaction_block_height),
            |p| {
                if p.is_transaction_block() {
                    p.height
                } else {
                    p.prev_transaction_block_height
                }
            },
        );
        let record = header_block_to_sub_block_record(
            &self.constants,
            required_iters,
            header,
            ssi,
            overflow,
            deficit,
            prev_tx_height,
            ses,
        )?;
        Ok(record)
    }

    // Fork choice over cumulative weight, then commit or reorg. Cache + pending updated after the store.
    // Takes ownership of the open per-block batch (carrying the record/body/status writes): the
    // linear-extend and genesis arms fold the coin deltas + peak flip into it and commit once; the
    // orphan and reorg arms seal the archive writes first (record+body durable before any reorg
    // machinery runs, matching the pre-batch ordering).
    async fn confirm(
        &mut self,
        batch: BatchHandle,
        delta: BlockDelta,
    ) -> Result<AddBlockOutcome, NodeError> {
        let peak = self.store.get_peak().await?;
        let outcome = match peak {
            None => {
                self.apply_on_chain(batch, &delta).await?;
                AddBlockOutcome::NewPeak {
                    height: delta.height,
                }
            }
            Some((peak_hh, peak_height)) => {
                let peak_weight = self.record_weight(&peak_hh).await?;
                if delta.weight <= peak_weight {
                    self.store.commit(batch).await?;
                    self.pending.insert(delta.header_hash, delta.clone());
                    self.prune_pending(delta.height);
                    AddBlockOutcome::Orphan {
                        height: delta.height,
                    }
                } else if delta.prev_hash == peak_hh {
                    self.apply_on_chain(batch, &delta).await?;
                    AddBlockOutcome::Extended {
                        height: delta.height,
                    }
                } else {
                    self.store.commit(batch).await?;
                    self.reorg(&delta, peak_height).await?
                }
            }
        };
        self.cache.insert(delta.record.clone());
        if !matches!(outcome, AddBlockOutcome::Orphan { .. }) {
            self.pending.remove(&delta.header_hash);
        }
        self.prune_pending(delta.height);
        Ok(outcome)
    }

    async fn apply_on_chain(
        &mut self,
        mut batch: BatchHandle,
        delta: &BlockDelta,
    ) -> Result<(), NodeError> {
        async {
            self.store
                .apply_block_in(
                    &mut batch,
                    delta.height,
                    delta.timestamp,
                    &delta.additions,
                    &delta.removals,
                )
                .await?;
            // coin_hint rows land in the SAME batch as the coin deltas; a no-op unless the hint
            // tier is built.
            self.store.apply_hints_in(&mut batch, &delta.hints).await?;
            self.store
                .set_peak_in(&mut batch, &delta.header_hash)
                .await?;
            self.store.commit(batch).await
        }
        .await?;
        Ok(())
    }

    // Reorg to a heavier branch: find the fork, revert coins above it (streamed, O(1) RAM),
    // re-apply the candidate branch's already-validated deltas in order, then flip confirmation
    // pointers to the new tip — ALL inside one store batch, so a crash anywhere means the reorg
    // never happened. Running it as N separate transactions would leave a crash window where
    // coins were reverted above the fork while the peak still pointed at the old branch. The
    // branch deltas were validated at arrival against their fork view; this replay applies those
    // fork-validated deltas unchanged.
    async fn reorg(
        &mut self,
        delta: &BlockDelta,
        old_peak_height: u32,
    ) -> Result<AddBlockOutcome, NodeError> {
        // A reorg's rollback (`rollback_to_in`: DELETE WHERE confirmed_index > $1 / UPDATE WHERE
        // spent_index > $1) and its rolled-back-state read (`rolled_back_coin_states`: per-height
        // confirmed_index/spent_index lookups) filter coin_record by columns whose indexes the SQL
        // backends DEFER to the sync->tip build (write-amp on the forward path). But a reorg can land
        // BELOW tip — a node stuck on a minority equal-weight tie-break branch must reorg to rejoin
        // the heavier chain, long before that build fires — and without the indexes each query
        // seq-scans the whole coin table — on a large table each rollback query scans every
        // row, stalling the reorg indefinitely. Ensure them here, idempotently, before any
        // rollback query runs.
        self.store.ensure_reorg_indexes().await?;
        self.pending.insert(delta.header_hash, delta.clone());
        let branch = self.candidate_branch(delta).await?;
        let fork_height = branch
            .first()
            .map_or(delta.height, |d| d.height)
            .saturating_sub(1);
        // The abandoned span's post-rollback coin states, read BEFORE the rollback mutates
        // them. Bounded by the reorg depth (≤ the pending horizon). A store failure here aborts
        // the reorg before any mutation.
        let rolled_back = self
            .store
            .rolled_back_coin_states(fork_height, old_peak_height)
            .await?;
        // Await-safe instrumentation: instrument the future rather than holding an Entered guard
        // across the store `.await`s below.
        log::debug!(
            "reorg fork_depth={} fork_height={}",
            branch.len(),
            fork_height
        );
        async move {
            let mut batch = self.store.begin().await?;
            self.store.rollback_to_in(&mut batch, fork_height).await?;
            for d in &branch {
                self.store
                    .apply_block_in(&mut batch, d.height, d.timestamp, &d.additions, &d.removals)
                    .await?;
                // coin_hint rows land in the SAME batch as the whole reorg; no-op without the
                // hint tier.
                self.store.apply_hints_in(&mut batch, &d.hints).await?;
            }
            let links = self
                .store
                .set_peak_in(&mut batch, &delta.header_hash)
                .await?;
            self.store.commit(batch).await?;
            for d in &branch {
                self.pending.remove(&d.header_hash);
            }
            self.reorg_reports.push_back(ReorgReport {
                fork_height,
                rolled_back,
                reapplied: branch.clone(),
            });
            if self.reorg_reports.len() > REORG_REPORT_CAP {
                self.reorg_reports.pop_front();
            }
            Ok(AddBlockOutcome::Reorg { fork_height, links })
        }
        .await
    }

    // Walk back from the new tip to the first confirmed ancestor; return the branch deltas
    // fork+1..=tip in height order. Each branch delta comes from `pending` when present and is
    // otherwise REBUILT FROM THE DURABLE STORE (`delta_from_store`) — the reorg-replay analog of
    // the store-backed fork walk in `fork_view`, so a reorg whose branch ancestors survive only in
    // the store (a restart, or a fork deeper than the pending horizon) re-applies the FULL
    // branch. The height guard makes the walk terminate at the confirmed chain and turns a
    // corrupt (cyclic) store into an error, never a truncated branch.
    async fn candidate_branch(&self, tip: &BlockDelta) -> Result<Vec<BlockDelta>, NodeError> {
        let mut branch = Vec::new();
        let mut cursor = tip.header_hash;
        let mut last_height = tip.height.saturating_add(1);
        loop {
            let d = match self.pending.get(&cursor) {
                Some(d) => d.clone(),
                None => self.delta_from_store(&cursor).await?,
            };
            let prev = d.prev_hash;
            let d_height = d.height;
            branch.push(d);
            if self.is_confirmed(&prev).await? {
                break;
            }
            cursor = Self::step_parent(cursor, prev, d_height, &mut last_height)?;
        }
        branch.reverse();
        Ok(branch)
    }

    async fn is_confirmed(&self, hash: &Bytes32) -> Result<bool, NodeError> {
        let Some(r) = self.store.get_block_record(hash).await? else {
            return Ok(false);
        };
        match self.store.get_block_record_by_height(r.height).await? {
            Some(confirmed) => Ok(confirmed.header_hash == *hash),
            None => Ok(false),
        }
    }

    async fn record_weight(&self, hash: &Bytes32) -> Result<u128, NodeError> {
        if let Some(r) = self.cache.get(hash) {
            return Ok(r.weight);
        }
        self.store
            .get_block_record(hash)
            .await?
            .map(|r| r.weight)
            .ok_or_else(|| {
                NodeError::Store(dg_xch_stores::StoreError::Corrupt(
                    "peak record missing".into(),
                ))
            })
    }

    fn prune_pending(&mut self, height: u32) {
        let floor = height.saturating_sub(self.horizon);
        self.pending.retain(|_, d| d.height >= floor);
    }
}

fn coin_record(coin: Coin, height: u32, timestamp: u64, coinbase: bool) -> CoinRecord {
    CoinRecord {
        coin,
        confirmed_block_index: height,
        spent_block_index: 0,
        coinbase,
        timestamp,
        spent: false,
    }
}

// Node-local FullBlock→HeaderBlock view for record computation. The two share every field the
// record needs; the transactions_filter is unused by header_block_to_sub_block_record. The
// filter default is the ENCODED-EMPTY filter b"\x00", never a zero-length byte string; the
// daemon's wallet-facing header serving overrides it with the real per-block filter.
pub fn header_block_from_full_block(block: &FullBlock) -> HeaderBlock {
    HeaderBlock {
        finished_sub_slots: block
            .finished_sub_slots
            .iter()
            .cloned()
            .map(sub_slot_to_end_of_sub_slot)
            .collect(),
        reward_chain_block: block.reward_chain_block.clone(),
        challenge_chain_sp_proof: block.challenge_chain_sp_proof.clone(),
        challenge_chain_ip_proof: block.challenge_chain_ip_proof.clone(),
        reward_chain_sp_proof: block.reward_chain_sp_proof.clone(),
        reward_chain_ip_proof: block.reward_chain_ip_proof.clone(),
        infused_challenge_chain_ip_proof: block.infused_challenge_chain_ip_proof.clone(),
        foliage: block.foliage,
        foliage_transaction_block: block.foliage_transaction_block,
        transactions_filter: UnsizedBytes::new(vec![0]),
        transactions_info: block.transactions_info.clone(),
    }
}

fn sub_slot_to_end_of_sub_slot(s: SubSlotBundle) -> EndOfSubSlotBundle {
    EndOfSubSlotBundle {
        challenge_chain: s.challenge_chain,
        infused_challenge_chain: s.infused_challenge_chain,
        reward_chain: s.reward_chain,
        proofs: s.proofs,
    }
}
