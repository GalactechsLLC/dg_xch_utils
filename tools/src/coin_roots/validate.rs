use super::{CoinSetAccumulator, RootV1, RootsError};
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use sha2::{Digest, Sha256};
use std::collections::HashMap;

/// v2 fold-leaf domain (a new version, never a mutation of v1; see `lib.rs` "Layout evolution").
pub const DOMAIN_FOLD_LEAF: &[u8] = b"dgxch.coinroot.v2.fold-leaf";

fn sha256_parts(parts: &[&[u8]]) -> [u8; 32] {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p);
    }
    h.finalize().into()
}

/// The exact bucket-C surface: the spent coin's creation height and the confirming block's
/// timestamp — the only data a spend does not carry that any condition can read. Two scalars,
/// nothing else.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CoinMeta {
    pub created_height: u32,
    pub created_timestamp: u64,
}

/// The v2 fold leaf: `H(domain || coin_id || LE32(created_height) || LE64(created_timestamp))`.
/// Position-independent and identical at ADD (from the creating block) and REMOVE (from the
/// hint), which is the whole point.
#[must_use]
pub fn fold_leaf(coin_id: &Bytes32, meta: CoinMeta) -> [u8; 32] {
    sha256_parts(&[
        DOMAIN_FOLD_LEAF,
        &**coin_id,
        &meta.created_height.to_le_bytes(),
        &meta.created_timestamp.to_le_bytes(),
    ])
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TimeLockConditions {
    /// ASSERT_MY_BIRTH_HEIGHT (75): `created_height == v`.
    pub birth_height: Option<u32>,
    /// ASSERT_MY_BIRTH_SECONDS (74): `created_timestamp == v`.
    pub birth_seconds: Option<u64>,
    /// ASSERT_HEIGHT_RELATIVE (82): `now_h - created_height >= v`.
    pub height_relative: Option<u32>,
    /// ASSERT_SECONDS_RELATIVE (80): `now_ts - created_timestamp >= v`.
    pub seconds_relative: Option<u64>,
    /// ASSERT_BEFORE_HEIGHT_RELATIVE (86): `now_h < created_height + v`.
    pub before_height_relative: Option<u32>,
    /// ASSERT_BEFORE_SECONDS_RELATIVE (84): `now_ts < created_timestamp + v`.
    pub before_seconds_relative: Option<u64>,
}

impl TimeLockConditions {
    pub fn check(
        &self,
        meta: CoinMeta,
        now_height: u32,
        now_timestamp: u64,
    ) -> Result<(), TimeLockError> {
        if let Some(v) = self.birth_height
            && meta.created_height != v
        {
            return Err(TimeLockError::BirthHeight {
                got: meta.created_height,
                want: v,
            });
        }
        if let Some(v) = self.birth_seconds
            && meta.created_timestamp != v
        {
            return Err(TimeLockError::BirthSeconds {
                got: meta.created_timestamp,
                want: v,
            });
        }
        if let Some(v) = self.height_relative
            && now_height < meta.created_height.saturating_add(v)
        {
            return Err(TimeLockError::HeightRelative {
                now: now_height,
                created: meta.created_height,
                delta: v,
            });
        }
        if let Some(v) = self.seconds_relative
            && now_timestamp < meta.created_timestamp.saturating_add(v)
        {
            return Err(TimeLockError::SecondsRelative {
                now: now_timestamp,
                created: meta.created_timestamp,
                delta: v,
            });
        }
        if let Some(v) = self.before_height_relative
            && now_height >= meta.created_height.saturating_add(v)
        {
            return Err(TimeLockError::BeforeHeightRelative {
                now: now_height,
                created: meta.created_height,
                delta: v,
            });
        }
        if let Some(v) = self.before_seconds_relative
            && now_timestamp >= meta.created_timestamp.saturating_add(v)
        {
            return Err(TimeLockError::BeforeSecondsRelative {
                now: now_timestamp,
                created: meta.created_timestamp,
                delta: v,
            });
        }
        Ok(())
    }
}

/// A bucket-C condition that failed against the supplied metadata.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimeLockError {
    BirthHeight { got: u32, want: u32 },
    BirthSeconds { got: u64, want: u64 },
    HeightRelative { now: u32, created: u32, delta: u32 },
    SecondsRelative { now: u64, created: u64, delta: u64 },
    BeforeHeightRelative { now: u32, created: u32, delta: u32 },
    BeforeSecondsRelative { now: u64, created: u64, delta: u64 },
}

impl std::fmt::Display for TimeLockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BirthHeight { got, want } => {
                write!(
                    f,
                    "ASSERT_MY_BIRTH_HEIGHT: created {got} != asserted {want}"
                )
            }
            Self::BirthSeconds { got, want } => {
                write!(
                    f,
                    "ASSERT_MY_BIRTH_SECONDS: created {got} != asserted {want}"
                )
            }
            Self::HeightRelative {
                now,
                created,
                delta,
            } => write!(
                f,
                "ASSERT_HEIGHT_RELATIVE: now {now} < created {created} + {delta}"
            ),
            Self::SecondsRelative {
                now,
                created,
                delta,
            } => write!(
                f,
                "ASSERT_SECONDS_RELATIVE: now {now} < created {created} + {delta}"
            ),
            Self::BeforeHeightRelative {
                now,
                created,
                delta,
            } => write!(
                f,
                "ASSERT_BEFORE_HEIGHT_RELATIVE: now {now} >= created {created} + {delta}"
            ),
            Self::BeforeSecondsRelative {
                now,
                created,
                delta,
            } => write!(
                f,
                "ASSERT_BEFORE_SECONDS_RELATIVE: now {now} >= created {created} + {delta}"
            ),
        }
    }
}

impl std::error::Error for TimeLockError {}

/// A coin creation observed while validating a block (an accumulator ADD input). Its metadata
/// is derived from the creating block itself (`created_height == this block's height`,
/// `created_timestamp == this block's timestamp`) — never a hint.
#[derive(Clone, Debug)]
pub struct Creation {
    pub coin_id: Bytes32,
    pub meta: CoinMeta,
}

/// A spend observed while validating a block (an accumulator REMOVE input). `meta` is
/// hint-supplied for coins created in a different block (or before the range) and self-supplied
/// for ephemeral coins created in this same block; either way a wrong value fails cancellation.
#[derive(Clone, Debug)]
pub struct SpendRef {
    pub coin_id: Bytes32,
    /// The height at which this spend confirms (this block's height) — feeds the phase-1
    /// spent-bitmap as the coin's `spent_index`.
    pub spent_height: u32,
    /// Hint-supplied creation metadata of the spent coin.
    pub meta: CoinMeta,
    /// The spend's bucket-C conditions (empty if it has none).
    pub time_locks: TimeLockConditions,
}

#[derive(Clone, Debug)]
pub struct BlockObservation {
    pub height: u32,
    pub now_height: u32,
    pub now_timestamp: u64,
    pub self_valid: bool,
    pub creations: Vec<Creation>,
    pub spends: Vec<SpendRef>,
}

/// Fail-closed reasons the range does not reconcile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ValidateError {
    /// A block's own self-contained check (generator/roots/aggsig/cost or intra-block scoping)
    /// failed. Bucket B/D, independent of processing order.
    BlockSelfInvalid { height: u32 },
    /// The same coin was created twice — impossible for a real chain (coin id is a hash of
    /// `(parent, puzzle_hash, amount)`); fail-closed against a corrupt/adversarial corpus.
    DuplicateCreation { coin_id: Bytes32 },
    /// A spend references a coin that was never created and never seeded — an unmatched REMOVE.
    SpendOfMissingCoin { coin_id: Bytes32 },
    /// A bucket-C condition failed against the supplied metadata.
    TimeLock {
        coin_id: Bytes32,
        source: TimeLockError,
    },
    /// The MSet-XOR residual did not equal the XOR of the hinted survivors — wrong hints, wrong
    /// metadata, or a missing/extra coin. The core fail-closed check.
    XorResidualMismatch,
    /// The survivor hint set did not equal `created - spent`.
    SurvivorHintMismatch { hinted: usize, actual: usize },
    /// The reconstructed phase-1 root did not equal the expected root for the range.
    RootMismatch { got: Bytes32, want: Bytes32 },
    /// Phase-1 accumulator rejected the canonical stream (misorder/spend-before-create).
    Roots(String),
}

impl std::fmt::Display for ValidateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BlockSelfInvalid { height } => write!(f, "block {height} failed its own check"),
            Self::DuplicateCreation { coin_id } => write!(f, "duplicate creation {coin_id}"),
            Self::SpendOfMissingCoin { coin_id } => write!(f, "spend of missing coin {coin_id}"),
            Self::TimeLock { coin_id, source } => write!(f, "coin {coin_id}: {source}"),
            Self::XorResidualMismatch => {
                write!(f, "MSet-XOR residual != survivor hints (fail-closed)")
            }
            Self::SurvivorHintMismatch { hinted, actual } => {
                write!(
                    f,
                    "survivor hint count {hinted} != created-minus-spent {actual}"
                )
            }
            Self::RootMismatch { got, want } => write!(f, "phase-1 root {got} != expected {want}"),
            Self::Roots(msg) => write!(f, "phase-1 accumulator: {msg}"),
        }
    }
}

impl std::error::Error for ValidateError {}

impl From<RootsError> for ValidateError {
    fn from(e: RootsError) -> Self {
        Self::Roots(e.to_string())
    }
}

/// Order-independent accumulation state for a block range. Blocks may be [`Self::observe`]d in
/// **any order** (the maps and the XOR aggregate are commutative); [`Self::reconcile`] then
/// applies the hints and ties out to the phase-1 root. Bounded: memory is O(coins in range),
/// which is the range's coin set — the same bound the phase-1 single-pass driver carries.
#[derive(Default)]
pub struct RangeValidator {
    created: HashMap<Bytes32, CoinMeta>,
    spent: HashMap<Bytes32, u32>,
    xor: [u8; 32],
    errors: Vec<ValidateError>,
}

fn xor_in(acc: &mut [u8; 32], leaf: [u8; 32]) {
    for (a, b) in acc.iter_mut().zip(leaf.iter()) {
        *a ^= *b;
    }
}

impl RangeValidator {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed a pre-range coin (the range-start UTXO set, verifiable against the phase-1 root at
    /// the range-start boundary). An ADD with no in-range creating block. For a from-genesis
    /// range there are none.
    pub fn seed(&mut self, coin_id: Bytes32, meta: CoinMeta) {
        if self.created.insert(coin_id, meta).is_some() {
            self.errors
                .push(ValidateError::DuplicateCreation { coin_id });
            return;
        }
        xor_in(&mut self.xor, fold_leaf(&coin_id, meta));
    }

    /// Fold one block's observations into the running state. Order-independent across blocks.
    pub fn observe(&mut self, block: &BlockObservation) {
        if !block.self_valid {
            self.errors.push(ValidateError::BlockSelfInvalid {
                height: block.height,
            });
        }
        for c in &block.creations {
            if self.created.insert(c.coin_id, c.meta).is_some() {
                self.errors
                    .push(ValidateError::DuplicateCreation { coin_id: c.coin_id });
                continue;
            }
            xor_in(&mut self.xor, fold_leaf(&c.coin_id, c.meta));
        }
        for s in &block.spends {
            if let Err(source) = s
                .time_locks
                .check(s.meta, block.now_height, block.now_timestamp)
            {
                self.errors.push(ValidateError::TimeLock {
                    coin_id: s.coin_id,
                    source,
                });
            }
            self.spent.insert(s.coin_id, s.spent_height);
            // REMOVE uses the hint-supplied meta; if it differs from the creation meta the XOR
            // will not cancel — the fail-closed weld between the condition check and the root.
            xor_in(&mut self.xor, fold_leaf(&s.coin_id, s.meta));
        }
    }

    /// The current MSet-XOR residual (adds XOR removes). Exposed for the order-independence
    /// proof: identical across any observation order for the same block set.
    #[must_use]
    pub fn xor_digest(&self) -> [u8; 32] {
        self.xor
    }

    /// Reconcile the range: check the survivor hints against the XOR aggregate, then rebuild the
    /// phase-1 root order-independently and compare to `expected_root`.
    ///
    /// `survivor_hints` is the SwiftSync hint set: the coin ids created (or seeded) in the range
    /// and still unspent at `boundary_height`. Fail-closed on any mismatch.
    ///
    /// # Errors
    /// The first [`ValidateError`] encountered — a self-invalid block, a duplicate/missing coin,
    /// a bucket-C violation, an XOR mismatch, a survivor-count mismatch, a phase-1 accumulator
    /// rejection, or a root mismatch.
    pub fn reconcile(
        &self,
        survivor_hints: &[Bytes32],
        boundary_height: u32,
        header_hash: Bytes32,
        expected_root: Bytes32,
    ) -> Result<RootV1, ValidateError> {
        if let Some(e) = self.errors.first() {
            return Err(e.clone());
        }
        // Every spend must match a creation (or seed): no unmatched REMOVE.
        for coin_id in self.spent.keys() {
            if !self.created.contains_key(coin_id) {
                return Err(ValidateError::SpendOfMissingCoin { coin_id: *coin_id });
            }
        }
        // Layer 1 (MSet-XOR): residual must equal XOR of the survivors' fold leaves.
        let mut survivor_xor = [0u8; 32];
        for s in survivor_hints {
            let meta = self
                .created
                .get(s)
                .ok_or(ValidateError::SpendOfMissingCoin { coin_id: *s })?;
            xor_in(&mut survivor_xor, fold_leaf(s, *meta));
        }
        if survivor_xor != self.xor {
            return Err(ValidateError::XorResidualMismatch);
        }
        // The survivor set must be exactly created-minus-spent.
        let actual_survivors = self.created.len() - self.spent.len();
        if survivor_hints.len() != actual_survivors {
            return Err(ValidateError::SurvivorHintMismatch {
                hinted: survivor_hints.len(),
                actual: actual_survivors,
            });
        }
        // Layer 2 (authoritative): rebuild the phase-1 root from the order-independent maps.
        let root = self.phase1_root(boundary_height, header_hash)?;
        if root.root_v1 != expected_root {
            return Err(ValidateError::RootMismatch {
                got: root.root_v1,
                want: expected_root,
            });
        }
        Ok(root)
    }

    /// Rebuild the phase-1 v1 root over every creation in range (spent and unspent), in the
    /// canonical `(confirmed_height, coin_id)` order the phase-1 spec fixes. Order-independent:
    /// the canonical sort erases observation order.
    ///
    /// # Errors
    /// [`ValidateError::Roots`] if the phase-1 accumulator rejects the stream.
    pub fn phase1_root(
        &self,
        boundary_height: u32,
        header_hash: Bytes32,
    ) -> Result<RootV1, ValidateError> {
        let mut coins: Vec<(u32, Bytes32, u64, u32)> = self
            .created
            .iter()
            .map(|(id, meta)| {
                let spent_index = self.spent.get(id).copied().unwrap_or(0);
                (
                    meta.created_height,
                    *id,
                    meta.created_timestamp,
                    spent_index,
                )
            })
            .collect();
        // Canonical v1 order: ascending (confirmed_height, coin_id bytewise). Compare the coin
        // id via `&*` (the `[u8; 32]` view) exactly as `lib.rs::append` does — `Bytes32` itself
        // is not relied on for `Ord`; the bytewise `[u8; 32]` compare is the v1 tie-break.
        coins.sort_unstable_by(|a, b| (a.0, &*a.1).cmp(&(b.0, &*b.1)));
        let mut acc = CoinSetAccumulator::new();
        for (height, coin_id, timestamp, spent_index) in coins {
            acc.append(coin_id, height, timestamp, spent_index)?;
        }
        Ok(acc.root_at(boundary_height, header_hash)?)
    }
}
