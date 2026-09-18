//! The farmer interface (node/consensus side): validating a farmer's declared proof of space
//! against a signage point we have accepted, holding the accepted declaration as a candidate, and
//! assembling the unfinished block from an accepted proof.
//!
//! This module is pure (no store, no locks): [`validate_declared_proof`] takes closures for the
//! two slot-state lookups so the early-return ladder is unit-testable offline. The server passes
//! `SlotState`-backed closures.

use dg_xch_core::blockchain::block_record::BlockRecord;
use dg_xch_core::blockchain::pool_target::PoolTarget;
use dg_xch_core::blockchain::proof_of_space::ProofOfSpace;
use dg_xch_core::blockchain::signage_point::SignagePoint;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::blockchain::subslot_bundle::SubSlotBundle;
use dg_xch_core::blockchain::unfinished_block::UnfinishedBlock;
use dg_xch_core::consensus::constants::ConsensusConstants;
use dg_xch_core::consensus::pot_iterations::{
    calculate_ip_iters, calculate_iterations_quality_for_proof, calculate_sp_iters,
};
use dg_xch_core::consensus::producer::{
    FarmerSignatures, RewardBlockClaim, calculate_infusion_point_total_iters,
    create_unfinished_block_with_sigs, g2_infinity,
};
use dg_xch_core::protocols::farmer::{
    DeclareProofOfSpace, NewSignagePoint, RequestSignedValues, SPVDFSourceData,
    SignagePointSourceData,
};
use dg_xch_pos::verify_and_get_quality_string;
use std::collections::{HashMap, VecDeque};

/// The outcome of validating a `DeclareProofOfSpace`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeclareVerdict {
    /// No accepted signage point matches `challenge_chain_sp`.
    UnknownSignagePoint,
    /// index > 0 and the SP's reward-chain output no longer matches — a stale point that has been
    /// superseded.
    StaleSignagePoint,
    /// The SP's challenge sits in no sub slot we hold.
    UnknownSubSlot,
    /// `verify_and_get_quality_string` rejected the proof.
    InvalidProof,
    /// Accepted; carries the proof's quality string.
    Accepted(Bytes32),
}

impl DeclareVerdict {
    #[must_use]
    pub fn result_label(&self) -> &'static str {
        match self {
            DeclareVerdict::UnknownSignagePoint => "unknown_signage_point",
            DeclareVerdict::StaleSignagePoint => "stale_signage_point",
            DeclareVerdict::UnknownSubSlot => "unknown_sub_slot",
            DeclareVerdict::InvalidProof => "pospace_verify_fail",
            DeclareVerdict::Accepted(_) => "accepted",
        }
    }
}

/// The challenge-chain challenge a declared proof is filtered/validated against: the SP hash
/// itself at signage-point index 0, otherwise the SP's challenge-chain VDF challenge.
#[must_use]
pub fn cc_challenge_hash(declare: &DeclareProofOfSpace, sp: &SignagePoint) -> Bytes32 {
    if declare.signage_point_index == 0 {
        // The sub-slot-start SP has no cc VDF; the cc challenge IS the SP hash.
        declare.challenge_chain_sp
    } else {
        // index > 0 always carries a real cc VDF; the fallback to the SP hash only guards a
        // malformed all-None SP reaching here, which the accept ladder rejects.
        sp.cc_vdf
            .as_ref()
            .map_or(declare.challenge_chain_sp, |v| v.challenge)
    }
}

/// Validate a farmer's `DeclareProofOfSpace` against our slot state. Returns the quality string
/// on accept. `lookup_sp(cc_sp)` resolves an accepted signage point by its challenge-chain SP
/// hash; `sub_slot_present(cc)` reports whether we hold the sub slot for challenge `cc`. `height`
/// is the current peak height (0 pre-genesis), feeding the plot filter.
///
/// The caller is expected to have already dropped the message if the node is syncing.
/// tip-context objects cannot validate mid-sync.
pub fn validate_declared_proof(
    constants: &ConsensusConstants,
    declare: &DeclareProofOfSpace,
    height: u32,
    lookup_sp: impl Fn(&Bytes32) -> Option<SignagePoint>,
    sub_slot_present: impl Fn(&Bytes32) -> bool,
) -> DeclareVerdict {
    // The proof must be for a signage point we have accepted.
    let Some(sp) = lookup_sp(&declare.challenge_chain_sp) else {
        return DeclareVerdict::UnknownSignagePoint;
    };
    // For index > 0, the SP's reward-chain output must still be the current one; a mismatch means
    // the farmer is answering a superseded signage point.
    if declare.signage_point_index > 0
        && sp.rc_vdf.as_ref().and_then(|v| v.output.hash().ok()) != Some(declare.reward_chain_sp)
    {
        return DeclareVerdict::StaleSignagePoint;
    }
    let cc_hash = cc_challenge_hash(declare, &sp);
    // A non-genesis SP must belong to a sub slot we hold.
    if declare.challenge_hash != constants.genesis_challenge && !sub_slot_present(&cc_hash) {
        return DeclareVerdict::UnknownSubSlot;
    }
    // The proof of space itself must verify against our checker.
    match verify_and_get_quality_string(
        &declare.proof_of_space,
        constants,
        cc_hash,
        declare.challenge_chain_sp,
        height,
    ) {
        Some(quality) => DeclareVerdict::Accepted(quality),
        None => DeclareVerdict::InvalidProof,
    }
}

/// Build the farmer-protocol `NewSignagePoint` we push to farming peers when we accept a signage
/// point. Distinct from the full-node `NewSignagePointOrEndOfSubSlot` gossip: this carries the
/// difficulty/SSI context a farmer needs to look up plots. `None` only if the SP's VDF outputs
/// fail to hash.
#[must_use]
pub fn new_signage_point_for_farmers(
    sp: &SignagePoint,
    challenge_hash: Bytes32,
    difficulty: u64,
    sub_slot_iters: u64,
    signage_point_index: u8,
    peak_height: u32,
    last_tx_height: u32,
) -> Option<NewSignagePoint> {
    // A normal (index > 0) signage point carries its cc/rc SP-VDF outputs as
    // sp_source_data.vdf_data so a farmer/harvester can reconstruct what it signs.
    let cc_vdf = sp.cc_vdf.as_ref()?;
    let rc_vdf = sp.rc_vdf.as_ref()?;
    Some(NewSignagePoint {
        challenge_hash,
        challenge_chain_sp: cc_vdf.output.hash().ok()?,
        reward_chain_sp: rc_vdf.output.hash().ok()?,
        difficulty,
        sub_slot_iters,
        signage_point_index,
        peak_height,
        last_tx_height,
        sp_source_data: Some(SignagePointSourceData {
            sub_slot_data: None,
            vdf_data: Some(SPVDFSourceData {
                cc_vdf: cc_vdf.output,
                rc_vdf: rc_vdf.output,
            }),
        }),
    })
}

/// The `rc_prev` for a `NewUnfinishedBlockTimelord`: the last reward-chain infusion before this
/// block's signage point. At signage-point index 0 it is the pos sub-slot's reward-chain hash,
/// falling back to the genesis challenge when the pos sub-slot IS genesis and is not held as a
/// finished sub-slot; `None` when we hold no such sub slot. At index > 0 it is the SP's
/// reward-chain VDF challenge.
///
/// `pos_sub_slot_rc_hash` is resolved by the caller from the slot state (index 0 only) so this
/// stays pure.
#[must_use]
pub fn timelord_rc_prev(
    genesis_challenge: Bytes32,
    signage_point_index: u8,
    pos_ss_cc_challenge_hash: Bytes32,
    reward_chain_sp_vdf: Option<&dg_xch_core::blockchain::vdf_info::VdfInfo>,
    pos_sub_slot_rc_hash: Option<Bytes32>,
) -> Option<Bytes32> {
    if signage_point_index == 0 {
        pos_sub_slot_rc_hash.or_else(|| {
            (pos_ss_cc_challenge_hash == genesis_challenge).then_some(genesis_challenge)
        })
    } else {
        reward_chain_sp_vdf.map(|v| v.challenge)
    }
}

/// An accepted declaration, held until a block is assembled from it (or it ages out).
#[derive(Debug, Clone)]
pub struct AcceptedProof {
    pub declare: DeclareProofOfSpace,
    pub quality_string: Bytes32,
}

/// Bounded FIFO of accepted proof declarations, keyed by quality string. Unlike the generator seed
/// cache (whose eviction could wall body validation), evicting a candidate here is harmless: the
/// only consequence is we do not build a block from that particular proof, and the next signage
/// point brings fresh declarations. So a plain capacity bound is correct.
#[derive(Debug)]
pub struct ProofCandidateStore {
    map: HashMap<Bytes32, AcceptedProof>,
    order: VecDeque<Bytes32>,
    cap: usize,
}

impl ProofCandidateStore {
    #[must_use]
    pub fn new(cap: usize) -> Self {
        Self {
            map: HashMap::new(),
            order: VecDeque::new(),
            cap: cap.max(1),
        }
    }

    /// Insert (or refresh) an accepted proof. FIFO-evicts the oldest past the cap.
    pub fn insert(&mut self, proof: AcceptedProof) {
        let key = proof.quality_string;
        if self.map.insert(key, proof).is_none() {
            self.order.push_back(key);
        }
        while self.map.len() > self.cap {
            match self.order.pop_front() {
                Some(oldest) => {
                    self.map.remove(&oldest);
                }
                None => break,
            }
        }
    }

    #[must_use]
    pub fn get(&self, quality_string: &Bytes32) -> Option<&AcceptedProof> {
        self.map.get(quality_string)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

impl Default for ProofCandidateStore {
    fn default() -> Self {
        // A slot carries 64 signage points; a few slots of candidates is ample headroom.
        Self::new(256)
    }
}

#[derive(Debug)]
pub struct CandidateBlockStore {
    map: HashMap<
        Bytes32,
        (
            u32,
            dg_xch_core::blockchain::unfinished_block::UnfinishedBlock,
        ),
    >,
    order: VecDeque<Bytes32>,
    cap: usize,
}

impl CandidateBlockStore {
    #[must_use]
    pub fn new(cap: usize) -> Self {
        Self {
            map: HashMap::new(),
            order: VecDeque::new(),
            cap: cap.max(1),
        }
    }

    /// Insert (or refresh) a candidate for `quality_string`. FIFO-evicts the oldest past the cap.
    pub fn insert(
        &mut self,
        quality_string: Bytes32,
        height: u32,
        block: dg_xch_core::blockchain::unfinished_block::UnfinishedBlock,
    ) {
        if self.map.insert(quality_string, (height, block)).is_none() {
            self.order.push_back(quality_string);
        }
        while self.map.len() > self.cap {
            match self.order.pop_front() {
                Some(oldest) => {
                    self.map.remove(&oldest);
                }
                None => break,
            }
        }
    }

    /// The candidate for `quality_string`.
    #[must_use]
    pub fn get(
        &self,
        quality_string: &Bytes32,
    ) -> Option<&(
        u32,
        dg_xch_core::blockchain::unfinished_block::UnfinishedBlock,
    )> {
        self.map.get(quality_string)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

impl Default for CandidateBlockStore {
    fn default() -> Self {
        Self::new(256)
    }
}

// Candidate assembly (pure). The server resolves the store/slot-derived inputs (peak, prev-block
// backtrack, finished sub-slots, reward-claim walk) and hands them to these functions. Kept pure
// (no store, no locks) so the consensus arithmetic is unit-testable offline, like
// `validate_declared_proof` above.

/// Difficulty + sub-slot-iters at the candidate's sub-slot.
/// Pre-hard-fork / near-genesis (`peak` is `None` or `peak.height <= MAX_SUB_SLOT_BLOCKS`) uses the
/// starting constants; otherwise the peak's own difficulty (`peak.weight - prev_weight`) and
/// `peak.sub_slot_iters`, each overridden by any epoch transition carried in the finished sub-slots being
/// added (`challenge_chain.new_difficulty` / `.new_sub_slot_iters`).
///
/// `peak` carries the peak record and its previous block's weight (`block_record(peak.prev_hash).weight`)
/// The server looks the previous weight up because `SlotState` and this function hold no store.
#[must_use]
pub fn candidate_difficulty_and_ssi(
    constants: &ConsensusConstants,
    peak: Option<(&BlockRecord, u128)>,
    finished_sub_slots: &[SubSlotBundle],
) -> (u64, u64) {
    match peak {
        Some((peak, prev_weight)) if peak.height > constants.max_sub_slot_blocks => {
            // The weight difference is a single block's difficulty and fits u64 by consensus;
            // clamp defensively rather than panic.
            let mut difficulty =
                u64::try_from(peak.weight.saturating_sub(prev_weight)).unwrap_or(u64::MAX);
            let mut sub_slot_iters = peak.sub_slot_iters;
            for ss in finished_sub_slots {
                if let Some(new_difficulty) = ss.challenge_chain.new_difficulty {
                    difficulty = new_difficulty;
                }
                if let Some(new_ssi) = ss.challenge_chain.new_sub_slot_iters {
                    sub_slot_iters = new_ssi;
                }
            }
            (difficulty, sub_slot_iters)
        }
        _ => (
            constants.difficulty_starting,
            constants.sub_slot_iters_starting,
        ),
    }
}

/// The candidate's iteration positions.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct CandidateIters {
    pub required_iters: u64,
    pub sp_iters: u64,
    pub ip_iters: u64,
    /// Becomes `RewardChainBlockUnfinished.total_iters`.
    pub infusion_point_total_iters: u128,
    /// `total_iters_pos_slot + sp_iters` — the empty-block-coercion key.
    pub candidate_sp_total_iters: u128,
}

#[must_use]
// The pospace-to-candidate-iters computation binds this many independent inputs (quality, pos
// size, difficulty, ssi, sp index, cc-sp hash, slot iters); allow the arity.
#[allow(clippy::too_many_arguments)]
pub fn resolve_candidate_iters(
    constants: &ConsensusConstants,
    quality_string: Bytes32,
    pos: &ProofOfSpace,
    difficulty: u64,
    sub_slot_iters: u64,
    signage_point_index: u8,
    // The cc SP output hash the quality is bound to.
    cc_sp_hash: Bytes32,
    total_iters_pos_slot: u128,
) -> Option<CandidateIters> {
    let required_iters = calculate_iterations_quality_for_proof(
        constants,
        pos,
        quality_string,
        difficulty,
        cc_sp_hash,
    );
    let sp_iters = calculate_sp_iters(constants, sub_slot_iters, signage_point_index).ok()?;
    let ip_iters = calculate_ip_iters(
        constants,
        sub_slot_iters,
        signage_point_index,
        required_iters,
    )
    .ok()?;
    let infusion_point_total_iters = calculate_infusion_point_total_iters(
        total_iters_pos_slot,
        sp_iters,
        ip_iters,
        sub_slot_iters,
    );
    let candidate_sp_total_iters = total_iters_pos_slot + u128::from(sp_iters);
    Some(CandidateIters {
        required_iters,
        sp_iters,
        ip_iters,
        infusion_point_total_iters,
        candidate_sp_total_iters,
    })
}

/// The block-store-derived previous-block linkage for a candidate. The server resolves these from
/// the store and passes them in.
#[derive(Clone, Debug)]
pub struct CandidatePrev {
    /// Always `true` for genesis.
    pub is_transaction_block: bool,
    /// `foliage.prev_block_hash` — `GENESIS_CHALLENGE` at height 0, else `prev_b.header_hash`.
    pub prev_block_hash: Bytes32,
    /// `foliage_transaction_block.prev_transaction_block_hash` — `GENESIS_CHALLENGE` when the
    /// previous transaction block is genesis, else that block's `header_hash`. Only consumed for
    /// a tx block.
    pub prev_transaction_block_hash: Bytes32,
    pub prev_transaction_block_height: u32,
    /// `reward_claims_incorporated` inputs — the prev transaction block (with its `fees`)
    /// followed by the non-transaction blocks between it and the transaction block before it
    /// (`fees == 0`). Empty for a non-tx candidate and for genesis.
    pub reward_claims: Vec<RewardBlockClaim>,
}

/// Assemble the candidate `UnfinishedBlock` + the `RequestSignedValues` the farmer must sign.
/// Reuses [`create_unfinished_block_with_sigs`]; the two signage-point signatures come verbatim
/// from `declare` and the two foliage signatures are [`g2_infinity`] placeholders spliced later
/// at `signed_values`.
///
/// `transactions` is the mempool-assembled block generator payload (built by
/// `Mempool::create_block_generator`), or `None` for an empty block. The server passes `Some`
/// only when the candidate IS a transaction block, the empty-block coercion
/// (`candidate_sp_total_iters <= tx_peak.total_iters`) did not fire, and the mempool's peak
/// matches the candidate's previous transaction block. The generator is never attached to a
/// non-tx unfinished candidate, so it never carries a dangling generator on the wire.
///
/// `sp` is the signage point's VDFs: `Some` for signage-point index > 0, `None` at index 0 (the
/// sub-slot start / genesis first SP), where the VDFs are nulled.
///
/// Returns `None` only if the assembled foliage/reward block fails to serialize for hashing.
#[allow(clippy::too_many_arguments)]
#[must_use]
pub fn assemble_candidate(
    constants: &ConsensusConstants,
    declare: &DeclareProofOfSpace,
    quality_string: Bytes32,
    sp: Option<&SignagePoint>,
    finished_sub_slots: Vec<SubSlotBundle>,
    iters: &CandidateIters,
    height: u32,
    prev: &CandidatePrev,
    // The mempool-assembled transactions, None for an empty block.
    transactions: Option<&dg_xch_core::consensus::producer::BlockTransactions>,
    // GENESIS_PRE_FARM at genesis, else pos.pool_contract_puzzle_hash / declare.
    pool_target: PoolTarget,
    // GENESIS_PRE_FARM_FARMER at genesis, else declare.farmer_puzzle_hash.
    farmer_reward_puzzle_hash: Bytes32,
    timestamp: u64,
    // The candidate's `pos_ss_cc_challenge_hash`.
    cc_challenge_hash: Bytes32,
) -> Option<(UnfinishedBlock, RequestSignedValues)> {
    // Index 0 nulls the VDFs; index > 0 carries the real signage-point VDFs/proofs into the
    // reward block + unfinished block.
    let (cc_sp_vdf, cc_sp_proof, rc_sp_vdf, rc_sp_proof) = match sp {
        // The SP fields are already `Option`, so an index-0 sub-slot-start SP (all-None)
        // naturally threads `None` into the reward/unfinished block.
        Some(sp) => (
            sp.cc_vdf,
            sp.cc_proof.clone(),
            sp.rc_vdf,
            sp.rc_proof.clone(),
        ),
        None => (None, None, None, None),
    };
    // cc_sp_hash -> declare.challenge_chain_sp_signature; rc_sp_hash ->
    // declare.reward_chain_sp_signature; foliage hashes -> infinity placeholder (spliced at
    // signed_values). The node never holds the plot key.
    let farmer_sigs = FarmerSignatures {
        challenge_chain_sp_signature: declare.challenge_chain_sp_signature,
        reward_chain_sp_signature: declare.reward_chain_sp_signature,
        foliage_block_data_signature: g2_infinity(),
        foliage_transaction_block_signature: g2_infinity(),
    };
    let block = create_unfinished_block_with_sigs(
        constants,
        iters.infusion_point_total_iters,
        declare.signage_point_index,
        declare.proof_of_space.clone(),
        cc_challenge_hash,
        cc_sp_vdf,
        cc_sp_proof,
        rc_sp_vdf,
        rc_sp_proof,
        finished_sub_slots,
        height,
        prev.is_transaction_block,
        &prev.reward_claims,
        // Some(..) farms the mempool's transactions into this candidate; None is a valid empty
        // transaction block with reward coins only.
        transactions,
        prev.prev_block_hash,
        prev.prev_transaction_block_hash,
        pool_target,
        declare.pool_signature,
        farmer_reward_puzzle_hash,
        timestamp,
        b"",
        farmer_sigs,
    )
    .ok()?;

    // RequestSignedValues carries the two foliage hashes the farmer signs.
    // foliage_transaction_block_hash is zeros for a non-transaction block.
    let foliage_block_data_hash = block.foliage.foliage_block_data.hash().ok()?;
    let foliage_transaction_block_hash = block
        .foliage
        .foliage_transaction_block_hash
        .unwrap_or_default();
    // The signature-source-data fields are populated only when the farmer asked for them
    // (include_signature_source_data); lets a source-data farmer verify what it signs.
    let (foliage_block_data, foliage_transaction_block_data, rc_block_unfinished) =
        if declare.include_signature_source_data {
            (
                Some(block.foliage.foliage_block_data),
                block.foliage_transaction_block,
                Some(block.reward_chain_block.clone()),
            )
        } else {
            (None, None, None)
        };
    let request = RequestSignedValues {
        quality_string,
        foliage_block_data_hash,
        foliage_transaction_block_hash,
        foliage_block_data,
        foliage_transaction_block_data,
        rc_block_unfinished,
    };
    Some((block, request))
}

#[cfg(test)]
#[path = "../tests/unit/farmer/tests.rs"]
mod tests;
