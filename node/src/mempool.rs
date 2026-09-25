use crate::fee_estimator::FeeEstimator;
use dg_xch_core::blockchain::coin::Coin;
use dg_xch_core::blockchain::coin_record::UnspentLineageInfo;
use dg_xch_core::blockchain::coin_spend::CoinSpend;
use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes96};
use dg_xch_core::blockchain::spend::{ELIGIBLE_FOR_DEDUP, ELIGIBLE_FOR_FF};
use dg_xch_core::blockchain::spend_bundle::SpendBundle;
use dg_xch_core::blockchain::spend_bundle_conditions::SpendBundleConditions;
use dg_xch_core::blockchain::tx_status::TXStatus;
use dg_xch_core::clvm::bls_bindings::aggregate_signatures;
use dg_xch_core::clvm::utils::is_clvm_canonical;
use dg_xch_core::consensus::block_generator::{
    BlockGeneratorFlags, BlockGeneratorInput, MAX_SPENDS_PER_BLOCK, additions_for_conditions,
    compressed_solution_generator_from_coin_spends, conditions_from_spend_bundle,
    execute_block_generator_result, removals_for_conditions, spend_bundle_generator_length,
};
use dg_xch_core::consensus::constants::ConsensusConstants;
use dg_xch_core::consensus::fast_forward::{fast_forward_singleton, supports_fast_forward};
use dg_xch_core::consensus::producer::BlockTransactions;
use dg_xch_stores::{CoinStore, StoreError};
use log::{info, warn};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt;
use std::time::{Duration, Instant};

// The minimum absolute fee bump a replacement must pay over the items it evicts — 0.00001 XCH,
// an anti-churn floor.
const MEMPOOL_MIN_FEE_INCREASE: u64 = 10_000_000;

// Bound on bundles parked for an unmet ASSERT_HEIGHT; lowest fee-per-cost evicts first when full.
const PENDING_CACHE_CAP: usize = 100;

// Bound on bundles rejected for a MEMPOOL_CONFLICT (they double-spend a coin an existing mempool
// item spends) and set aside for retry. Oldest-first (FIFO) eviction once either the summed cost
// clears one block's cost (`conflict_cache_max_cost`, set in `new`) or the item count clears this
// cap. Retried on every new peak: the conflicting resident may have left the pool unconfirmed
// (expiry / RBF), freeing the coin.
const CONFLICT_CACHE_MAX_SIZE: usize = 1000;

// No single item may pay more than this (2^50), so the sum of all fees stays clear of the
// signed-int64 ceiling. The companion sum guard is enforced too, with the same i64::MAX bound.
const MEMPOOL_ITEM_FEE_LIMIT: u64 = 1 << 50;

// The fee-per-cost floor an item must clear to displace anything when the pool is at capacity.
const NONZERO_FEE_MINIMUM_FPC: f64 = 5.0;

// The expiring-soon window: items whose effective ASSERT_BEFORE bound lands within 48 blocks /
// 900 seconds of the current peak collectively hold at most one block's cost.
const EXPIRING_BLOCK_CUTOFF: u32 = 48;
const EXPIRING_TIME_CUTOFF: u64 = 900;

// The per-spend penalty added to CLVM cost when computing virtual cost. Blocks are capped at
// 6,000 spends besides the cost limit, so pricing spend slots keeps many-spend low-cost bundles
// from crowding out the spend budget.
const SPEND_PENALTY_COST: u64 = 500_000;

// Once this many items have been skipped during block assembly, items carrying
// dedup/fast-forward spends are skipped outright — their processing is potentially expensive and
// the block is nearly full anyway.
const PRIORITY_TX_THRESHOLD: usize = 3;

// Bound on the FF lineage-lookup candidates scanned at admission; see
// `CoinStore::get_unspent_lineage_info`.

/// One coin spend of a resident mempool item, joined to its `SpendBundleConditions` spend.
/// Carries the dedup/fast-forward eligibility the conditions runner computed and, for a
/// fast-forward-capable spend, the singleton's latest unspent lineage (`None` ⇒ the spend is
/// pinned to its exact coin).
#[derive(Clone, Debug)]
pub struct BundleCoinSpend {
    pub coin_spend: CoinSpend,
    pub eligible_for_dedup: bool,
    pub additions: Vec<Coin>,
    // This spend's condition + execution cost (no byte cost) — the dedup saving.
    pub cost: u64,
    pub latest_singleton_lineage: Option<UnspentLineageInfo>,
}

impl BundleCoinSpend {
    /// The spend may be rebased iff a latest unspent singleton lineage was resolved for it.
    #[must_use]
    pub fn supports_fast_forward(&self) -> bool {
        self.latest_singleton_lineage.is_some()
    }

    fn coin_id(&self) -> Bytes32 {
        self.coin_spend.coin.name()
    }
}

// ---- block-assembly constants -------------------------------------------------------------------

// Maximum number of mempool items that can be skipped (not considered) during the creation of a
// block bundle. An item is skipped if it won't fit in the block we're trying to create; the loop
// breaks ON the tenth skip.
const MAX_SKIPPED_ITEMS: usize = 10;

// Typical cost of a standard XCH spend — the stop heuristic: once the remaining block budget
// drops below this, we're unlikely to find anything that fits.
const MIN_COST_THRESHOLD: u64 = 6_000_000;

// The block overhead the mempool's per-item cost accounting does not carry: the wrapping quote
// opcode's two serialized bytes' byte-cost plus its execution cost. The selection budget is
// `MAX_BLOCK_COST_CLVM - BLOCK_OVERHEAD`, so the assembled generator's true cost — item costs +
// this overhead, minus the per-item wrapper bytes shared once n > 1 — cannot exceed
// `MAX_BLOCK_COST_CLVM`.
const QUOTE_BYTES: u64 = 2;
const QUOTE_EXECUTION_COST: u64 = 20;

// The most restrictive absolute time-lock bounds of a bundle: the bundle-level absolutes plus
// every per-spend RELATIVE lock resolved against that removal's confirmed coin record. These are
// what the pending-drain boundary, the resident-expiry sweep, and can_replace's
// timelock-equality clauses run on — never the raw absolutes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EffectiveTimelocks {
    // height_absolute folded with confirmed_height + height_relative per spend (max);
    // 0 = unconstrained.
    pub assert_height: u32,
    pub assert_seconds: u64,
    pub assert_before_height: Option<u32>,
    pub assert_before_seconds: Option<u64>,
}

// Fold each spend's relative locks into the absolute bounds using the removal's
// (confirmed_height, timestamp). `records` must cover every removal — ephemeral
// (in-bundle-created) removals carry the synthesized peak+1/peak-timestamp record built in
// `admit`. Sums saturate: a saturated assert bound simply never satisfies, the same conservative
// outcome as an overflow error.
fn effective_timelocks(
    conds: &SpendBundleConditions,
    records: &HashMap<Bytes32, (u32, u64)>,
) -> EffectiveTimelocks {
    let mut tl = EffectiveTimelocks {
        assert_height: conds.height_absolute,
        assert_seconds: conds.seconds_absolute,
        assert_before_height: conds.before_height_absolute,
        assert_before_seconds: conds.before_seconds_absolute,
    };
    for spend in &conds.spends {
        let Some(&(confirmed, timestamp)) = records.get(&spend.coin_id) else {
            continue;
        };
        if let Some(rel) = spend.height_relative {
            tl.assert_height = tl.assert_height.max(confirmed.saturating_add(rel));
        }
        if let Some(rel) = spend.seconds_relative {
            tl.assert_seconds = tl.assert_seconds.max(timestamp.saturating_add(rel));
        }
        if let Some(rel) = spend.before_height_relative {
            let bound = confirmed.saturating_add(rel);
            tl.assert_before_height = Some(tl.assert_before_height.map_or(bound, |b| b.min(bound)));
        }
        if let Some(rel) = spend.before_seconds_relative {
            let bound = timestamp.saturating_add(rel);
            tl.assert_before_seconds =
                Some(tl.assert_before_seconds.map_or(bound, |b| b.min(bound)));
        }
    }
    tl
}

// The specific failing time-lock condition, named as the protocol's error code for that
// condition. Carried through [`MempoolError`] so the wire-facing rejects report the exact
// `TransactionAck.error` name string wallets match on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelockFailure {
    AssertHeightAbsolute,
    AssertHeightRelative,
    AssertSecondsAbsolute,
    AssertSecondsRelative,
    AssertMyBirthHeight,
    AssertMyBirthSeconds,
    AssertBeforeHeightAbsolute,
    AssertBeforeHeightRelative,
    AssertBeforeSecondsAbsolute,
    AssertBeforeSecondsRelative,
    ImpossibleHeightConstraints,
    ImpossibleSecondsConstraints,
}

impl TimelockFailure {
    /// The exact error-name string put in `TransactionAck.error` for this failure (wallets
    /// string-match these).
    #[must_use]
    pub fn chia_err_name(self) -> &'static str {
        match self {
            TimelockFailure::AssertHeightAbsolute => "ASSERT_HEIGHT_ABSOLUTE_FAILED",
            TimelockFailure::AssertHeightRelative => "ASSERT_HEIGHT_RELATIVE_FAILED",
            TimelockFailure::AssertSecondsAbsolute => "ASSERT_SECONDS_ABSOLUTE_FAILED",
            TimelockFailure::AssertSecondsRelative => "ASSERT_SECONDS_RELATIVE_FAILED",
            TimelockFailure::AssertMyBirthHeight => "ASSERT_MY_BIRTH_HEIGHT_FAILED",
            TimelockFailure::AssertMyBirthSeconds => "ASSERT_MY_BIRTH_SECONDS_FAILED",
            TimelockFailure::AssertBeforeHeightAbsolute => "ASSERT_BEFORE_HEIGHT_ABSOLUTE_FAILED",
            TimelockFailure::AssertBeforeHeightRelative => "ASSERT_BEFORE_HEIGHT_RELATIVE_FAILED",
            TimelockFailure::AssertBeforeSecondsAbsolute => "ASSERT_BEFORE_SECONDS_ABSOLUTE_FAILED",
            TimelockFailure::AssertBeforeSecondsRelative => "ASSERT_BEFORE_SECONDS_RELATIVE_FAILED",
            TimelockFailure::ImpossibleHeightConstraints => {
                "IMPOSSIBLE_HEIGHT_ABSOLUTE_CONSTRAINTS"
            }
            TimelockFailure::ImpossibleSecondsConstraints => {
                "IMPOSSIBLE_SECONDS_ABSOLUTE_CONSTRAINTS"
            }
        }
    }
}

// How a time-lock check failed — the pend-vs-fail split: only unmet height locks get PENDING;
// every other time-lock error is FAILED and the bundle is dropped, never cached. Each variant
// carries the specific failing condition for the named reject.
enum TimelockCheck {
    // An unmet height lock (absolute or relative): park for retry on a future peak.
    Park(TimelockFailure),
    // A passed ASSERT_BEFORE_* bound: dead on arrival.
    Expired(TimelockFailure),
    // An unmet seconds lock or a birth-assert mismatch: failed outright — seconds-based locks do
    // not park (the wallet resubmits).
    NotMet(TimelockFailure),
}

// Check every absolute, birth, and relative time/height condition against the previous
// transaction block's height and timestamp — which, for the mempool, are the current peak's.
// Check order is fixed so the same condition fails first.
fn check_time_locks(
    conds: &SpendBundleConditions,
    records: &HashMap<Bytes32, (u32, u64)>,
    prev_height: u32,
    prev_timestamp: u64,
) -> Option<TimelockCheck> {
    if prev_height < conds.height_absolute {
        return Some(TimelockCheck::Park(TimelockFailure::AssertHeightAbsolute));
    }
    if prev_timestamp < conds.seconds_absolute {
        return Some(TimelockCheck::NotMet(
            TimelockFailure::AssertSecondsAbsolute,
        ));
    }
    if let Some(bound) = conds.before_height_absolute
        && prev_height >= bound
    {
        return Some(TimelockCheck::Expired(
            TimelockFailure::AssertBeforeHeightAbsolute,
        ));
    }
    if let Some(bound) = conds.before_seconds_absolute
        && prev_timestamp >= bound
    {
        return Some(TimelockCheck::Expired(
            TimelockFailure::AssertBeforeSecondsAbsolute,
        ));
    }
    for spend in &conds.spends {
        let Some(&(confirmed, timestamp)) = records.get(&spend.coin_id) else {
            continue;
        };
        if let Some(birth) = spend.birth_height
            && birth != confirmed
        {
            return Some(TimelockCheck::NotMet(TimelockFailure::AssertMyBirthHeight));
        }
        if let Some(birth) = spend.birth_seconds
            && birth != timestamp
        {
            return Some(TimelockCheck::NotMet(TimelockFailure::AssertMyBirthSeconds));
        }
        if let Some(rel) = spend.height_relative
            && prev_height < confirmed.saturating_add(rel)
        {
            return Some(TimelockCheck::Park(TimelockFailure::AssertHeightRelative));
        }
        if let Some(rel) = spend.seconds_relative
            && prev_timestamp < timestamp.saturating_add(rel)
        {
            return Some(TimelockCheck::NotMet(
                TimelockFailure::AssertSecondsRelative,
            ));
        }
        if let Some(rel) = spend.before_height_relative
            && prev_height >= confirmed.saturating_add(rel)
        {
            return Some(TimelockCheck::Expired(
                TimelockFailure::AssertBeforeHeightRelative,
            ));
        }
        if let Some(rel) = spend.before_seconds_relative
            && prev_timestamp >= timestamp.saturating_add(rel)
        {
            return Some(TimelockCheck::Expired(
                TimelockFailure::AssertBeforeSecondsRelative,
            ));
        }
    }
    None
}

// Why a spend bundle could not enter the mempool. Every variant is a stated reason (a
// double-spend must be rejected *with a reason*).
#[derive(Debug)]
pub enum MempoolError {
    NoPeak,
    ZeroCost,
    CostExceedsMax(u64),
    UnknownUnspent(Bytes32),
    DoubleSpend(Bytes32),
    Conflict(Bytes32),
    // Parked for retry: an ASSERT_HEIGHT (absolute or relative) the current peak has not reached.
    // Not a failure — the bundle re-admits on a new peak. Only HEIGHT locks park; seconds-based
    // locks fail outright. Carries the failing condition for the named wire reject.
    Pending(Bytes32, TimelockFailure),
    // Dead on arrival: an ASSERT_BEFORE bound the chain has already passed.
    Expired(Bytes32, TimelockFailure),
    // An unmet ASSERT_SECONDS lock or a birth-assert mismatch: failed, never parked — the wallet
    // resubmits once wall-clock time catches up.
    TimelockNotMet(Bytes32, TimelockFailure),
    // assert_before_* <= assert_*: can never be satisfied at any height/time — rejected outright,
    // never parked.
    ImpossibleTimelock(Bytes32, TimelockFailure),
    FeeTooLow,
    // Pool at capacity and the fee-per-cost is below the nonzero floor — distinct from FeeTooLow.
    FeeNearZero,
    // A single item's fee above MEMPOOL_ITEM_FEE_LIMIT (2^50), or the pool's fee sum would clear
    // i64::MAX.
    FeeLimitExceeded,
    // A DEDUP-eligible coin spend whose solution is not canonically serialized: dedup identity is
    // byte identity, so every dedup solution must have exactly one representation.
    NonCanonicalSolution(Bytes32),
    // The bundle id is in the seen cache (recently validated, or known-invalid): no second
    // validation run.
    AlreadyIncluding(Bytes32),
    // The bundle is structurally unacceptable: every spend is a fast-forward spend (an FF spend
    // can only be evicted alongside a normal spend), or the bundle doesn't match its own
    // conditions.
    InvalidSpendBundle(&'static str),
    Name(String),
    Store(StoreError),
}

impl fmt::Display for MempoolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MempoolError::NoPeak => write!(f, "mempool has no peak yet"),
            MempoolError::ZeroCost => write!(f, "spend bundle has zero cost"),
            MempoolError::CostExceedsMax(c) => write!(f, "cost {c} exceeds max block cost"),
            MempoolError::UnknownUnspent(id) => write!(f, "removal {id} is unknown/unspent"),
            MempoolError::DoubleSpend(id) => write!(f, "coin {id} already spent at peak"),
            MempoolError::Conflict(id) => write!(f, "coin {id} already spent by a mempool item"),
            MempoolError::Pending(id, tf) => {
                write!(
                    f,
                    "bundle {id} parked until its ASSERT_HEIGHT is reached ({})",
                    tf.chia_err_name()
                )
            }
            MempoolError::Expired(id, tf) => {
                write!(
                    f,
                    "bundle {id} ASSERT_BEFORE bound already passed ({})",
                    tf.chia_err_name()
                )
            }
            MempoolError::TimelockNotMet(id, tf) => {
                write!(
                    f,
                    "bundle {id} has an unmet seconds/birth time-lock ({})",
                    tf.chia_err_name()
                )
            }
            MempoolError::ImpossibleTimelock(id, tf) => {
                write!(
                    f,
                    "bundle {id} carries impossible before<=assert time-lock constraints ({})",
                    tf.chia_err_name()
                )
            }
            MempoolError::FeeTooLow => write!(f, "fee too low to displace mempool at capacity"),
            MempoolError::FeeNearZero => {
                write!(f, "fee per cost below the nonzero minimum at capacity")
            }
            MempoolError::FeeLimitExceeded => {
                write!(f, "fee exceeds the per-item mempool fee limit")
            }
            MempoolError::NonCanonicalSolution(id) => {
                write!(f, "dedup-eligible spend {id} has a non-canonical solution")
            }
            MempoolError::AlreadyIncluding(id) => {
                write!(f, "bundle {id} was already seen; not revalidating")
            }
            MempoolError::InvalidSpendBundle(why) => {
                write!(f, "invalid spend bundle: {why}")
            }
            MempoolError::Name(e) => write!(f, "spend bundle name failed: {e}"),
            MempoolError::Store(e) => write!(f, "store error: {e}"),
        }
    }
}

impl MempoolError {
    /// The `(status, error name)` for this reject — the `TransactionAck` wire mapping. Exactly
    /// two reject classes are PENDING: an unmet ASSERT_HEIGHT lock (parked) and a losing
    /// MEMPOOL_CONFLICT (conflict-cached). Every other reject is FAILED.
    #[must_use]
    pub fn ack(&self) -> (TXStatus, &'static str) {
        match self {
            MempoolError::Conflict(_) => (TXStatus::PENDING, "MEMPOOL_CONFLICT"),
            MempoolError::Pending(_, tf) => (TXStatus::PENDING, tf.chia_err_name()),
            MempoolError::Expired(_, tf)
            | MempoolError::TimelockNotMet(_, tf)
            | MempoolError::ImpossibleTimelock(_, tf) => (TXStatus::FAILED, tf.chia_err_name()),
            MempoolError::NoPeak => (TXStatus::FAILED, "MEMPOOL_NOT_INITIALIZED"),
            // A zero-cost bundle maps to INVALID_SPEND_BUNDLE.
            MempoolError::ZeroCost => (TXStatus::FAILED, "INVALID_SPEND_BUNDLE"),
            MempoolError::CostExceedsMax(_) => (TXStatus::FAILED, "BLOCK_COST_EXCEEDS_MAX"),
            MempoolError::UnknownUnspent(_) => (TXStatus::FAILED, "UNKNOWN_UNSPENT"),
            MempoolError::DoubleSpend(_) => (TXStatus::FAILED, "DOUBLE_SPEND"),
            MempoolError::FeeTooLow => (TXStatus::FAILED, "INVALID_FEE_LOW_FEE"),
            MempoolError::FeeNearZero => (TXStatus::FAILED, "INVALID_FEE_TOO_CLOSE_TO_ZERO"),
            MempoolError::FeeLimitExceeded => (TXStatus::FAILED, "INVALID_BLOCK_FEE_AMOUNT"),
            MempoolError::NonCanonicalSolution(_) => (TXStatus::FAILED, "INVALID_COIN_SOLUTION"),
            MempoolError::AlreadyIncluding(_) => {
                (TXStatus::FAILED, "ALREADY_INCLUDING_TRANSACTION")
            }
            MempoolError::InvalidSpendBundle(_) => (TXStatus::FAILED, "INVALID_SPEND_BUNDLE"),
            MempoolError::Name(_) | MempoolError::Store(_) => (TXStatus::FAILED, "UNKNOWN"),
        }
    }
}

impl Error for MempoolError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            MempoolError::Store(e) => Some(e),
            _ => None,
        }
    }
}

impl From<StoreError> for MempoolError {
    fn from(e: StoreError) -> Self {
        MempoolError::Store(e)
    }
}

// One mempool item resolved for block assembly: its spends after fast-forward rebasing and
// identical-spend deduplication.
struct ProcessedItem {
    unique_spends: Vec<CoinSpend>,
    unique_additions: Vec<Coin>,
    // The cost the block saves by not repeating already-included identical spends.
    cost_saving: u64,
    // puzzle hash -> the singleton's lineage AFTER this item's spends — committed by the caller
    // iff the item is included.
    ff_update: HashMap<Bytes32, UnspentLineageInfo>,
    // Dedup entries discovered in this item: (coin id, solution bytes, per-spend cost) —
    // committed by the caller as soon as processing succeeds, before the fit checks.
    new_dedup: Vec<(Bytes32, Vec<u8>, u64)>,
}

enum ProcessError {
    // The item spends a dedup coin with a different solution than the one the block is
    // deduplicating on — skip the item WITHOUT charging the skip budget.
    SkipDedup(&'static str),
    // Any other failure: skip the item and charge the skip budget.
    Failed(String),
}

// Fast-forward + dedup processing for one item: rebase every FF spend onto the latest lineage
// (the committed block state first, else the item's own resolved lineage), re-validate the
// rebased bundle, then fold identical dedup spends into the block's dedup state.
fn process_item_spends(
    item: &MempoolItem,
    dedup_spends: &HashMap<Bytes32, (Vec<u8>, u64)>,
    ff_state: &HashMap<Bytes32, UnspentLineageInfo>,
    constants: &ConsensusConstants,
    height: u32,
) -> Result<ProcessedItem, ProcessError> {
    // ---- fast-forward pass ----------------------------------------------------------------------
    let mut post_ff: Vec<(CoinSpend, bool, Vec<Coin>, u64)> = Vec::new();
    let mut ff_update: HashMap<Bytes32, UnspentLineageInfo> = HashMap::new();
    let mut fast_forwarded = 0usize;
    for bcs in &item.bundle_coin_spends {
        if !bcs.supports_fast_forward() {
            post_ff.push((
                bcs.coin_spend.clone(),
                bcs.eligible_for_dedup,
                bcs.additions.clone(),
                bcs.cost,
            ));
            continue;
        }
        let puzzle_hash = bcs.coin_spend.coin.puzzle_hash;
        let amount = bcs.coin_spend.coin.amount;
        // Read only the committed block state here; absent that, the item's own lineage.
        let (target, already_chained) = match ff_state.get(&puzzle_hash) {
            Some(lineage) => (*lineage, true),
            None => {
                let Some(lineage) = bcs.latest_singleton_lineage else {
                    return Err(ProcessError::Failed("FF spend without lineage".to_string()));
                };
                (lineage, false)
            }
        };
        if !already_chained && target.coin_id == bcs.coin_id() {
            // We ARE the latest version: no rebase; record the NEXT version from our additions
            // so later FF spends chain.
            let Some(child) = bcs
                .additions
                .iter()
                .find(|c| c.puzzle_hash == puzzle_hash && c.amount == amount)
            else {
                return Err(ProcessError::Failed(
                    "could not find fast forward child singleton".to_string(),
                ));
            };
            ff_update.insert(
                puzzle_hash,
                UnspentLineageInfo {
                    coin_id: child.name(),
                    parent_id: child.parent_coin_info,
                    parent_parent_id: bcs.coin_spend.coin.parent_coin_info,
                },
            );
            post_ff.push((
                bcs.coin_spend.clone(),
                bcs.eligible_for_dedup,
                bcs.additions.clone(),
                bcs.cost,
            ));
            continue;
        }
        // Rebase: spend the latest version instead.
        let new_coin = Coin {
            parent_coin_info: target.parent_id,
            puzzle_hash,
            amount,
        };
        let new_parent = Coin {
            parent_coin_info: target.parent_parent_id,
            puzzle_hash,
            amount,
        };
        if new_coin.name() != target.coin_id || new_parent.name() != target.parent_id {
            return Err(ProcessError::Failed(
                "fast forward lineage does not reproduce its coin ids".to_string(),
            ));
        }
        let new_solution = fast_forward_singleton(&bcs.coin_spend, &new_coin, &new_parent)
            .map_err(|e| ProcessError::Failed(format!("fast forward rebase failed: {e:?}")))?;
        let mut singleton_child = None;
        let patched_additions: Vec<Coin> = bcs
            .additions
            .iter()
            .map(|addition| {
                let patched = Coin {
                    parent_coin_info: target.coin_id,
                    puzzle_hash: addition.puzzle_hash,
                    amount: addition.amount,
                };
                if addition.puzzle_hash == puzzle_hash && addition.amount == amount {
                    singleton_child = Some(patched);
                }
                patched
            })
            .collect();
        let Some(child) = singleton_child else {
            return Err(ProcessError::Failed(
                "could not find fast forward child singleton".to_string(),
            ));
        };
        ff_update.insert(
            puzzle_hash,
            UnspentLineageInfo {
                coin_id: child.name(),
                parent_id: child.parent_coin_info,
                parent_parent_id: target.parent_id,
            },
        );
        post_ff.push((
            CoinSpend {
                coin: new_coin,
                puzzle_reveal: bcs.coin_spend.puzzle_reveal.clone(),
                solution: new_solution,
            },
            bcs.eligible_for_dedup,
            patched_additions,
            bcs.cost,
        ));
        fast_forwarded += 1;
    }
    if fast_forwarded > 0 {
        // Re-run the rebased bundle to make sure it remains valid.
        let new_bundle = SpendBundle {
            coin_spends: post_ff.iter().map(|(spend, ..)| spend.clone()).collect(),
            aggregated_signature: item.bundle.aggregated_signature,
        };
        conditions_from_spend_bundle(&new_bundle, height, constants).map_err(|e| {
            ProcessError::Failed(format!(
                "item became invalid after singleton fast forward: {e:?}"
            ))
        })?;
    }
    // ---- dedup pass -----------------------------------------------------------------------------
    let mut unique_spends: Vec<CoinSpend> = Vec::new();
    let mut unique_additions: Vec<Coin> = Vec::new();
    let mut cost_saving: u64 = 0;
    let mut new_dedup: Vec<(Bytes32, Vec<u8>, u64)> = Vec::new();
    for (spend, eligible_for_dedup, spend_additions, spend_cost) in post_ff {
        if !eligible_for_dedup {
            unique_spends.push(spend);
            unique_additions.extend(spend_additions);
            continue;
        }
        let coin_id = spend.coin.name();
        match dedup_spends.get(&coin_id) {
            None => {
                new_dedup.push((coin_id, spend.solution.as_ref().to_vec(), spend_cost));
                unique_spends.push(spend);
                unique_additions.extend(spend_additions);
            }
            Some((solution, saved_cost)) => {
                if solution.as_slice() != spend.solution.as_ref() {
                    // Should not happen: check_removals rejects differing dedup solutions.
                    return Err(ProcessError::SkipDedup(
                        "solution differs from what the block deduplicates on",
                    ));
                }
                cost_saving = cost_saving.saturating_add(*saved_cost);
            }
        }
    }
    Ok(ProcessedItem {
        unique_spends,
        unique_additions,
        cost_saving,
        ff_update,
        new_dedup,
    })
}

// What a peak advance did to the pool: items dropped (spent/invalidated), items expired (an
// ASSERT_BEFORE bound the new peak passed), and parked bundles that became admissible —
// (name, cost, fee), the re-gossip announcement tuple.
pub struct NewPeakResult {
    pub dropped: usize,
    pub expired: usize,
    pub admitted: Vec<(Bytes32, u64, u64)>,
}

// A validated bundle resident in the mempool: the bundle itself (served back on RequestTransaction), its
// pre-run conditions, the derived fee/cost, and the removal coin ids (the conflict index key).
#[derive(Clone, Debug)]
pub struct MempoolItem {
    pub bundle: SpendBundle,
    pub conds: SpendBundleConditions,
    pub name: Bytes32,
    pub fee: u64,
    pub cost: u64,
    pub spends: usize,
    pub removals: Vec<Bytes32>,
    // Per-coin-spend dedup/FF metadata in the bundle's spend order (a Vec, not a map, so block
    // assembly is byte-deterministic). Empty when the bundle carries no coin spends
    // (test-synthesized conditions); such items behave as plain spends throughout.
    pub bundle_coin_spends: Vec<BundleCoinSpend>,
    // Effective time-locks computed at admission: the inputs to can_replace's equality clauses
    // and the new-peak expiry sweep.
    pub timelocks: EffectiveTimelocks,
    // The peak height when this item entered the mempool: the fee estimator's "blocks waited"
    // reference frame.
    pub height_added: u32,
    seq: u64,
}

impl MempoolItem {
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn fee_per_cost(&self) -> f64 {
        if self.cost == 0 {
            0.0
        } else {
            self.fee as f64 / self.cost as f64
        }
    }

    /// This item's dedup/FF metadata for `coin_id`.
    #[must_use]
    pub fn bundle_coin_spend(&self, coin_id: &Bytes32) -> Option<&BundleCoinSpend> {
        self.bundle_coin_spends
            .iter()
            .find(|bcs| bcs.coin_id() == *coin_id)
    }

    // The conflict-index keys for this item: each removal coin id, except that a fast-forward
    // spend indexes under its LATEST singleton coin id — that's the coin whose on-chain spend
    // must reach this item.
    fn index_keys(&self) -> Vec<Bytes32> {
        self.removals
            .iter()
            .map(|coin_id| {
                self.bundle_coin_spend(coin_id)
                    .and_then(|bcs| bcs.latest_singleton_lineage.as_ref())
                    .map_or(*coin_id, |lineage| lineage.coin_id)
            })
            .collect()
    }

    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn fee_per_virtual_cost(&self) -> f64 {
        let vcost = self
            .cost
            .saturating_add(self.spends as u64 * SPEND_PENALTY_COST);
        if vcost == 0 {
            0.0
        } else {
            self.fee as f64 / vcost as f64
        }
    }
}

pub struct Mempool {
    // MAX_BLOCK_COST_CLVM * MEMPOOL_BLOCK_BUFFER — the total-cost ceiling that triggers eviction.
    max_total_cost: u64,
    // Per-transaction cost cap: half a block's cost.
    max_tx_cost: u64,
    // MAX_BLOCK_COST_CLVM - BLOCK_OVERHEAD — the expiring-soon budget.
    max_block_clvm_cost: u64,
    total_cost: u64,
    // Sum of resident fees — the INVALID_BLOCK_FEE_AMOUNT overflow guard.
    total_fee: u64,
    // The most recent transaction block's (height, timestamp): the reference frame for every
    // time-lock check at admission.
    peak: Option<(u32, u64)>,
    items: HashMap<Bytes32, MempoolItem>,
    // index coin id -> owning item names: a coin may be spent by SEVERAL resident items
    // (identical dedup spends coexist; multiple FF spends of one singleton chain), and a
    // fast-forward spend indexes under its LATEST singleton coin id.
    by_coin: HashMap<Bytes32, Vec<Bytes32>>,
    // Monotonic insertion counter: FIFO tiebreak at equal priority.
    seq: u64,
    // Bundles parked for an unmet ASSERT_HEIGHT — absolute or relative, keyed by the EFFECTIVE
    // assert height; drained by `new_peak` once `assert_height <= peak`. Only height locks park:
    // seconds-based locks fail outright.
    pending: HashMap<Bytes32, PendingEntry>,
    // Bundles rejected for a MEMPOOL_CONFLICT (double-spend of a coin an existing mempool item
    // spends). `conflict_order` holds FIFO insertion order so eviction pops the oldest first;
    // `conflict_cost` tracks the summed cost against `conflict_cache_max_cost`. Drained whole
    // and re-admitted on each new peak (a resolved conflict re-admits, a still-conflicting one
    // re-caches, a now-double-spent one drops).
    conflict: HashMap<Bytes32, ConflictEntry>,
    conflict_order: std::collections::VecDeque<Bytes32>,
    conflict_cost: u64,
    conflict_cache_max_cost: u64,
    // The seen cache: recently-validated bundle ids (resident) plus known-invalid ones, so a
    // failed bundle isn't revalidated on every re-announce. FIFO-evicting.
    seen: HashSet<Bytes32>,
    seen_order: std::collections::VecDeque<Bytes32>,
    // The bitcoin-core-derived fee estimator: fed by add_to_pool / remove (non-block) / new_peak
    // (block inclusion). Answers the `get_fee_estimate` RPC and the `RequestFeeEstimates` wallet
    // handler. See node/src/fee_estimator.rs.
    fee_estimator: FeeEstimator,
}

const SEEN_CACHE_SIZE: usize = 10_000;

// A parked bundle: everything needed to retry admission plus the effective assert height (the
// drain key) and fee/cost (the re-gossip tuple + eviction order).
struct PendingEntry {
    bundle: SpendBundle,
    conds: SpendBundleConditions,
    fee: u64,
    cost: u64,
    assert_height: u32,
}

// A conflict-cached bundle: everything needed to re-run admission plus the fee/cost (the
// re-gossip tuple + FIFO eviction accounting). Unlike `PendingEntry` there is no drain key — the
// whole cache is retried on every new peak.
struct ConflictEntry {
    bundle: SpendBundle,
    conds: SpendBundleConditions,
    fee: u64,
    cost: u64,
}

impl Mempool {
    #[must_use]
    pub fn new(constants: &ConsensusConstants) -> Self {
        // A single transaction may cost at most half a block; the pool ceiling is
        // MAX_BLOCK_COST_CLVM * MEMPOOL_BLOCK_BUFFER.
        let max_tx_cost = constants.max_block_cost_clvm / 2;
        let block_overhead = QUOTE_BYTES * constants.cost_per_byte + QUOTE_EXECUTION_COST;
        let max_total_cost = constants
            .max_block_cost_clvm
            .saturating_mul(constants.mempool_block_buffer);
        Self {
            max_total_cost,
            max_tx_cost,
            max_block_clvm_cost: constants.max_block_cost_clvm.saturating_sub(block_overhead),
            total_cost: 0,
            total_fee: 0,
            peak: None,
            items: HashMap::new(),
            by_coin: HashMap::new(),
            seq: 0,
            pending: HashMap::new(),
            conflict: HashMap::new(),
            conflict_order: std::collections::VecDeque::new(),
            conflict_cost: 0,
            // The conflict-cache cost cap is exactly one block's cost.
            conflict_cache_max_cost: constants.max_block_cost_clvm,
            seen: HashSet::new(),
            seen_order: std::collections::VecDeque::new(),
            fee_estimator: FeeEstimator::new(max_total_cost),
        }
    }

    /// Record a bundle id in the bounded seen cache, FIFO-evicting past 10,000 entries.
    pub fn add_seen(&mut self, name: Bytes32) {
        if self.seen.insert(name) {
            self.seen_order.push_back(name);
            while self.seen_order.len() > SEEN_CACHE_SIZE {
                if let Some(evicted) = self.seen_order.pop_front() {
                    self.seen.remove(&evicted);
                }
            }
        }
    }

    /// Whether this bundle id was validated recently (resident or known-invalid).
    #[must_use]
    pub fn seen(&self, name: &Bytes32) -> bool {
        self.seen.contains(name)
    }

    pub fn remove_seen(&mut self, name: &Bytes32) {
        if self.seen.remove(name) {
            self.seen_order.retain(|entry| entry != name);
        }
    }

    // Set the reference frame for time-lock checks: the most recent TRANSACTION block's height
    // and timestamp — for the next block to be farmed, that peak IS the previous transaction
    // block the locks validate against.
    pub fn set_peak(&mut self, height: u32, timestamp: u64) {
        self.peak = Some((height, timestamp));
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    #[must_use]
    pub fn total_cost(&self) -> u64 {
        self.total_cost
    }

    /// Sum of resident fees.
    #[must_use]
    pub fn total_fees(&self) -> u64 {
        self.total_fee
    }

    /// Count of bundles held in the conflict cache — rejected for a MEMPOOL_CONFLICT and awaiting
    /// retry on a future peak.
    #[must_use]
    pub fn conflict_cache_len(&self) -> usize {
        self.conflict.len()
    }

    /// Summed cost of the conflict-cached bundles.
    #[must_use]
    pub fn conflict_cache_cost(&self) -> u64 {
        self.conflict_cost
    }

    #[must_use]
    pub fn max_total_cost(&self) -> u64 {
        self.max_total_cost
    }

    /// The fee estimator — read surface for the `get_fee_estimate` RPC and the
    /// `RequestFeeEstimates` wallet handler.
    #[must_use]
    pub fn fee_estimator(&self) -> &FeeEstimator {
        &self.fee_estimator
    }

    /// The fee estimator, mutably — for the block-inclusion/eviction feed and for tests that drive
    /// confirmations directly through [`FeeEstimator::ingest_block`].
    pub fn fee_estimator_mut(&mut self) -> &mut FeeEstimator {
        &mut self.fee_estimator
    }

    /// Whether a transaction of `cost` cannot fit without eviction.
    #[must_use]
    pub fn at_full_capacity(&self, cost: u64) -> bool {
        self.total_cost.saturating_add(cost) > self.max_total_cost
    }

    /// The minimum fee-per-cost a transaction of `cost` must beat to get in — 0 while there's
    /// room, otherwise the fee-per-cost of the last item that would have to be evicted (walking
    /// lowest fee-per-cost first), `None` if it can't fit even after evicting everything.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn get_min_fee_rate(&self, cost: u64) -> Option<f64> {
        if !self.at_full_capacity(cost) {
            return Some(0.0);
        }
        // ORDER BY fee_per_cost ASC, seq DESC.
        let mut candidates: Vec<(f64, u64, u64)> = self
            .items
            .values()
            .map(|i| (i.fee_per_cost(), i.seq, i.cost))
            .collect();
        candidates.sort_by(|a, b| {
            a.0.partial_cmp(&b.0)
                .unwrap_or(Ordering::Equal)
                .then(b.1.cmp(&a.1))
        });
        let mut current_cost = self.total_cost;
        for (fee_per_cost, _, item_cost) in candidates {
            current_cost -= item_cost;
            if current_cost.saturating_add(cost) <= self.max_total_cost {
                return Some(fee_per_cost);
            }
        }
        None
    }

    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn is_fee_enough(&self, fees: u64, cost: u64) -> bool {
        if cost == 0 {
            return false;
        }
        if !self.at_full_capacity(cost) {
            return true;
        }
        let fees_per_cost = fees as f64 / cost as f64;
        if fees_per_cost < NONZERO_FEE_MINIMUM_FPC {
            return false;
        }
        self.get_min_fee_rate(cost)
            .is_some_and(|min_fee_rate| fees_per_cost > min_fee_rate)
    }

    #[must_use]
    pub fn get(&self, name: &Bytes32) -> Option<&MempoolItem> {
        self.items.get(name)
    }

    // The spend bundle for a name, served back on RequestTransaction.
    #[must_use]
    pub fn spend_bundle(&self, name: &Bytes32) -> Option<SpendBundle> {
        self.items.get(name).map(|i| i.bundle.clone())
    }

    /// Validate `bundle` (with its pre-run `conds`) against the peak + pool and, if valid, admit
    /// it in fee order — evicting the lowest fee-per-cost items if it would exceed capacity.
    /// Returns the bundle name.
    ///
    /// # Errors
    /// Returns the specific [`MempoolError`] (double-spend, conflict, unknown-unspent, cost, fee-too-low)
    /// if the bundle cannot enter, or [`MempoolError::Store`] on a store failure.
    pub async fn admit<S: CoinStore + Sync + ?Sized>(
        &mut self,
        store: &S,
        bundle: SpendBundle,
        conds: SpendBundleConditions,
    ) -> Result<Bytes32, MempoolError> {
        let name = bundle
            .name()
            .map_err(|e| MempoolError::Name(e.to_string()))?;
        if self.items.contains_key(&name) {
            return Ok(name); // idempotent: already resident
        }
        let Some((peak_height, peak_timestamp)) = self.peak else {
            return Err(MempoolError::NoPeak);
        };
        let cost = conds.cost;
        if cost == 0 {
            return Err(MempoolError::ZeroCost);
        }
        if cost > self.max_tx_cost {
            return Err(MempoolError::CostExceedsMax(cost));
        }

        let removals = removals_for_conditions(&conds);
        let created: HashSet<Bytes32> = additions_for_conditions(&conds, &[])
            .iter()
            .map(Coin::name)
            .collect();

        // 0. Per-spend dedup/FF metadata: join each CoinSpend to its conditions spend, enforce
        //    canonical solutions on DEDUP-eligible spends, resolve the latest unspent singleton
        //    lineage for FF-eligible spends, and reject all-fast-forward bundles (an FF spend can
        //    only be evicted alongside a normal spend). A bundle with no coin spends
        //    (test-synthesized conditions) yields an empty vec and plain-spend behavior.
        let bundle_coin_spends = self
            .build_bundle_coin_spends(store, &bundle, &conds)
            .await?;

        // 1. Removals against the confirmed set at peak: a spent record is a double-spend; a removal
        //    that is neither on-chain nor created in-bundle (ephemeral) is unknown-unspent. The
        //    fetched records double as the time-lock reference frame below — one query, both uses.
        let store_records = store.get_coin_records(&removals).await?;
        // (confirmed_height, timestamp) per removal — the inputs the relative/birth locks are
        // resolved against.
        let mut records: HashMap<Bytes32, (u32, u64)> = store_records
            .iter()
            .map(|r| (r.coin.name(), (r.confirmed_block_index, r.timestamp)))
            .collect();
        for coin_id in &removals {
            if !records.contains_key(coin_id) {
                if created.contains(coin_id) {
                    // Ephemeral removal: synthesize a coin record confirmed at peak+1 with the
                    // PEAK's timestamp — all spends land simultaneously, so an ephemeral
                    // ASSERT_SECONDS_RELATIVE 0 still passes.
                    records.insert(*coin_id, (peak_height.saturating_add(1), peak_timestamp));
                } else {
                    return Err(MempoolError::UnknownUnspent(*coin_id));
                }
            }
        }

        // 1b. Fee gates: fees right after the removal records, BEFORE conflict classification and
        //     time-locks.
        let fee = u64::try_from(conds.removal_amount.saturating_sub(conds.addition_amount))
            .unwrap_or(u64::MAX);
        // The per-item fee cap (2^50) plus the signed-int64 fee-sum headroom guard.
        if fee > MEMPOOL_ITEM_FEE_LIMIT
            || u64::try_from(i64::MAX).unwrap_or(u64::MAX) - self.total_fee <= fee
        {
            return Err(MempoolError::FeeLimitExceeded);
        }
        // At capacity, the fee must clear the nonzero floor and strictly beat the min fee rate —
        // checked BEFORE any expensive work, regardless of what the insertion-time eviction would
        // find.
        if self.at_full_capacity(cost) {
            #[allow(clippy::cast_precision_loss)]
            let fees_per_cost = fee as f64 / cost as f64;
            if fees_per_cost < NONZERO_FEE_MINIMUM_FPC {
                return Err(MempoolError::FeeNearZero);
            }
            // Unreachable in practice: max_tx_cost (half a block) always fits an emptied pool —
            // fold into FeeTooLow.
            let Some(min_fee_rate) = self.get_min_fee_rate(cost) else {
                return Err(MempoolError::FeeTooLow);
            };
            if fees_per_cost <= min_fee_rate {
                return Err(MempoolError::FeeTooLow);
            }
        }

        // 1c. Double-spend against the confirmed set, AFTER the fee gates: a SPENT removal is
        //     fine iff the spend supports fast-forward (the singleton has a newer unspent version
        //     to rebase onto); anything else is a double-spend.
        for r in &store_records {
            if r.spent {
                let coin_id = r.coin.name();
                let ff = bundle_coin_spends
                    .iter()
                    .find(|bcs| bcs.coin_id() == coin_id)
                    .is_some_and(BundleCoinSpend::supports_fast_forward);
                if !ff {
                    return Err(MempoolError::DoubleSpend(coin_id));
                }
            }
        }

        // 2. Time-locks: effective bounds, the impossible-constraint rejects, then the
        //    pend-vs-fail split — all against the peak's height AND timestamp.
        let timelocks = effective_timelocks(&conds, &records);
        if timelocks
            .assert_before_height
            .is_some_and(|b| b <= timelocks.assert_height)
        {
            // Impossible constraints fail outright — never parked, even though the unmet assert
            // alone would pend.
            return Err(MempoolError::ImpossibleTimelock(
                name,
                TimelockFailure::ImpossibleHeightConstraints,
            ));
        }
        if timelocks
            .assert_before_seconds
            .is_some_and(|b| b <= timelocks.assert_seconds)
        {
            return Err(MempoolError::ImpossibleTimelock(
                name,
                TimelockFailure::ImpossibleSecondsConstraints,
            ));
        }
        match check_time_locks(&conds, &records, peak_height, peak_timestamp) {
            Some(TimelockCheck::Park(tf)) => {
                self.park_pending(name, bundle, conds, timelocks.assert_height);
                return Err(MempoolError::Pending(name, tf));
            }
            Some(TimelockCheck::Expired(tf)) => return Err(MempoolError::Expired(name, tf)),
            Some(TimelockCheck::NotMet(tf)) => return Err(MempoolError::TimelockNotMet(name, tf)),
            None => {}
        }

        // 3. Conflict against resident items spending the same coins — FF/dedup-aware
        //    classification: a spent coin is NOT a conflict when both sides can chain (two FF
        //    spends of one singleton) or merge (identical DEDUP solutions); everything else falls
        //    to replace-by-fee (`can_replace`) and, failing that, a Conflict reject.
        let mut conflict_names: Vec<Bytes32> = Vec::new();
        for coin_id in &removals {
            let Some(owners) = self.by_coin.get(coin_id) else {
                continue;
            };
            let new_bcs = bundle_coin_spends.iter().find(|b| b.coin_id() == *coin_id);
            let new_ff = new_bcs.is_some_and(BundleCoinSpend::supports_fast_forward);
            let new_dedup = new_bcs.is_some_and(|b| b.eligible_for_dedup);
            for owner in owners.clone() {
                if conflict_names.contains(&owner) {
                    continue;
                }
                let Some(item) = self.items.get(&owner) else {
                    continue;
                };
                // The pool item's spend of this coin: direct, or the FF spend whose latest
                // singleton version IS this coin.
                let conflict_bcs = item.bundle_coin_spend(coin_id).or_else(|| {
                    item.bundle_coin_spends.iter().find(|b| {
                        b.latest_singleton_lineage
                            .as_ref()
                            .is_some_and(|lineage| lineage.coin_id == *coin_id)
                    })
                });
                let is_conflict = match conflict_bcs {
                    None if item.bundle_coin_spends.is_empty() => true, // plain (synthetic) item
                    None => {
                        // Not expected but handled gracefully.
                        warn!(
                            "coin indexed but not found in mempool item coin_id={} item={}",
                            coin_id, owner
                        );
                        return Err(MempoolError::InvalidSpendBundle(
                            "indexed coin missing from mempool item",
                        ));
                    }
                    Some(existing) => {
                        if !new_ff && !new_dedup {
                            // a plain spend conflicts with everything
                            true
                        } else if new_ff && !existing.supports_fast_forward() {
                            // FF cannot chain onto a non-FF spend
                            true
                        } else if new_dedup && !existing.eligible_for_dedup {
                            // dedup cannot merge with a non-dedup spend
                            true
                        } else {
                            // dedup identity is byte identity of the solutions
                            new_dedup
                                && new_bcs.is_some_and(|b| {
                                    b.coin_spend.solution.as_ref()
                                        != existing.coin_spend.solution.as_ref()
                                })
                        }
                    }
                };
                if is_conflict {
                    conflict_names.push(owner);
                }
            }
        }
        if !conflict_names.is_empty() {
            let removal_set: HashSet<Bytes32> = removals.iter().copied().collect();
            if !self.can_replace(
                &conflict_names,
                &removal_set,
                fee,
                cost,
                &timelocks,
                &bundle_coin_spends,
            ) {
                let first = removals
                    .iter()
                    .find(|c| self.by_coin.contains_key(*c))
                    .copied()
                    .unwrap_or_default();
                // A MEMPOOL_CONFLICT result sets the bundle aside in the conflict cache rather
                // than dropping it — the conflicting resident may leave the pool unconfirmed
                // (expire, or be replaced by a higher-fee tx) and free the coin, at which point
                // new_peak's conflict drain re-admits this bundle. The wire ack is still
                // PENDING/MEMPOOL_CONFLICT.
                self.cache_conflict(name, bundle, conds, fee, cost);
                return Err(MempoolError::Conflict(first));
            }
            for name in &conflict_names {
                self.remove(name);
            }
        }
        let seq = self.seq;
        self.seq += 1;
        let spends = conds.spends.len();
        let item = MempoolItem {
            bundle,
            conds,
            name,
            fee,
            cost,
            spends,
            removals,
            bundle_coin_spends,
            timelocks,
            // The peak height at admission time.
            height_added: peak_height,
            seq,
        };

        self.add_to_pool(item)?;
        Ok(name)
    }

    // Join every CoinSpend to its SpendBundleConditions spend, enforce solution canonicality on
    // DEDUP-eligible spends, resolve the latest unspent singleton lineage for spends that are
    // FF-eligible AND structurally fast-forwardable, and reject a bundle whose spends are ALL
    // fast-forward.
    async fn build_bundle_coin_spends<S: CoinStore + Sync + ?Sized>(
        &self,
        store: &S,
        bundle: &SpendBundle,
        conds: &SpendBundleConditions,
    ) -> Result<Vec<BundleCoinSpend>, MempoolError> {
        if bundle.coin_spends.is_empty() {
            return Ok(Vec::new());
        }
        let mut out: Vec<BundleCoinSpend> = Vec::with_capacity(bundle.coin_spends.len());
        for coin_spend in &bundle.coin_spends {
            let coin_id = coin_spend.coin.name();
            // if this coin isn't in the conditions, the bundle doesn't match its conditions
            let Some(spend_conds) = conds.spends.iter().find(|s| s.coin_id == coin_id) else {
                return Err(MempoolError::InvalidSpendBundle(
                    "coin spend missing from conditions",
                ));
            };
            if (spend_conds.flags & ELIGIBLE_FOR_DEDUP) != 0
                && !is_clvm_canonical(coin_spend.solution.as_ref())
            {
                return Err(MempoolError::NonCanonicalSolution(coin_id));
            }
            let mut lineage = None;
            if (spend_conds.flags & ELIGIBLE_FOR_FF) != 0 && supports_fast_forward(coin_spend) {
                // The singleton must still have an unspent version; if it was fully spent in a
                // non-FF way this spend can never become valid, so it degrades to a normal spend
                // requiring its exact coin unspent.
                lineage = store
                    .get_unspent_lineage_info(&spend_conds.puzzle_hash)
                    .await?;
            }
            let additions: Vec<Coin> = spend_conds
                .create_coin
                .iter()
                .map(|new_coin| Coin {
                    parent_coin_info: coin_id,
                    puzzle_hash: new_coin.puzzle_hash,
                    amount: new_coin.amount,
                })
                .collect();
            out.push(BundleCoinSpend {
                coin_spend: coin_spend.clone(),
                eligible_for_dedup: (spend_conds.flags & ELIGIBLE_FOR_DEDUP) != 0,
                additions,
                cost: spend_conds
                    .condition_cost
                    .saturating_add(spend_conds.execution_cost),
                latest_singleton_lineage: lineage,
            });
        }
        // Fast-forward spends are only allowed bundled with other, non-FF spends: to evict an FF
        // spend it must ride with a normal spend a block can invalidate.
        if out.iter().all(BundleCoinSpend::supports_fast_forward) {
            return Err(MempoolError::InvalidSpendBundle(
                "all spends are fast-forward",
            ));
        }
        Ok(out)
    }

    // The expiring-soon budget sweep, then the POOL_FULL eviction of the lowest-priority items
    // until the incoming item fits. The fee-rate admission gates already ran — eviction here does
    // NOT re-compare against the incoming fee.
    fn add_to_pool(&mut self, item: MempoolItem) -> Result<(), MempoolError> {
        let (peak_height, peak_timestamp) = self.peak.unwrap_or((0, 0));
        // Expiring-soon budget: if the incoming item expires within 48 blocks / 900 seconds, all
        // such items together may hold at most one block's cost. Expiring items are ordered
        // (priority DESC, seq ASC) with a running cumulative cost, processed from the
        // lowest-priority end.
        let block_cutoff = peak_height.saturating_add(EXPIRING_BLOCK_CUTOFF);
        let time_cutoff = peak_timestamp.saturating_add(EXPIRING_TIME_CUTOFF);
        let expires_soon = |tl: &EffectiveTimelocks| {
            tl.assert_before_height.is_some_and(|h| h < block_cutoff)
                || tl.assert_before_seconds.is_some_and(|s| s < time_cutoff)
        };
        if expires_soon(&item.timelocks) {
            let mut expiring: Vec<(Bytes32, f64, u64, u64)> = self
                .items
                .values()
                .filter(|i| expires_soon(&i.timelocks))
                .map(|i| (i.name, i.fee_per_virtual_cost(), i.seq, i.cost))
                .collect();
            expiring.sort_by(|a, b| {
                b.1.partial_cmp(&a.1)
                    .unwrap_or(Ordering::Equal)
                    .then(a.2.cmp(&b.2))
            });
            let mut cumulative: u64 = 0;
            let rows: Vec<(Bytes32, f64, u64)> = expiring
                .into_iter()
                .map(|(name, priority, _, item_cost)| {
                    cumulative = cumulative.saturating_add(item_cost);
                    (name, priority, cumulative)
                })
                .collect();
            let incoming_priority = item.fee_per_virtual_cost();
            let mut to_remove: Vec<Bytes32> = Vec::new();
            for (name, priority, cumulative_cost) in rows.into_iter().rev() {
                // There's room for us below this row: stop pruning.
                if cumulative_cost.saturating_add(item.cost) <= self.max_block_clvm_cost {
                    break;
                }
                // Can't evict a higher-priority expiring item: abort, and do NOT evict what was
                // set aside.
                if priority > incoming_priority {
                    return Err(MempoolError::FeeTooLow);
                }
                to_remove.push(name);
            }
            for name in to_remove {
                self.remove(&name);
            }
        }
        // POOL_FULL: keep the highest-priority prefix whose total cost fits alongside the
        // incoming item; evict the rest.
        if self.total_cost.saturating_add(item.cost) > self.max_total_cost {
            let mut by_priority: Vec<(Bytes32, f64, u64, u64)> = self
                .items
                .values()
                .map(|i| (i.name, i.fee_per_virtual_cost(), i.seq, i.cost))
                .collect();
            by_priority.sort_by(|a, b| {
                b.1.partial_cmp(&a.1)
                    .unwrap_or(Ordering::Equal)
                    .then(a.2.cmp(&b.2))
            });
            // The running total INCLUDES the current row, so kept is the strict prefix whose
            // running total stays within budget — everything after is evicted.
            let budget = self.max_total_cost.saturating_sub(item.cost);
            let mut running: u64 = 0;
            let mut evict: Vec<Bytes32> = Vec::new();
            for (name, _, _, item_cost) in by_priority {
                running = running.saturating_add(item_cost);
                if running > budget {
                    evict.push(name);
                }
            }
            for name in evict {
                self.remove(&name);
            }
        }
        let name = item.name;
        // Index insertion — FF spends index under their LATEST singleton coin id; a coin id may
        // map to several owners (dedup / FF chains).
        for key in item.index_keys() {
            let owners = self.by_coin.entry(key).or_default();
            if !owners.contains(&name) {
                owners.push(name);
            }
        }
        self.total_cost += item.cost;
        self.total_fee += item.fee;
        // Feed the estimator AFTER the totals update, with the new mempool cost.
        let (cost, fee, height_added) = (item.cost, item.fee, item.height_added);
        let total = self.total_cost;
        self.fee_estimator
            .add_mempool_item(cost, fee, height_added, total);
        self.items.insert(name, item);
        Ok(())
    }

    // Replace-by-fee rules: the new bundle must (a) spend a superset of every coin the
    // conflicting items spend — otherwise replacing bundle AB with a higher-fee B kicks A out of
    // the pool entirely, (b) strictly increase the aggregate fee-per-cost, (c) raise the total
    // fee by at least MEMPOOL_MIN_FEE_INCREASE, and (d) leave the EFFECTIVE ASSERT_HEIGHT /
    // ASSERT_BEFORE time-locks unchanged — the comparison runs on the effective bounds, never the
    // raw absolutes, and does not compare assert_seconds at all.
    fn can_replace(
        &self,
        conflicting: &[Bytes32],
        new_removals: &HashSet<Bytes32>,
        new_fee: u64,
        new_cost: u64,
        new_timelocks: &EffectiveTimelocks,
        new_bundle_coin_spends: &[BundleCoinSpend],
    ) -> bool {
        let mut conflicting_fees: u64 = 0;
        let mut conflicting_cost: u64 = 0;
        // Fold with max over plain u32s (0 = unconstrained) for assert_height and min for the
        // before-bounds.
        let mut assert_height: u32 = 0;
        let mut assert_before_height: Option<u32> = None;
        let mut assert_before_seconds: Option<u64> = None;
        // Replacements may not strip dedup/fast-forward eligibility from a coin spend — doing so
        // could deny such spends from operating as intended.
        let mut existing_ff_spends: HashSet<Bytes32> = HashSet::new();
        let mut existing_dedup_spends: HashSet<Bytes32> = HashSet::new();
        for name in conflicting {
            let Some(item) = self.items.get(name) else {
                return false;
            };
            for coin_id in &item.removals {
                if !new_removals.contains(coin_id) {
                    return false; // superset rule
                }
            }
            conflicting_fees = conflicting_fees.saturating_add(item.fee);
            conflicting_cost = conflicting_cost.saturating_add(item.cost);
            for bcs in &item.bundle_coin_spends {
                if bcs.supports_fast_forward() {
                    existing_ff_spends.insert(bcs.coin_id());
                }
                if bcs.eligible_for_dedup {
                    existing_dedup_spends.insert(bcs.coin_id());
                }
            }
            assert_height = assert_height.max(item.timelocks.assert_height);
            assert_before_height = match (assert_before_height, item.timelocks.assert_before_height)
            {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            assert_before_seconds =
                match (assert_before_seconds, item.timelocks.assert_before_seconds) {
                    (Some(a), Some(b)) => Some(a.min(b)),
                    (a, b) => a.or(b),
                };
        }
        // Strictly higher fee-per-cost, compared exactly by cross-multiplication.
        if u128::from(new_fee) * u128::from(conflicting_cost)
            <= u128::from(conflicting_fees) * u128::from(new_cost)
        {
            return false;
        }
        if new_fee.saturating_sub(conflicting_fees) < MEMPOOL_MIN_FEE_INCREASE {
            return false;
        }
        if new_timelocks.assert_height != assert_height
            || new_timelocks.assert_before_height != assert_before_height
            || new_timelocks.assert_before_seconds != assert_before_seconds
        {
            return false;
        }
        // Eligibility preservation: every coin the evicted items spent as FF/dedup must stay
        // FF/dedup in the replacement. (The superset rule above guarantees the replacement
        // spends these coins at all.)
        let new_bcs_for = |coin_id: &Bytes32| {
            new_bundle_coin_spends
                .iter()
                .find(|b| b.coin_id() == *coin_id)
        };
        for coin_id in &existing_ff_spends {
            if !new_bcs_for(coin_id).is_some_and(BundleCoinSpend::supports_fast_forward) {
                return false;
            }
        }
        for coin_id in &existing_dedup_spends {
            if !new_bcs_for(coin_id).is_some_and(|b| b.eligible_for_dedup) {
                return false;
            }
        }
        true
    }

    // Park a height-locked bundle for retry, evicting the entry with the HIGHEST effective
    // assert height at capacity — the furthest-from-admissible goes first.
    fn park_pending(
        &mut self,
        name: Bytes32,
        bundle: SpendBundle,
        conds: SpendBundleConditions,
        assert_height: u32,
    ) {
        let fee = u64::try_from(conds.removal_amount.saturating_sub(conds.addition_amount))
            .unwrap_or(u64::MAX);
        let cost = conds.cost.max(1);
        if self.pending.len() >= PENDING_CACHE_CAP {
            let worst = self
                .pending
                .iter()
                .max_by_key(|(_, e)| e.assert_height)
                .map(|(k, _)| *k);
            if let Some(k) = worst {
                self.pending.remove(&k);
            }
        }
        self.pending.insert(
            name,
            PendingEntry {
                bundle,
                conds,
                fee,
                cost,
                assert_height,
            },
        );
    }

    // Set aside a bundle rejected for a MEMPOOL_CONFLICT so a later peak can retry it. Dedups by
    // name (an already-cached name is a no-op, no double-counted cost), records it as newest in
    // the FIFO order, then evicts oldest-first while the summed cost clears one block's cost OR
    // the item count clears the cap.
    fn cache_conflict(
        &mut self,
        name: Bytes32,
        bundle: SpendBundle,
        conds: SpendBundleConditions,
        fee: u64,
        cost: u64,
    ) {
        if self.conflict.contains_key(&name) {
            return;
        }
        self.conflict.insert(
            name,
            ConflictEntry {
                bundle,
                conds,
                fee,
                cost,
            },
        );
        self.conflict_order.push_back(name);
        self.conflict_cost = self.conflict_cost.saturating_add(cost);
        // A single over-cap entry evicts itself (it is also the oldest once alone), leaving an
        // empty cache.
        while self.conflict_cost > self.conflict_cache_max_cost
            || self.conflict.len() > CONFLICT_CACHE_MAX_SIZE
        {
            let Some(oldest) = self.conflict_order.pop_front() else {
                break;
            };
            if let Some(evicted) = self.conflict.remove(&oldest) {
                self.conflict_cost = self.conflict_cost.saturating_sub(evicted.cost);
            }
        }
    }

    // Take every conflict-cached bundle out and reset the accounting. The caller re-runs
    // admission on each; anything that still conflicts re-populates the (now-empty) cache via
    // `cache_conflict`.
    fn drain_conflict(&mut self) -> Vec<(Bytes32, ConflictEntry)> {
        self.conflict_order.clear();
        self.conflict_cost = 0;
        self.conflict.drain().collect()
    }

    // The pure index/accounting removal — no fee-estimator signal. Used for BLOCK_INCLUSION
    // removals in `new_peak` (those feed the estimator as confirmations via `process_block`,
    // never as `remove_tx`).
    fn remove_inner(&mut self, name: &Bytes32) -> Option<MempoolItem> {
        let item = self.items.remove(name)?;
        self.total_cost = self.total_cost.saturating_sub(item.cost);
        self.total_fee = self.total_fee.saturating_sub(item.fee);
        for key in item.index_keys() {
            if let Some(owners) = self.by_coin.get_mut(&key) {
                owners.retain(|owner| owner != name);
                if owners.is_empty() {
                    self.by_coin.remove(&key);
                }
            }
        }
        Some(item)
    }

    // A NON-block removal (eviction / expiry / replacement / reorg): index bookkeeping plus the
    // estimator's `remove_tx` signal.
    fn remove(&mut self, name: &Bytes32) -> Option<MempoolItem> {
        let item = self.remove_inner(name)?;
        let total = self.total_cost;
        self.fee_estimator
            .remove_mempool_item(item.cost, item.fee, item.height_added, total);
        Some(item)
    }

    /// Priority-ordered view (highest fee per VIRTUAL cost first, FIFO on ties) — the
    /// block-builder feed.
    #[must_use]
    pub fn items_by_fee(&self) -> Vec<&MempoolItem> {
        let mut v: Vec<&MempoolItem> = self.items.values().collect();
        v.sort_by(|a, b| {
            b.fee_per_virtual_cost()
                .partial_cmp(&a.fee_per_virtual_cost())
                .unwrap_or(Ordering::Equal)
                .then(a.seq.cmp(&b.seq))
        });
        v
    }

    /// RAW fee-per-cost view (`fee_per_cost DESC, seq ASC`) — the `request_mempool_transactions`
    /// serve order. Assembly and eviction use the virtual-cost priority; serving does not.
    #[must_use]
    pub fn items_by_feerate(&self) -> Vec<&MempoolItem> {
        let mut v: Vec<&MempoolItem> = self.items.values().collect();
        v.sort_by(|a, b| {
            b.fee_per_cost()
                .partial_cmp(&a.fee_per_cost())
                .unwrap_or(Ordering::Equal)
                .then(a.seq.cmp(&b.seq))
        });
        v
    }

    /// The mempool's reference frame — the most recent TRANSACTION block's `(height, timestamp)`,
    /// `None` until the first [`Mempool::set_peak`]/[`Mempool::new_peak`]. The producer gates
    /// block assembly on this matching the candidate's previous transaction block (see
    /// [`Mempool::create_block_generator`]).
    #[must_use]
    pub fn peak(&self) -> Option<(u32, u64)> {
        self.peak
    }

    #[must_use]
    pub fn create_block_generator(
        &self,
        constants: &ConsensusConstants,
        height: u32,
        timeout: Duration,
    ) -> Option<BlockTransactions> {
        self.peak?;
        let block_overhead = QUOTE_BYTES * constants.cost_per_byte + QUOTE_EXECUTION_COST;
        let max_cost = constants.max_block_cost_clvm.saturating_sub(block_overhead);
        let start = Instant::now();

        // `cost_sum` tracks the PLAIN (uncompressed) accounting — item.cost already carries each
        // item's plain generator byte cost, so it's a sound UPPER BOUND on the true (back-ref
        // compressed) block cost. While the plain sum stays under budget the compressed block
        // certainly fits, so we admit cheaply. Only when the plain sum would overflow do we
        // measure the actual compressed size and re-check against the full budget.
        // `exec_cond_sum` is the block's execution + condition cost (item.cost minus each item's
        // plain byte cost) — the non-byte term the compressed check adds to the measured
        // compressed byte cost.
        let mut cost_sum: u64 = 0;
        let mut exec_cond_sum: u64 = 0;
        let mut fee_sum: u64 = 0;
        let mut spend_count: usize = 0;
        let mut skipped_items: usize = 0;
        let mut coin_spends: Vec<CoinSpend> = Vec::new();
        let mut sigs: Vec<Bytes96> = Vec::new();
        let mut additions: Vec<Coin> = Vec::new();
        let mut removals: Vec<Coin> = Vec::new();
        // Dedup state: coin id -> (solution bytes, per-spend cost) of the first-selected spend of
        // that coin; later identical spends save that cost.
        let mut dedup_spends: HashMap<Bytes32, (Vec<u8>, u64)> = HashMap::new();
        // Fast-forward state committed so far: puzzle hash -> the latest lineage after the spends
        // already included, so FF spends chain within the block.
        let mut ff_state: HashMap<Bytes32, UnspentLineageInfo> = HashMap::new();

        for item in self.items_by_fee() {
            // The wall-clock budget for the whole selection.
            if start.elapsed() >= timeout {
                info!("block assembly: timeout reached, stopping selection");
                break;
            }
            // The fee sum must stay a representable coin amount.
            let Some(new_fee_sum) = fee_sum.checked_add(item.fee) else {
                break;
            };
            if new_fee_sum > constants.max_coin_amount {
                break;
            }
            // Resolve this item's spends: fast-forward rebases + identical-spend dedup. Past
            // PRIORITY_TX_THRESHOLD skips, items with dedup/FF spends are passed over and the
            // rest assemble at full cost.
            let has_special = item
                .bundle_coin_spends
                .iter()
                .any(|b| b.eligible_for_dedup || b.supports_fast_forward());
            let processed = if item.bundle_coin_spends.is_empty() {
                // No per-spend metadata (test-synthesized conditions): plain assembly.
                ProcessedItem {
                    unique_spends: item.bundle.coin_spends.clone(),
                    unique_additions: additions_for_conditions(&item.conds, &[]),
                    cost_saving: 0,
                    ff_update: HashMap::new(),
                    new_dedup: Vec::new(),
                }
            } else if skipped_items >= PRIORITY_TX_THRESHOLD {
                if has_special {
                    info!(
                        "block assembly: skipping dedup/FF item past priority threshold item={}",
                        item.name
                    );
                    continue;
                }
                ProcessedItem {
                    unique_spends: item
                        .bundle_coin_spends
                        .iter()
                        .map(|b| b.coin_spend.clone())
                        .collect(),
                    unique_additions: item
                        .bundle_coin_spends
                        .iter()
                        .flat_map(|b| b.additions.iter().copied())
                        .collect(),
                    cost_saving: 0,
                    ff_update: HashMap::new(),
                    new_dedup: Vec::new(),
                }
            } else {
                match process_item_spends(item, &dedup_spends, &ff_state, constants, height) {
                    Ok(processed) => processed,
                    Err(ProcessError::SkipDedup(why)) => {
                        // Not counted against the skip budget.
                        info!("block assembly: dedup skip item={} why={}", item.name, why);
                        continue;
                    }
                    Err(ProcessError::Failed(why)) => {
                        info!(
                            "block assembly: item failed dedup/FF processing item={} why={}",
                            item.name, why
                        );
                        skipped_items += 1;
                        continue;
                    }
                }
            };
            // New dedup entries commit as soon as they're discovered, before the fit checks.
            for (coin_id, solution, spend_cost) in &processed.new_dedup {
                dedup_spends.insert(*coin_id, (solution.clone(), *spend_cost));
            }
            let item_cost = item.cost.saturating_sub(processed.cost_saving);
            let new_cost_sum = cost_sum.saturating_add(item_cost);
            let new_spend_count = spend_count + processed.unique_spends.len();
            // This item's execution + condition cost = its effective cost minus the PLAIN byte
            // cost of its own unique spends. The byte term is dropped here and re-added as the
            // measured COMPRESSED size below.
            let item_plain_byte_cost = if processed.unique_spends.is_empty() {
                0
            } else {
                (spend_bundle_generator_length(&processed.unique_spends) as u64)
                    .saturating_sub(QUOTE_BYTES)
                    .saturating_mul(constants.cost_per_byte)
            };
            let item_exec_cond = item_cost.saturating_sub(item_plain_byte_cost);
            // Fit decision. The 6,000-spend cap is hard. For cost: if the PLAIN sum fits, the
            // compressed block certainly fits (compressed ≤ plain), so admit without measuring.
            // Only when the plain sum would overflow do we serialize the tentative spend set with
            // back-references and re-check the true compressed cost.
            let fits = if new_spend_count > MAX_SPENDS_PER_BLOCK {
                false
            } else if new_cost_sum <= max_cost {
                true
            } else {
                let mut tentative: Vec<CoinSpend> =
                    Vec::with_capacity(coin_spends.len() + processed.unique_spends.len());
                tentative.extend(coin_spends.iter().cloned());
                tentative.extend(processed.unique_spends.iter().cloned());
                match compressed_solution_generator_from_coin_spends(&tentative) {
                    Ok(program) => {
                        let byte_cost =
                            (program.as_ref().len() as u64).saturating_mul(constants.cost_per_byte);
                        // block_cost = compressed byte cost + quote-execution + Σ exec/cond
                        // costs, compared to the FULL block budget.
                        let total = byte_cost
                            .saturating_add(QUOTE_EXECUTION_COST)
                            .saturating_add(exec_cond_sum)
                            .saturating_add(item_exec_cond);
                        total <= constants.max_block_cost_clvm
                    }
                    Err(e) => {
                        warn!("block assembly: compressed fit-check serialize failed: {e:?}");
                        false
                    }
                }
            };
            // Doesn't fit: skip it and keep looking for smaller items, up to MAX_SKIPPED_ITEMS
            // (break ON the tenth skip).
            if !fits {
                skipped_items += 1;
                if skipped_items < MAX_SKIPPED_ITEMS {
                    continue;
                }
                break;
            }
            // Included: commit the fast-forward chain state.
            for (puzzle_hash, lineage) in processed.ff_update {
                ff_state.insert(puzzle_hash, lineage);
            }
            removals.extend(processed.unique_spends.iter().map(|cs| cs.coin));
            coin_spends.extend(processed.unique_spends);
            sigs.push(item.bundle.aggregated_signature);
            additions.extend(processed.unique_additions);
            cost_sum = new_cost_sum;
            exec_cond_sum = exec_cond_sum.saturating_add(item_exec_cond);
            fee_sum = new_fee_sum;
            spend_count = new_spend_count;
            // Stop once the remaining budget is below a typical spend.
            // `cost_sum` is the plain upper bound; once a compression-only admit pushes it past
            // `max_cost` the remaining plain budget saturates to 0 (we're effectively full) and we
            // stop — the compressed block still validates under the true limit via the re-run below.
            if max_cost.saturating_sub(cost_sum) < MIN_COST_THRESHOLD
                || spend_count >= MAX_SPENDS_PER_BLOCK
            {
                break;
            }
        }
        if coin_spends.is_empty() {
            return None;
        }

        // Emit the back-reference-compressed generator: the reversed spend list,
        // subtree-deduplicated. Same program as the plain form (identical run output, conditions,
        // tree hash), fewer bytes — so the re-run cost below is the compressed byte cost and the
        // block packs more transactions.
        let program = match compressed_solution_generator_from_coin_spends(&coin_spends) {
            Ok(program) => program,
            Err(e) => {
                // The spends were parsed at admission; a serialize failure here is a bug.
                warn!("block assembly: generator serialization failed: {e:?}");
                return None;
            }
        };
        // The included items' aggregate signatures combined.
        let aggregated_signature = match aggregate_signatures(sigs.iter()) {
            Ok(sig) => sig,
            Err(e) => {
                warn!("block assembly: signature aggregation failed: {e}");
                return None;
            }
        };
        // The re-run cost check: run the emitted generator through our own validator and take
        // ITS cost.
        let rerun = execute_block_generator_result(&BlockGeneratorInput {
            transactions_generator: program.clone(),
            generator_refs: Vec::new(),
            constants: *constants,
            height,
            flags: BlockGeneratorFlags::for_height(constants, height),
        });
        let conds = match rerun {
            Ok(conds) => conds,
            Err(e) => {
                warn!(
                    "block assembly: failed to compute block cost during farming \
                     (re-run rejected the assembled generator): {e:?}"
                );
                return None;
            }
        };
        if conds.cost > constants.max_block_cost_clvm {
            // Unreachable given the budgeted selection; defense in depth.
            warn!(
                "block assembly: re-run cost {} exceeds MAX_BLOCK_COST_CLVM; dropping generator",
                conds.cost
            );
            return None;
        }
        info!(
            "block assembly: {} spends, {} additions, cost {}, fees {}",
            coin_spends.len(),
            additions.len(),
            conds.cost,
            fee_sum
        );
        Some(BlockTransactions {
            program,
            block_refs: Vec::new(),
            additions,
            removals,
            aggregated_signature,
            cost: conds.cost,
        })
    }

    /// Rebuild the pool for a new peak (the most recent TRANSACTION block, with its timestamp):
    /// expire every resident item whose effective `ASSERT_BEFORE` bound the new peak passed, drop
    /// every item whose removals the new peak spent (its transaction is either in the new block
    /// or invalidated by it), then retry parked bundles whose effective assert height the peak
    /// reached.
    ///
    /// # Errors
    /// Returns [`MempoolError::Store`] on a store failure.
    pub async fn new_peak<S: CoinStore + Sync>(
        &mut self,
        store: &S,
        height: u32,
        timestamp: u64,
        spent: &[Bytes32],
    ) -> Result<NewPeakResult, MempoolError> {
        self.peak = Some((height, timestamp));
        // Expiry sweep first (`assert_before_seconds <= timestamp OR assert_before_height <=
        // block_height`) on the EFFECTIVE bounds computed at admission.
        let expired_names: Vec<Bytes32> = self
            .items
            .values()
            .filter(|i| {
                i.timelocks
                    .assert_before_height
                    .is_some_and(|b| b <= height)
                    || i.timelocks
                        .assert_before_seconds
                        .is_some_and(|b| b <= timestamp)
            })
            .map(|i| i.name)
            .collect();
        let expired = expired_names.len();
        for name in &expired_names {
            self.remove(name);
        }
        // O(delta) fast path: only items INDEXED by the block's spent coins are touched — no
        // store re-query per resident item. A spent coin reaching a plain spend evicts its item
        // (the transaction is in the block, or invalidated by it); a spent coin reaching a
        // fast-forward spend gets the item REBASED onto the singleton's new latest version, or
        // evicted if the singleton has no unspent version left. Out-of-band store changes (a
        // reorg) go through [`Mempool::revalidate_for_reorg`] — the slow path.
        let mut to_remove: HashSet<Bytes32> = HashSet::new();
        // The items a plain (non-FF) coin spend put into THIS block. These feed the fee
        // estimator as CONFIRMATIONS (`new_block`/`process_block`), the positive signal;
        // FF-evicted items are NOT included here (they left without confirming, and
        // BLOCK_INCLUSION removals skip `remove_tx`).
        let mut included: Vec<(u64, u64, u32)> = Vec::new();
        // FF rebases deferred until all plain evictions are decided.
        let mut deferred_ff: Vec<(Bytes32, Bytes32)> = Vec::new();
        for spend in spent {
            let Some(owners) = self.by_coin.get(spend) else {
                continue;
            };
            for owner in owners.clone() {
                if to_remove.contains(&owner) {
                    continue;
                }
                let Some(item) = self.items.get(&owner) else {
                    continue;
                };
                match item.bundle_coin_spend(spend) {
                    Some(bcs) if bcs.supports_fast_forward() => {
                        deferred_ff.push((*spend, owner));
                    }
                    Some(_) => {
                        // A regular coin spend that just made it into a block — counted as an
                        // included/confirmed item.
                        included.push((item.cost, item.fee, item.height_added));
                        to_remove.insert(owner);
                    }
                    None => {
                        // indexed under a LATEST singleton coin id (FF), or a synthetic item
                        let is_ff = item.bundle_coin_spends.iter().any(|bcs| {
                            bcs.latest_singleton_lineage
                                .as_ref()
                                .is_some_and(|lineage| lineage.coin_id == *spend)
                        });
                        if is_ff {
                            deferred_ff.push((*spend, owner));
                        } else {
                            included.push((item.cost, item.fee, item.height_added));
                            to_remove.insert(owner);
                        }
                    }
                }
            }
        }
        // Phase A: which (item, spend) pairs need a fresh lineage — and the puzzle hash to ask.
        let mut ff_lookups: Vec<(Bytes32, Bytes32, Bytes32)> = Vec::new();
        for (spend, owner) in &deferred_ff {
            if to_remove.contains(owner) {
                continue;
            }
            let Some(item) = self.items.get(owner) else {
                continue;
            };
            let mut found = false;
            for bcs in &item.bundle_coin_spends {
                let matches_spend = bcs
                    .latest_singleton_lineage
                    .as_ref()
                    .is_some_and(|lineage| lineage.coin_id == *spend);
                if matches_spend {
                    found = true;
                    ff_lookups.push((*owner, *spend, bcs.coin_spend.coin.puzzle_hash));
                }
            }
            if !found {
                // Defensive: indexed as spending this coin but no matching spend; evict rather
                // than leave a wedged item.
                warn!(
                    "FF-indexed item has no matching spend; evicting item={} coin={}",
                    owner, spend
                );
                to_remove.insert(*owner);
            }
        }
        // Phase B: resolve each puzzle hash's new lineage once.
        let mut lineage_cache: HashMap<Bytes32, Option<UnspentLineageInfo>> = HashMap::new();
        for (_, _, puzzle_hash) in &ff_lookups {
            if !lineage_cache.contains_key(puzzle_hash) {
                let lineage = store.get_unspent_lineage_info(puzzle_hash).await?;
                lineage_cache.insert(*puzzle_hash, lineage);
            }
        }
        // Rebase or evict, then move the index entries in bulk.
        let mut index_updates: Vec<(Bytes32, Bytes32, Bytes32)> = Vec::new();
        for (owner, spend, puzzle_hash) in ff_lookups {
            if to_remove.contains(&owner) {
                continue;
            }
            match lineage_cache.get(&puzzle_hash).copied().flatten() {
                None => {
                    // no unspent version left: FF is no longer available, evict
                    to_remove.insert(owner);
                }
                Some(lineage) => {
                    if let Some(item) = self.items.get_mut(&owner) {
                        for bcs in &mut item.bundle_coin_spends {
                            let matches_spend = bcs
                                .latest_singleton_lineage
                                .as_ref()
                                .is_some_and(|l| l.coin_id == spend);
                            if matches_spend {
                                bcs.latest_singleton_lineage = Some(lineage);
                            }
                        }
                    }
                    index_updates.push((lineage.coin_id, spend, owner));
                }
            }
        }
        // Applied even for items marked for removal in a LATER phase-C iteration: their
        // bundle_coin_spends were already rebased, and `remove` derives its index keys from that
        // state — the index must agree with it.
        for (new_coin_id, current_coin_id, owner) in index_updates {
            if let Some(owners) = self.by_coin.get_mut(&current_coin_id) {
                owners.retain(|o| *o != owner);
                if owners.is_empty() {
                    self.by_coin.remove(&current_coin_id);
                }
            }
            let owners = self.by_coin.entry(new_coin_id).or_default();
            if !owners.contains(&owner) {
                owners.push(owner);
            }
        }
        let dropped = to_remove.len();
        for name in &to_remove {
            // A block-included item leaves the seen cache too, so a reorg can resubmit it.
            self.remove_seen(name);
            // BLOCK_INCLUSION removal: pure bookkeeping, NO `remove_tx` — these items are fed to
            // the estimator as confirmations below.
            self.remove_inner(name);
        }
        // Feed the fee estimator this block's confirmations. Runs even for an empty `included`
        // so the tracker's block history / decay advances in lock-step with the chain.
        let total = self.total_cost;
        self.fee_estimator.new_block(height, &included, total);
        // Retry parked bundles whose EFFECTIVE assert height the new peak reached
        // (`assert_height <= peak.height`, strictly: an assert of peak+1 stays parked — a block
        // built on this peak could not carry it). Newly admitted names return so the caller can
        // re-gossip them.
        let eligible: Vec<Bytes32> = self
            .pending
            .iter()
            .filter(|(_, entry)| entry.assert_height <= height)
            .map(|(name, _)| *name)
            .collect();
        let mut admitted = Vec::new();
        for name in eligible {
            let Some(entry) = self.pending.remove(&name) else {
                continue;
            };
            let (fee, cost) = (entry.fee, entry.cost);
            if self.admit(store, entry.bundle, entry.conds).await.is_ok() {
                admitted.push((name, cost, fee));
            }
        }
        // Retry every conflict-cached bundle. Ordered AFTER the spent-coin removal + expiry
        // sweep above so a coin freed by the winner LEAVING the pool is already visible: the
        // loser re-admits (conflict resolved), or re-caches (winner still resident), or drops
        // (winner confirmed → the coin is spent on-chain → DOUBLE_SPEND, which never routes to
        // the conflict cache).
        for (name, entry) in self.drain_conflict() {
            let (fee, cost) = (entry.fee, entry.cost);
            if self.admit(store, entry.bundle, entry.conds).await.is_ok() {
                admitted.push((name, cost, fee));
            }
        }
        Ok(NewPeakResult {
            dropped,
            expired,
            admitted,
        })
    }

    /// The reorg slow path — the pool rebuild when the new peak is NOT a child of the old one:
    /// every resident item is re-checked against the store. Items whose removals ceased to exist
    /// (rolled back), were spent without fast-forward support, or whose singleton lost its
    /// unspent version, are dropped; surviving FF spends are rebased onto the singleton's
    /// current lineage. Returns the number of dropped items. O(pool) store work — reorgs only.
    ///
    /// # Errors
    /// Returns [`MempoolError::Store`] on a store failure.
    pub async fn revalidate_for_reorg<S: CoinStore + Sync>(
        &mut self,
        store: &S,
    ) -> Result<usize, MempoolError> {
        // The slow path resets the seen cache wholesale — after a reorg, previously-failed
        // bundles may be valid on the new branch.
        self.seen.clear();
        self.seen_order.clear();
        let snapshot: Vec<(Bytes32, Vec<Bytes32>)> = self
            .items
            .values()
            .map(|item| (item.name, item.removals.clone()))
            .collect();
        let mut drop: Vec<Bytes32> = Vec::new();
        let mut lineage_cache: HashMap<Bytes32, Option<UnspentLineageInfo>> = HashMap::new();
        for (name, removals) in snapshot {
            let records = store.get_coin_records(&removals).await?;
            let by_id: HashMap<Bytes32, bool> =
                records.iter().map(|r| (r.coin.name(), r.spent)).collect();
            // In-bundle-created (ephemeral) removals never exist in the store.
            let created: HashSet<Bytes32> = self.items.get(&name).map_or_else(HashSet::new, |i| {
                additions_for_conditions(&i.conds, &[])
                    .iter()
                    .map(Coin::name)
                    .collect()
            });
            let mut item_dead = false;
            let mut rebases: Vec<(Bytes32, Bytes32)> = Vec::new(); // (coin_id, puzzle_hash)
            for coin_id in &removals {
                let ff_ph = self.items.get(&name).and_then(|i| {
                    i.bundle_coin_spend(coin_id)
                        .filter(|b| b.supports_fast_forward())
                        .map(|b| b.coin_spend.coin.puzzle_hash)
                });
                match by_id.get(coin_id) {
                    None if created.contains(coin_id) => {}
                    None => {
                        // the removal no longer exists after the rollback — UNKNOWN_UNSPENT
                        item_dead = true;
                        break;
                    }
                    Some(spent) => {
                        if let Some(puzzle_hash) = ff_ph {
                            // FF spends must still have an unspent singleton version
                            rebases.push((*coin_id, puzzle_hash));
                        } else if *spent {
                            item_dead = true;
                            break;
                        }
                    }
                }
            }
            if !item_dead {
                for (_, puzzle_hash) in &rebases {
                    if !lineage_cache.contains_key(puzzle_hash) {
                        let lineage = store.get_unspent_lineage_info(puzzle_hash).await?;
                        lineage_cache.insert(*puzzle_hash, lineage);
                    }
                    if lineage_cache.get(puzzle_hash).copied().flatten().is_none() {
                        item_dead = true;
                        break;
                    }
                }
            }
            if item_dead {
                drop.push(name);
                continue;
            }
            // Rebase surviving FF spends onto the current lineage, keeping the index in step.
            for (coin_id, puzzle_hash) in rebases {
                let Some(new_lineage) = lineage_cache.get(&puzzle_hash).copied().flatten() else {
                    continue;
                };
                let mut old_key = None;
                if let Some(item) = self.items.get_mut(&name)
                    && let Some(bcs) = item
                        .bundle_coin_spends
                        .iter_mut()
                        .find(|b| b.coin_id() == coin_id)
                {
                    old_key = bcs
                        .latest_singleton_lineage
                        .as_ref()
                        .map(|lineage| lineage.coin_id);
                    bcs.latest_singleton_lineage = Some(new_lineage);
                }
                if let Some(old_key) = old_key
                    && old_key != new_lineage.coin_id
                {
                    if let Some(owners) = self.by_coin.get_mut(&old_key) {
                        owners.retain(|o| *o != name);
                        if owners.is_empty() {
                            self.by_coin.remove(&old_key);
                        }
                    }
                    let owners = self.by_coin.entry(new_lineage.coin_id).or_default();
                    if !owners.contains(&name) {
                        owners.push(name);
                    }
                }
            }
        }
        let dropped = drop.len();
        for name in drop {
            self.remove(&name);
        }
        Ok(dropped)
    }
}

impl dg_xch_core::errors::ErrorCode for MempoolError {
    fn band(&self) -> dg_xch_core::errors::ErrorBand {
        match self {
            MempoolError::Store(inner) => inner.band(),
            _ => dg_xch_core::errors::ErrorBand::Mempool,
        }
    }
    fn variant(&self) -> u16 {
        match self {
            MempoolError::NoPeak => 1,
            MempoolError::ZeroCost => 2,
            MempoolError::CostExceedsMax(_) => 3,
            MempoolError::UnknownUnspent(_) => 4,
            MempoolError::DoubleSpend(_) => 5,
            MempoolError::Conflict(_) => 6,
            MempoolError::Pending(_, _) => 7,
            MempoolError::Expired(_, _) => 8,
            MempoolError::TimelockNotMet(_, _) => 9,
            MempoolError::ImpossibleTimelock(_, _) => 10,
            MempoolError::FeeTooLow => 11,
            MempoolError::FeeNearZero => 12,
            MempoolError::FeeLimitExceeded => 13,
            MempoolError::NonCanonicalSolution(_) => 14,
            MempoolError::AlreadyIncluding(_) => 15,
            MempoolError::InvalidSpendBundle(_) => 16,
            MempoolError::Name(_) => 17,
            MempoolError::Store(inner) => inner.variant(),
        }
    }
}
