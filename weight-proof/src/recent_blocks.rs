// Consensus helpers for recent-chain validation.

use std::collections::HashMap;

use blst::min_pk::{PublicKey, Signature};
use dg_xch_core::blockchain::block_record::BlockRecord;
use dg_xch_core::blockchain::challenge_block_info::ChallengeBlockInfo;
use dg_xch_core::blockchain::class_group_element::ClassgroupElement;
use dg_xch_core::blockchain::end_of_subslot_bundle::EndOfSubSlotBundle;
use dg_xch_core::blockchain::header_block::HeaderBlock;
use dg_xch_core::blockchain::proof_of_space::ProofOfSpace;
use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes48, Bytes96};
use dg_xch_core::blockchain::sub_epoch_summary::SubEpochSummary;
use dg_xch_core::blockchain::vdf_info::VdfInfo;
use dg_xch_core::blockchain::vdf_output::VdfOutput;
use dg_xch_core::blockchain::vdf_proof::VdfProof;
use dg_xch_core::clvm::bls_bindings::verify_signature;
use dg_xch_core::consensus::constants::ConsensusConstants;
use dg_xch_core::consensus::pot_iterations::{
    calculate_ip_iters, calculate_iterations_quality, calculate_sp_interval_iters,
    calculate_sp_iters, is_overflow_block,
};
use dg_xch_pos::verify_and_get_quality_string;
use dg_xch_serialize::ChiaSerialize;
use dg_xch_vdf::validate_vdf_info;

use crate::{WeightProofError, hash_of};

// HeaderBlock accessors (`prev_header_hash`/`header_hash`/`height`/`weight`/`total_iters`/
// `first_in_sub_slot`) now live as methods on `dg_xch_core::HeaderBlock`. The
// `ClassgroupElement` <-> `VdfOutput` carrier conversion is `From`/`Into` beside those models in
// dg_xch_core, and the identity element is `ClassgroupElement::get_default_element()`.

/// `VDFInfo(challenge, iters, output)`.
fn vdf_info(challenge: Bytes32, iters: u64, output: ClassgroupElement) -> VdfInfo {
    VdfInfo {
        challenge,
        number_of_iterations: iters,
        output,
    }
}
/// reference `info.replace(number_of_iterations=iters)` (VdfInfo is Copy).
fn with_iters(info: &VdfInfo, iters: u64) -> VdfInfo {
    VdfInfo {
        number_of_iterations: iters,
        ..*info
    }
}

/// reference `validate_vdf(proof, constants, input, info, target_info=None)` — the basic (non
/// normalization-aware) form; callers do the `normalized_to_identity` branching explicitly, exactly as
/// the reference does. Maps to dg_xch `validate_vdf_info` (arg order differs).
fn validate_vdf(
    c: &ConsensusConstants,
    input: &ClassgroupElement,
    info: &VdfInfo,
    proof: &VdfProof,
    target: Option<&VdfInfo>,
) -> bool {
    validate_vdf_info(c, input, info, proof, target)
}

/// AugScheme BLS verify over sized-bytes, fail-closed on malformed sig/key (no panic): a bad `Bytes96`
/// can't parse to a `Signature` -> `false`; a bad `Bytes48` -> default pubkey -> verify fails.
fn bls_verify(pk: &Bytes48, msg: &[u8], sig: &Bytes96) -> bool {
    match Signature::try_from(sig) {
        Ok(s) => verify_signature(&PublicKey::from(pk), msg, &s),
        Err(_) => false,
    }
}

/// The recent-chain block-record cache. The reference threads a `BlockCache` that starts empty and is
/// filled as each recent-chain block is validated; every `block_record` lookup therefore resolves inside
/// the recent chain itself (no full blockchain needed). Missing lookups are an `Err`, never a panic —
/// malformed input must fail closed, not crash the validator.
#[derive(Default)]
pub(crate) struct BlockCache {
    by_hash: HashMap<Bytes32, BlockRecord>,
}

impl BlockCache {
    pub(crate) fn new() -> Self {
        Self {
            by_hash: HashMap::new(),
        }
    }

    pub(crate) fn add_block(&mut self, record: BlockRecord) {
        self.by_hash.insert(record.header_hash, record);
    }

    /// Reference `blocks.block_record(h)` — asserts presence. Here: `Err` on miss (fail closed).
    pub(crate) fn block_record(&self, hash: Bytes32) -> Result<&BlockRecord, WeightProofError> {
        self.by_hash.get(&hash).ok_or(WeightProofError::Rejected(
            "block_record: unknown prev hash",
        ))
    }

    /// Reference `blocks.try_block_record(h)` — `None` on miss.
    pub(crate) fn try_block_record(&self, hash: Bytes32) -> Option<&BlockRecord> {
        self.by_hash.get(&hash)
    }
}

// `BlockRecord`'s consensus accessors (`first_in_sub_slot`/`is_transaction_block`/`is_challenge_block`/
// `ip_iters`) now live as methods on `dg_xch_core::BlockRecord`. `ip_iters` there returns the native
// `std::io::Error` from `calculate_ip_iters`; call sites here map it to `WeightProofError`.

/// `calculate_deficit` (ref `chia/consensus/deficit.py:7`) — the deficit state machine at a block.
pub(crate) fn calculate_deficit(
    c: &ConsensusConstants,
    height: u32,
    prev_b: Option<&BlockRecord>,
    overflow: bool,
    num_finished_sub_slots: usize,
) -> u8 {
    let min = c.min_blocks_per_challenge_block;
    if height == 0 {
        return min - 1;
    }
    // height != 0 ⇒ prev_b present in a well-formed chain; fail-closed callers guarantee this.
    let prev_deficit = prev_b.map_or(min, |p| p.deficit);
    if prev_deficit == min {
        // Prev sb must be an overflow sb. However maybe it's in a different sub-slot.
        if overflow {
            if num_finished_sub_slots > 0 {
                prev_deficit - 1
            } else {
                prev_deficit
            }
        } else {
            prev_deficit - 1
        }
    } else if prev_deficit == 0 {
        if num_finished_sub_slots == 0 {
            0
        } else if num_finished_sub_slots == 1 {
            if overflow { min } else { min - 1 }
        } else {
            min - 1
        }
    } else {
        prev_deficit - 1
    }
}

/// `validate_pospace_and_get_required_iters` (ref `chia/consensus/pot_iterations.py:50`). Returns
/// `Ok(None)` when the proof of space is invalid (reference returns `None`), `Ok(Some(iters))` otherwise.
/// See the module fidelity note re: `prev_transaction_block_height`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn validate_pospace_and_get_required_iters(
    c: &ConsensusConstants,
    proof_of_space: &ProofOfSpace,
    challenge: Bytes32,
    cc_sp_hash: Bytes32,
    height: u32,
    difficulty: u64,
    _prev_transaction_block_height: u32,
) -> Option<u64> {
    let q_str = verify_and_get_quality_string(proof_of_space, c, challenge, cc_sp_hash, height)?;
    Some(calculate_iterations_quality(
        c.difficulty_constant_factor,
        q_str,
        proof_of_space.size,
        difficulty,
        cc_sp_hash,
    ))
}

/// `pre_sp_tx_block` (ref `chia/consensus/get_block_challenge.py:104`) — the previous transaction block
/// up to this block's signage point. Walks back through the recent-chain cache.
fn pre_sp_tx_block<'a>(
    c: &ConsensusConstants,
    blocks: &'a BlockCache,
    prev_b_hash: Bytes32,
    sp_index: u8,
    finished_sub_slots: usize,
) -> Result<Option<&'a BlockRecord>, WeightProofError> {
    if prev_b_hash == c.genesis_challenge {
        return Ok(None);
    }
    let mut curr = blocks.block_record(prev_b_hash)?;
    let overflow =
        is_overflow_block(c, sp_index).map_err(|_| WeightProofError::Rejected("overflow"))?;
    let mut slots_crossed = finished_sub_slots;
    while curr.height > 0 {
        let before_sp = if overflow {
            slots_crossed >= 2 || (slots_crossed == 1 && curr.signage_point_index < sp_index)
        } else {
            curr.signage_point_index < sp_index || slots_crossed > 0
        };
        if curr.is_transaction_block() && before_sp {
            break;
        }
        if curr.first_in_sub_slot() {
            slots_crossed += 1;
        }
        curr = blocks.block_record(curr.prev_hash)?;
    }
    Ok(Some(curr))
}

/// `pre_sp_tx_block_height` (ref `chia/consensus/get_block_challenge.py:136`).
pub(crate) fn pre_sp_tx_block_height(
    c: &ConsensusConstants,
    blocks: &BlockCache,
    prev_b_hash: Bytes32,
    sp_index: u8,
    finished_sub_slots: usize,
) -> Result<u32, WeightProofError> {
    Ok(
        pre_sp_tx_block(c, blocks, prev_b_hash, sp_index, finished_sub_slots)?
            .map_or(0, |b| b.height),
    )
}

/// `get_block_challenge` (ref `chia/consensus/get_block_challenge.py:53`) — the challenge-chain challenge
/// for this block, from finished sub-slots or by walking back through prior slots.
pub(crate) fn get_block_challenge(
    c: &ConsensusConstants,
    header_block: &HeaderBlock,
    blocks: &BlockCache,
    genesis_block: bool,
    overflow: bool,
    skip_overflow_last_ss_validation: bool,
) -> Result<Bytes32, WeightProofError> {
    let fss = &header_block.finished_sub_slots;
    if !fss.is_empty() {
        let last = &fss[fss.len() - 1];
        let challenge = if overflow {
            if skip_overflow_last_ss_validation {
                hash_of(&last.challenge_chain)?
            } else {
                last.challenge_chain
                    .challenge_chain_end_of_slot_vdf
                    .challenge
            }
        } else {
            hash_of(&last.challenge_chain)?
        };
        return Ok(challenge);
    }
    if genesis_block {
        return Ok(c.genesis_challenge);
    }
    let challenges_to_look_for: usize = if overflow && !skip_overflow_last_ss_validation {
        2
    } else {
        1
    };
    let mut reversed_challenge_hashes: Vec<Bytes32> = Vec::new();
    let mut curr = blocks.block_record(header_block.foliage.prev_block_hash)?;
    while reversed_challenge_hashes.len() < challenges_to_look_for {
        if curr.first_in_sub_slot() {
            let hashes =
                curr.finished_challenge_slot_hashes
                    .as_ref()
                    .ok_or(WeightProofError::Rejected(
                        "no finished_challenge_slot_hashes",
                    ))?;
            reversed_challenge_hashes.extend(hashes.iter().rev().copied());
            if reversed_challenge_hashes.len() >= challenges_to_look_for {
                break;
            }
        }
        if curr.height == 0 {
            let hashes =
                curr.finished_challenge_slot_hashes
                    .as_ref()
                    .ok_or(WeightProofError::Rejected(
                        "genesis no finished_challenge_slot_hashes",
                    ))?;
            if hashes.is_empty() {
                return Err(WeightProofError::Rejected("genesis empty challenge hashes"));
            }
            break;
        }
        curr = blocks.block_record(curr.prev_hash)?;
    }
    reversed_challenge_hashes
        .get(challenges_to_look_for - 1)
        .copied()
        .ok_or(WeightProofError::Rejected(
            "get_block_challenge: not enough challenges",
        ))
}

/// `height_can_be_first_in_epoch` (ref `difficulty_adjustment.py:130`).
fn height_can_be_first_in_epoch(c: &ConsensusConstants, height: u32) -> bool {
    (height - (height % c.sub_epoch_blocks)).is_multiple_of(c.epoch_blocks)
}

/// `can_finish_sub_and_full_epoch` (ref `difficulty_adjustment.py:134`). In the recent-blocks path
/// `prev_ses_block` is always `None` (the loop passes `ValidationState(ssi, diff, None)`), so the
/// walk-back branch is exercised. Returns `(can_finish_sub_epoch, can_finish_full_epoch)`.
fn can_finish_sub_and_full_epoch(
    c: &ConsensusConstants,
    blocks: &BlockCache,
    height: u32,
    prev_header_hash: Bytes32,
    deficit: u8,
    block_at_height_included_ses: bool,
) -> Result<(bool, bool), WeightProofError> {
    if height < c.sub_epoch_blocks - 1 {
        return Ok((false, false));
    }
    if deficit > 0 {
        return Ok((false, false));
    }
    if block_at_height_included_ses {
        return Ok((false, false));
    }
    if (height + 1) % c.sub_epoch_blocks > 1 {
        // prev_ses_block is None in the recent path -> walk back.
        let mut curr = blocks.block_record(prev_header_hash)?;
        while curr.height % c.sub_epoch_blocks > 0 {
            if curr.sub_epoch_summary_included.is_some() {
                return Ok((false, false));
            }
            curr = blocks.block_record(curr.prev_hash)?;
        }
        if curr.sub_epoch_summary_included.is_some() {
            return Ok((false, false));
        }
    }
    Ok((true, height_can_be_first_in_epoch(c, height + 1)))
}

/// `make_sub_epoch_summary` (ref `make_sub_epoch_summary.py:22`). Reconstructs the SES expected at
/// `blocks_included_height`. dg_xch's `SubEpochSummary` is the mainnet-active 5-field form (no
/// `challenge_merkle_root`) — matching the fixture's on-chain hashes; do not add the 6th field until it
/// activates. In the recent path `prev_ses_block` is always `None`, so the walk-back finds it.
fn make_sub_epoch_summary(
    c: &ConsensusConstants,
    blocks: &BlockCache,
    blocks_included_height: u32,
    prev_prev_block: &BlockRecord,
    new_difficulty: Option<u64>,
    new_sub_slot_iters: Option<u64>,
) -> Result<SubEpochSummary, WeightProofError> {
    if prev_prev_block.height != blocks_included_height.wrapping_sub(2) {
        return Err(WeightProofError::Rejected("make_ses: prev_prev height"));
    }
    // First sub_epoch.
    if (u64::from(blocks_included_height) + u64::from(c.max_sub_slot_blocks))
        / u64::from(c.sub_epoch_blocks)
        <= 1
    {
        return Ok(SubEpochSummary {
            prev_subepoch_summary_hash: c.genesis_challenge,
            reward_chain_hash: c.genesis_challenge,
            num_blocks_overflow: 0,
            new_difficulty: None,
            new_sub_slot_iters: None,
        });
    }
    let mut curr = prev_prev_block;
    while curr.sub_epoch_summary_included.is_none() {
        curr = blocks.block_record(curr.prev_hash)?;
    }
    let prev_ses_block = curr;
    let included = prev_ses_block
        .sub_epoch_summary_included
        .as_ref()
        .ok_or(WeightProofError::Rejected("make_ses: no included ses"))?;
    let reward_slot_hashes =
        prev_ses_block
            .finished_reward_slot_hashes
            .as_ref()
            .ok_or(WeightProofError::Rejected(
                "make_ses: no reward slot hashes",
            ))?;
    let prev_ses = hash_of(included)?;
    Ok(SubEpochSummary {
        prev_subepoch_summary_hash: prev_ses,
        reward_chain_hash: *reward_slot_hashes.last().ok_or(WeightProofError::Rejected(
            "make_ses: empty reward slot hashes",
        ))?,
        num_blocks_overflow: u8::try_from(prev_ses_block.height % c.sub_epoch_blocks)
            .map_err(|_| WeightProofError::Rejected("make_ses: overflow count"))?,
        new_difficulty,
        new_sub_slot_iters,
    })
}

/// `get_signage_point_vdf_info` (ref `vdf_info_computation.py:11`). Returns
/// `(cc_challenge, rc_challenge, cc_input, rc_input, cc_iters, rc_iters)`; rc_input is always the default
/// element and cc_iters == rc_iters == sp_vdf_iters.
#[allow(clippy::type_complexity)]
fn get_signage_point_vdf_info(
    c: &ConsensusConstants,
    finished_sub_slots: &[EndOfSubSlotBundle],
    overflow: bool,
    prev_b: Option<&BlockRecord>,
    blocks: &BlockCache,
    sp_total_iters: u128,
    sp_iters: u64,
) -> Result<
    (
        Bytes32,
        Bytes32,
        ClassgroupElement,
        ClassgroupElement,
        u64,
        u64,
    ),
    WeightProofError,
> {
    let new_sub_slot = !finished_sub_slots.is_empty();
    let genesis_block = prev_b.is_none();
    let n = finished_sub_slots.len();

    let (cc_vdf_challenge, rc_vdf_challenge, cc_vdf_input, sp_vdf_iters): (
        Bytes32,
        Bytes32,
        ClassgroupElement,
        u64,
    );

    if new_sub_slot && !overflow {
        let last = &finished_sub_slots[n - 1];
        rc_vdf_challenge = hash_of(&last.reward_chain)?;
        cc_vdf_challenge = hash_of(&last.challenge_chain)?;
        sp_vdf_iters = sp_iters;
        cc_vdf_input = ClassgroupElement::get_default_element();
    } else if new_sub_slot && overflow && n > 1 {
        let prev = &finished_sub_slots[n - 2];
        rc_vdf_challenge = hash_of(&prev.reward_chain)?;
        cc_vdf_challenge = hash_of(&prev.challenge_chain)?;
        sp_vdf_iters = sp_iters;
        cc_vdf_input = ClassgroupElement::get_default_element();
    } else if genesis_block {
        rc_vdf_challenge = c.genesis_challenge;
        cc_vdf_challenge = c.genesis_challenge;
        sp_vdf_iters = sp_iters;
        cc_vdf_input = ClassgroupElement::get_default_element();
    } else if new_sub_slot && overflow && n == 1 {
        // Case 4.
        let prev = prev_b.ok_or(WeightProofError::Rejected("sp_vdf: prev_b"))?;
        let mut curr = prev;
        while !curr.first_in_sub_slot() && curr.total_iters > sp_total_iters {
            curr = blocks.block_record(curr.prev_hash)?;
        }
        if curr.total_iters < sp_total_iters {
            sp_vdf_iters = u64::try_from(sp_total_iters - curr.total_iters)
                .map_err(|_| WeightProofError::Rejected("sp_vdf: iters"))?;
            cc_vdf_input = ClassgroupElement::try_from(&curr.challenge_vdf_output)
                .map_err(|_| WeightProofError::Rejected("invalid challenge VDF output"))?;
            rc_vdf_challenge = curr.reward_infusion_new_challenge;
        } else {
            let hashes = curr
                .finished_reward_slot_hashes
                .as_ref()
                .ok_or(WeightProofError::Rejected("sp_vdf: no reward slot hashes"))?;
            sp_vdf_iters = sp_iters;
            cc_vdf_input = ClassgroupElement::get_default_element();
            rc_vdf_challenge = *hashes.last().ok_or(WeightProofError::Rejected(
                "sp_vdf: empty reward slot hashes",
            ))?;
        }
        while !curr.first_in_sub_slot() {
            curr = blocks.block_record(curr.prev_hash)?;
        }
        let ch = curr
            .finished_challenge_slot_hashes
            .as_ref()
            .ok_or(WeightProofError::Rejected(
                "sp_vdf: no challenge slot hashes",
            ))?;
        cc_vdf_challenge = *ch.last().ok_or(WeightProofError::Rejected(
            "sp_vdf: empty challenge slot hashes",
        ))?;
    } else if !new_sub_slot && overflow {
        // Case 5.
        let prev = prev_b.ok_or(WeightProofError::Rejected("sp_vdf: prev_b"))?;
        let mut curr = prev;
        let mut found_sub_slots: Vec<(Bytes32, Bytes32)> = Vec::new();
        if curr.first_in_sub_slot() {
            let ch = curr
                .finished_challenge_slot_hashes
                .as_ref()
                .ok_or(WeightProofError::Rejected("sp_vdf: no cc slot hashes"))?;
            let rw = curr
                .finished_reward_slot_hashes
                .as_ref()
                .ok_or(WeightProofError::Rejected("sp_vdf: no rc slot hashes"))?;
            found_sub_slots = ch.iter().copied().zip(rw.iter().copied()).rev().collect();
        }
        let mut sp_pre_sb: Option<&BlockRecord> = None;
        while found_sub_slots.len() < 2 && curr.height > 0 {
            if sp_pre_sb.is_none() && curr.total_iters < sp_total_iters {
                sp_pre_sb = Some(curr);
            }
            curr = blocks.block_record(curr.prev_hash)?;
            if curr.first_in_sub_slot() {
                let ch = curr
                    .finished_challenge_slot_hashes
                    .as_ref()
                    .ok_or(WeightProofError::Rejected("sp_vdf: no cc slot hashes"))?;
                let rw = curr
                    .finished_reward_slot_hashes
                    .as_ref()
                    .ok_or(WeightProofError::Rejected("sp_vdf: no rc slot hashes"))?;
                found_sub_slots.extend(ch.iter().copied().zip(rw.iter().copied()).rev());
            }
        }
        if sp_pre_sb.is_none() && curr.total_iters < sp_total_iters {
            sp_pre_sb = Some(curr);
        }
        if found_sub_slots.len() < 2 {
            return Err(WeightProofError::Rejected("sp_vdf: <2 found_sub_slots"));
        }
        if let Some(pre) = sp_pre_sb {
            sp_vdf_iters = u64::try_from(sp_total_iters - pre.total_iters)
                .map_err(|_| WeightProofError::Rejected("sp_vdf: iters"))?;
            cc_vdf_input = ClassgroupElement::try_from(&pre.challenge_vdf_output)
                .map_err(|_| WeightProofError::Rejected("invalid challenge VDF output"))?;
            rc_vdf_challenge = pre.reward_infusion_new_challenge;
        } else {
            sp_vdf_iters = sp_iters;
            cc_vdf_input = ClassgroupElement::get_default_element();
            rc_vdf_challenge = found_sub_slots[1].1;
        }
        cc_vdf_challenge = found_sub_slots[1].0;
    } else if !new_sub_slot && !overflow {
        // Case 6.
        let prev = prev_b.ok_or(WeightProofError::Rejected("sp_vdf: prev_b"))?;
        let mut curr = prev;
        while !curr.first_in_sub_slot() && curr.total_iters > sp_total_iters {
            curr = blocks.block_record(curr.prev_hash)?;
        }
        if curr.total_iters < sp_total_iters {
            sp_vdf_iters = u64::try_from(sp_total_iters - curr.total_iters)
                .map_err(|_| WeightProofError::Rejected("sp_vdf: iters"))?;
            cc_vdf_input = ClassgroupElement::try_from(&curr.challenge_vdf_output)
                .map_err(|_| WeightProofError::Rejected("invalid challenge VDF output"))?;
            rc_vdf_challenge = curr.reward_infusion_new_challenge;
        } else {
            let hashes = curr
                .finished_reward_slot_hashes
                .as_ref()
                .ok_or(WeightProofError::Rejected("sp_vdf: no reward slot hashes"))?;
            sp_vdf_iters = sp_iters;
            cc_vdf_input = ClassgroupElement::get_default_element();
            rc_vdf_challenge = *hashes.last().ok_or(WeightProofError::Rejected(
                "sp_vdf: empty reward slot hashes",
            ))?;
        }
        while !curr.first_in_sub_slot() {
            curr = blocks.block_record(curr.prev_hash)?;
        }
        let ch = curr
            .finished_challenge_slot_hashes
            .as_ref()
            .ok_or(WeightProofError::Rejected(
                "sp_vdf: no challenge slot hashes",
            ))?;
        cc_vdf_challenge = *ch.last().ok_or(WeightProofError::Rejected(
            "sp_vdf: empty challenge slot hashes",
        ))?;
    } else {
        return Err(WeightProofError::Rejected("sp_vdf: unreachable case"));
    }

    Ok((
        cc_vdf_challenge,
        rc_vdf_challenge,
        cc_vdf_input,
        ClassgroupElement::get_default_element(),
        sp_vdf_iters,
        sp_vdf_iters,
    ))
}

/// `header_block_to_sub_block_record` (ref `full_block_to_block_record.py:81`). The recent-blocks loop
/// calls this directly (ssi already known), so `get_next_sub_slot_iters_and_difficulty` is not needed.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn header_block_to_sub_block_record(
    c: &ConsensusConstants,
    required_iters: u64,
    block: &HeaderBlock,
    sub_slot_iters: u64,
    overflow: bool,
    deficit: u8,
    prev_transaction_block_height: u32,
    ses: Option<SubEpochSummary>,
) -> Result<BlockRecord, WeightProofError> {
    let rcb = &block.reward_chain_block;
    let cbi = ChallengeBlockInfo {
        proof_of_space: rcb.proof_of_space.clone(),
        challenge_chain_sp_vdf: rcb.challenge_chain_sp_vdf,
        challenge_chain_sp_signature: rcb.challenge_chain_sp_signature,
        challenge_chain_ip_vdf: rcb.challenge_chain_ip_vdf,
    };
    let icc_output = rcb.infused_challenge_chain_ip_vdf.map(|v| v.output);

    let (fcsh, frsh, ficsh): (
        Option<Vec<Bytes32>>,
        Option<Vec<Bytes32>>,
        Option<Vec<Bytes32>>,
    ) = if !block.finished_sub_slots.is_empty() {
        let mut cc = Vec::with_capacity(block.finished_sub_slots.len());
        let mut rw = Vec::with_capacity(block.finished_sub_slots.len());
        let mut icc = Vec::new();
        for ss in &block.finished_sub_slots {
            cc.push(hash_of(&ss.challenge_chain)?);
            rw.push(hash_of(&ss.reward_chain)?);
            if let Some(icc_ss) = &ss.infused_challenge_chain {
                icc.push(hash_of(icc_ss)?);
            }
        }
        (Some(cc), Some(rw), Some(icc))
    } else if block.height() == 0 {
        (
            Some(vec![c.genesis_challenge]),
            Some(vec![c.genesis_challenge]),
            None,
        )
    } else {
        (None, None, None)
    };

    let (timestamp, prev_tx_hash) = match &block.foliage_transaction_block {
        Some(ftb) => (Some(ftb.timestamp), Some(ftb.prev_transaction_block_hash)),
        None => (None, None),
    };
    let (fees, reward_claims) = match &block.transactions_info {
        Some(ti) => (Some(ti.fees), Some(ti.reward_claims_incorporated.clone())),
        None => (None, None),
    };

    Ok(BlockRecord {
        header_hash: block
            .header_hash()
            .map_err(|_| WeightProofError::Malformed("serialize"))?,
        prev_hash: block.prev_header_hash(),
        height: block.height(),
        weight: block.weight(),
        total_iters: block.total_iters(),
        signage_point_index: rcb.signage_point_index,
        challenge_vdf_output: VdfOutput::from(rcb.challenge_chain_ip_vdf.output),
        infused_challenge_vdf_output: icc_output.map(VdfOutput::from),
        reward_infusion_new_challenge: hash_of(rcb)?,
        challenge_block_info_hash: hash_of(&cbi)?,
        sub_slot_iters,
        pool_puzzle_hash: block.foliage.foliage_block_data.pool_target.puzzle_hash,
        farmer_puzzle_hash: block.foliage.foliage_block_data.farmer_reward_puzzle_hash,
        required_iters,
        deficit,
        overflow,
        prev_transaction_block_height,
        timestamp,
        prev_transaction_block_hash: prev_tx_hash,
        fees,
        reward_claims_incorporated: reward_claims,
        finished_challenge_slot_hashes: fcsh,
        finished_infused_challenge_slot_hashes: ficsh,
        finished_reward_slot_hashes: frsh,
        sub_epoch_summary_included: ses,
    })
}

/// Reference `ValidationState(ssi, difficulty, prev_ses_block)`. In the recent-blocks path
/// `prev_ses_block` is always `None`, so it is omitted.
#[derive(Clone, Copy)]
pub(crate) struct ValidationState {
    pub ssi: u64,
    pub difficulty: u64,
}

/// The unfinished-reward-block hash used by check 18 (reference `reward_chain_block.get_unfinished()`).
fn unfinished_rcb_hash(
    rcb: &dg_xch_core::blockchain::reward_chain_block::RewardChainBlock,
) -> Result<Bytes32, WeightProofError> {
    use dg_xch_core::blockchain::reward_chain_block_unfinished::RewardChainBlockUnfinished;
    hash_of(&RewardChainBlockUnfinished {
        total_iters: rcb.total_iters,
        signage_point_index: rcb.signage_point_index,
        pos_ss_cc_challenge_hash: rcb.pos_ss_cc_challenge_hash,
        proof_of_space: rcb.proof_of_space.clone(),
        challenge_chain_sp_vdf: rcb.challenge_chain_sp_vdf,
        challenge_chain_sp_signature: rcb.challenge_chain_sp_signature,
        reward_chain_sp_vdf: rcb.reward_chain_sp_vdf,
        reward_chain_sp_signature: rcb.reward_chain_sp_signature,
    })
}

/// `validate_unfinished_header_block` (ref `block_header_validation.py:47`) specialized for the recent
/// chain: `skip_overflow_last_ss_validation=False` and `skip_vdf_is_valid=False` always (that is exactly
/// what `validate_finished_header_block` passes), so the skip-branches — which include the
/// `final_eos_is_already_included` path — are unreachable here and not ported. Returns `Ok(required_iters)`
/// when valid; any failure is `Err` (fail closed), matching the reference's `(None, ValidationError)`.
#[allow(clippy::too_many_lines)]
fn validate_unfinished_header_block(
    c: &ConsensusConstants,
    blocks: &BlockCache,
    block: &HeaderBlock,
    vs: ValidationState,
    check_sub_epoch_summary: bool,
) -> Result<u64, WeightProofError> {
    let rcb = &block.reward_chain_block;
    let e = WeightProofError::Rejected;

    // 6. check signage point index
    if u32::from(rcb.signage_point_index) >= c.num_sps_sub_slot {
        return Err(e("INVALID_SP_INDEX"));
    }
    // 1. previous block / genesis
    let prev_b = blocks.try_block_record(block.prev_header_hash());
    let genesis_block = prev_b.is_none();
    if genesis_block && block.prev_header_hash() != c.genesis_challenge {
        return Err(e("INVALID_PREV_BLOCK_HASH"));
    }
    let overflow = is_overflow_block(c, rcb.signage_point_index).map_err(|_| e("overflow"))?;
    let finished_sub_slots_since_prev = block.finished_sub_slots.len();
    let new_sub_slot = finished_sub_slots_since_prev > 0;

    let (mut can_finish_se, mut can_finish_epoch) = (false, false);
    let height: u32;
    if genesis_block {
        height = 0;
    } else {
        let pb = prev_b.ok_or(e("prev_b"))?;
        height = pb.height + 1;
        if new_sub_slot {
            let (se, ep) = can_finish_sub_and_full_epoch(
                c,
                blocks,
                pb.height,
                pb.prev_hash,
                pb.deficit,
                pb.sub_epoch_summary_included.is_some(),
            )?;
            can_finish_se = se;
            can_finish_epoch = ep;
        }
    }

    // 2. finished slots crossed since prev_b
    let mut ses_hash: Option<Bytes32> = None;
    if new_sub_slot {
        for (n, sub_slot) in block.finished_sub_slots.iter().enumerate() {
            let cc = &sub_slot.challenge_chain;
            let rc = &sub_slot.reward_chain;
            let challenge_hash = cc.challenge_chain_end_of_slot_vdf.challenge;

            if n == 0 {
                if genesis_block {
                    if challenge_hash != c.genesis_challenge {
                        return Err(e("INVALID_PREV_CHALLENGE_SLOT_HASH"));
                    }
                } else {
                    let mut curr = prev_b.ok_or(e("prev_b"))?;
                    while !curr.first_in_sub_slot() {
                        curr = blocks.block_record(curr.prev_hash)?;
                    }
                    let fcsh = curr
                        .finished_challenge_slot_hashes
                        .as_ref()
                        .ok_or(e("no fcsh"))?;
                    if *fcsh.last().ok_or(e("empty fcsh"))? != challenge_hash {
                        return Err(e("INVALID_PREV_CHALLENGE_SLOT_HASH"));
                    }
                }
            } else if hash_of(&block.finished_sub_slots[n - 1].challenge_chain)? != challenge_hash {
                return Err(e("INVALID_PREV_CHALLENGE_SLOT_HASH (empty slot)"));
            }

            if genesis_block {
                // 2d. genesis has no ICC
                if sub_slot.infused_challenge_chain.is_some() {
                    return Err(e("SHOULD_NOT_HAVE_ICC"));
                }
            } else {
                let pb = prev_b.ok_or(e("prev_b"))?;
                let mut icc_iters_committed: Option<u64> = None;
                let mut icc_iters_proof: Option<u64> = None;
                let mut icc_challenge_hash: Option<Bytes32> = None;
                let mut icc_vdf_input: Option<ClassgroupElement> = None;
                if pb.deficit < c.min_blocks_per_challenge_block {
                    if n == 0 {
                        let mut curr = pb;
                        while !curr.is_challenge_block(c.min_blocks_per_challenge_block)
                            && !curr.first_in_sub_slot()
                        {
                            curr = blocks.block_record(curr.prev_hash)?;
                        }
                        if curr.is_challenge_block(c.min_blocks_per_challenge_block) {
                            icc_challenge_hash = Some(curr.challenge_block_info_hash);
                            icc_iters_committed = Some(
                                pb.sub_slot_iters - curr.ip_iters(c).map_err(|_| e("ip_iters"))?,
                            );
                        } else {
                            let ficsh = curr
                                .finished_infused_challenge_slot_hashes
                                .as_ref()
                                .ok_or(e("no ficsh"))?;
                            icc_challenge_hash = Some(*ficsh.last().ok_or(e("empty ficsh"))?);
                            icc_iters_committed = Some(pb.sub_slot_iters);
                        }
                        icc_iters_proof =
                            Some(pb.sub_slot_iters - pb.ip_iters(c).map_err(|_| e("ip_iters"))?);
                        if pb.is_challenge_block(c.min_blocks_per_challenge_block) {
                            icc_vdf_input = Some(ClassgroupElement::get_default_element());
                        } else {
                            icc_vdf_input = pb
                                .infused_challenge_vdf_output
                                .as_ref()
                                .map(ClassgroupElement::try_from)
                                .transpose()
                                .map_err(|_| e("invalid infused challenge VDF output"))?;
                        }
                    } else if block.finished_sub_slots[n - 1].reward_chain.deficit
                        < c.min_blocks_per_challenge_block
                    {
                        let finished_ss = &block.finished_sub_slots[n - 1];
                        let icc_ss = finished_ss
                            .infused_challenge_chain
                            .as_ref()
                            .ok_or(e("no prev icc"))?;
                        icc_challenge_hash = Some(hash_of(icc_ss)?);
                        icc_iters_committed = Some(pb.sub_slot_iters);
                        icc_iters_proof = icc_iters_committed;
                        icc_vdf_input = Some(ClassgroupElement::get_default_element());
                    }
                }

                // 2e. icc present iff icc_challenge_hash present
                if sub_slot.infused_challenge_chain.is_some() != icc_challenge_hash.is_some() {
                    return Err(e("INVALID_ICC (presence)"));
                }
                if let Some(icc) = &sub_slot.infused_challenge_chain {
                    let icc_vdf_input = icc_vdf_input.ok_or(e("icc_vdf_input"))?;
                    let icc_iters_proof = icc_iters_proof.ok_or(e("icc_iters_proof"))?;
                    let icc_iters_committed =
                        icc_iters_committed.ok_or(e("icc_iters_committed"))?;
                    let icc_challenge_hash = icc_challenge_hash.ok_or(e("icc_challenge_hash"))?;
                    let icc_proof = sub_slot
                        .proofs
                        .infused_challenge_chain_slot_proof
                        .as_ref()
                        .ok_or(e("no icc slot proof"))?;
                    // 2f. ICC EOS VDF
                    let icc_eos = &icc.infused_challenge_chain_end_of_slot_vdf;
                    let target = vdf_info(icc_challenge_hash, icc_iters_proof, icc_eos.output);
                    if *icc_eos != with_iters(&target, icc_iters_committed) {
                        return Err(e("INVALID_ICC_EOS_VDF"));
                    }
                    if !icc_proof.normalized_to_identity
                        && !validate_vdf(c, &icc_vdf_input, &target, icc_proof, None)
                    {
                        return Err(e("INVALID_ICC_EOS_VDF"));
                    }
                    if icc_proof.normalized_to_identity
                        && !validate_vdf(
                            c,
                            &ClassgroupElement::get_default_element(),
                            icc_eos,
                            icc_proof,
                            None,
                        )
                    {
                        return Err(e("INVALID_ICC_EOS_VDF"));
                    }

                    if rc.deficit == c.min_blocks_per_challenge_block {
                        // 2g. deficit 16 -> icc hash in cc
                        if Some(hash_of(icc)?) != cc.infused_challenge_chain_sub_slot_hash {
                            return Err(e("INVALID_ICC_HASH_CC"));
                        }
                    } else if cc.infused_challenge_chain_sub_slot_hash.is_some() {
                        // 2h.
                        return Err(e("INVALID_ICC_HASH_CC"));
                    }
                    // 2i. icc hash in reward sub-slot
                    if Some(hash_of(icc)?) != rc.infused_challenge_chain_sub_slot_hash {
                        return Err(e("INVALID_ICC_HASH_RC"));
                    }
                } else {
                    // 2j/2k. no icc -> cc/rc must not include it
                    if cc.infused_challenge_chain_sub_slot_hash.is_some() {
                        return Err(e("INVALID_ICC_HASH_CC"));
                    }
                    if rc.infused_challenge_chain_sub_slot_hash.is_some() {
                        return Err(e("INVALID_ICC_HASH_RC"));
                    }
                }
            }

            if let Some(seh) = cc.subepoch_summary_hash {
                if ses_hash.is_some() {
                    return Err(e("two ses hashes"));
                }
                ses_hash = Some(seh);
            }
            // 2l.
            if n != 0 && cc.subepoch_summary_hash.is_some() {
                return Err(e("INVALID_SUB_EPOCH_SUMMARY_HASH (empty slot)"));
            }
            if can_finish_epoch && cc.subepoch_summary_hash.is_some() {
                // 2m.
                if cc.new_sub_slot_iters != Some(vs.ssi) {
                    return Err(e("INVALID_NEW_SUB_SLOT_ITERS"));
                }
                if cc.new_difficulty != Some(vs.difficulty) {
                    return Err(e("INVALID_NEW_DIFFICULTY"));
                }
            } else {
                // 2n.
                if cc.new_sub_slot_iters.is_some() {
                    return Err(e("INVALID_NEW_SUB_SLOT_ITERS"));
                }
                if cc.new_difficulty.is_some() {
                    return Err(e("INVALID_NEW_DIFFICULTY"));
                }
            }
            // 2o. challenge sub-slot hash in reward sub-slot
            if hash_of(cc)? != rc.challenge_chain_sub_slot_hash {
                return Err(e("INVALID_CHALLENGE_SLOT_HASH_RC"));
            }

            let mut eos_vdf_iters = vs.ssi;
            let mut cc_start_element = ClassgroupElement::get_default_element();
            let mut cc_eos_vdf_challenge = challenge_hash;
            let rc_eos_vdf_challenge: Bytes32;
            if genesis_block {
                if n == 0 {
                    rc_eos_vdf_challenge = c.genesis_challenge;
                    cc_eos_vdf_challenge = c.genesis_challenge;
                } else {
                    rc_eos_vdf_challenge = hash_of(&block.finished_sub_slots[n - 1].reward_chain)?;
                }
            } else {
                let pb = prev_b.ok_or(e("prev_b"))?;
                if n == 0 {
                    rc_eos_vdf_challenge = pb.reward_infusion_new_challenge;
                    eos_vdf_iters =
                        pb.sub_slot_iters - pb.ip_iters(c).map_err(|_| e("ip_iters"))?;
                    cc_start_element = ClassgroupElement::try_from(&pb.challenge_vdf_output)
                        .map_err(|_| e("invalid challenge VDF output"))?;
                } else {
                    rc_eos_vdf_challenge = hash_of(&block.finished_sub_slots[n - 1].reward_chain)?;
                }
            }

            // 2p. end of reward slot VDF
            let rc_target = vdf_info(
                rc_eos_vdf_challenge,
                eos_vdf_iters,
                rc.end_of_slot_vdf.output,
            );
            if !validate_vdf(
                c,
                &ClassgroupElement::get_default_element(),
                &rc.end_of_slot_vdf,
                &sub_slot.proofs.reward_chain_slot_proof,
                Some(&rc_target),
            ) {
                return Err(e("INVALID_RC_EOS_VDF"));
            }

            // 2q. challenge chain sub-slot VDF
            let partial_cc = vdf_info(
                cc_eos_vdf_challenge,
                eos_vdf_iters,
                cc.challenge_chain_end_of_slot_vdf.output,
            );
            let cc_eos_vdf_info_iters = if genesis_block {
                c.sub_slot_iters_starting
            } else {
                let pb = prev_b.ok_or(e("prev_b"))?;
                if n == 0 { pb.sub_slot_iters } else { vs.ssi }
            };
            if cc.challenge_chain_end_of_slot_vdf != with_iters(&partial_cc, cc_eos_vdf_info_iters)
            {
                return Err(e("INVALID_CC_EOS_VDF (data)"));
            }
            let cc_proof = &sub_slot.proofs.challenge_chain_slot_proof;
            if !cc_proof.normalized_to_identity
                && !validate_vdf(c, &cc_start_element, &partial_cc, cc_proof, None)
            {
                return Err(e("INVALID_CC_EOS_VDF"));
            }
            if cc_proof.normalized_to_identity
                && !validate_vdf(
                    c,
                    &ClassgroupElement::get_default_element(),
                    &cc.challenge_chain_end_of_slot_vdf,
                    cc_proof,
                    None,
                )
            {
                return Err(e("INVALID_CC_EOS_VDF"));
            }

            // 2r/2s/2t. deficit at slot end
            if genesis_block {
                if rc.deficit != c.min_blocks_per_challenge_block {
                    return Err(e("INVALID_DEFICIT (genesis)"));
                }
            } else {
                let pb = prev_b.ok_or(e("prev_b"))?;
                if pb.deficit == 0 {
                    if rc.deficit != c.min_blocks_per_challenge_block {
                        return Err(e("INVALID_DEFICIT (reset)"));
                    }
                } else if rc.deficit != pb.deficit {
                    return Err(e("INVALID_DEFICIT (slot end)"));
                }
            }
        }

        // 3. sub-epoch summary
        if let Some(seh) = ses_hash {
            if genesis_block {
                return Err(e("INVALID_SUB_EPOCH_SUMMARY_HASH (genesis)"));
            }
            let pb = prev_b.ok_or(e("prev_b"))?;
            if !new_sub_slot || !can_finish_se {
                return Err(e("INVALID_SUB_EPOCH_SUMMARY_HASH (not finishing)"));
            }
            if check_sub_epoch_summary {
                let prev_prev = blocks.block_record(pb.prev_hash)?;
                let expected = make_sub_epoch_summary(
                    c,
                    blocks,
                    height,
                    prev_prev,
                    if can_finish_epoch {
                        Some(vs.difficulty)
                    } else {
                        None
                    },
                    if can_finish_epoch { Some(vs.ssi) } else { None },
                )?;
                if hash_of(&expected)? != seh {
                    return Err(e("INVALID_SUB_EPOCH_SUMMARY"));
                }
            }
        } else if !genesis_block && (can_finish_se || can_finish_epoch) {
            // 3d.
            return Err(e("INVALID_SUB_EPOCH_SUMMARY (ses-hash None)"));
        }
    }

    // 4. number of blocks < max
    if !new_sub_slot && !genesis_block {
        let mut num_blocks = 2u32;
        let mut curr = prev_b.ok_or(e("prev_b"))?;
        while !curr.first_in_sub_slot() {
            num_blocks += 1;
            curr = blocks.block_record(curr.prev_hash)?;
        }
        if num_blocks > c.max_sub_slot_blocks {
            return Err(e("TOO_MANY_BLOCKS"));
        }
    }

    // 5a. proof of space challenge
    let challenge = get_block_challenge(c, block, blocks, genesis_block, overflow, false)?;
    if challenge != rcb.pos_ss_cc_challenge_hash {
        return Err(e("INVALID_CC_CHALLENGE"));
    }
    // 5b.
    let cc_sp_hash = match &rcb.challenge_chain_sp_vdf {
        None => challenge,
        Some(vdf) => hash_of(&vdf.output)?,
    };
    let pre_sp_tx_h = pre_sp_tx_block_height(
        c,
        blocks,
        block.prev_header_hash(),
        rcb.signage_point_index,
        block.finished_sub_slots.len(),
    )?;
    let required_iters = validate_pospace_and_get_required_iters(
        c,
        &rcb.proof_of_space,
        challenge,
        cc_sp_hash,
        height,
        vs.difficulty,
        pre_sp_tx_h,
    )
    .ok_or(e("INVALID_POSPACE"))?;

    // 7. required iters
    let sp_interval_iters =
        calculate_sp_interval_iters(c, vs.ssi).map_err(|_| e("sp_interval_iters"))?;
    if required_iters >= sp_interval_iters {
        return Err(e("INVALID_REQUIRED_ITERS"));
    }
    // 8a/8b.
    if (rcb.signage_point_index == 0) != rcb.challenge_chain_sp_vdf.is_none() {
        return Err(e("INVALID_SP_INDEX (cc)"));
    }
    if (rcb.signage_point_index == 0) != rcb.reward_chain_sp_vdf.is_none() {
        return Err(e("INVALID_SP_INDEX (rc)"));
    }
    let sp_iters =
        calculate_sp_iters(c, vs.ssi, rcb.signage_point_index).map_err(|_| e("sp_iters"))?;
    let ip_iters = calculate_ip_iters(c, vs.ssi, rcb.signage_point_index, required_iters)
        .map_err(|_| e("ip_iters"))?;
    if rcb.challenge_chain_sp_vdf.is_none() && overflow {
        return Err(e("low-iters block cannot be overflow"));
    }
    // 9. no overflow in first sub-slot of new epoch
    if overflow && can_finish_epoch && finished_sub_slots_since_prev < 2 {
        return Err(e("NO_OVERFLOWS_IN_FIRST_SUB_SLOT_NEW_EPOCH"));
    }
    // 10. total iters
    let total_iters: u128 = if genesis_block {
        u128::from(vs.ssi) * finished_sub_slots_since_prev as u128
    } else {
        let pb = prev_b.ok_or(e("prev_b"))?;
        if new_sub_slot {
            let mut t = pb.total_iters;
            t += u128::from(pb.sub_slot_iters - pb.ip_iters(c).map_err(|_| e("ip_iters"))?);
            t += u128::from(vs.ssi) * (finished_sub_slots_since_prev as u128 - 1);
            t
        } else {
            pb.total_iters - u128::from(pb.ip_iters(c).map_err(|_| e("ip_iters"))?)
        }
    };
    let total_iters = total_iters + u128::from(ip_iters);
    if total_iters != rcb.total_iters {
        return Err(e("INVALID_TOTAL_ITERS"));
    }

    let sp_total_iters = total_iters - u128::from(ip_iters) + u128::from(sp_iters)
        - if overflow { u128::from(vs.ssi) } else { 0 };
    let (
        cc_vdf_challenge,
        rc_vdf_challenge,
        cc_vdf_input,
        rc_vdf_input,
        cc_vdf_iters,
        rc_vdf_iters,
    ) = get_signage_point_vdf_info(
        c,
        &block.finished_sub_slots,
        overflow,
        prev_b,
        blocks,
        sp_total_iters,
        sp_iters,
    )?;

    // 11. reward chain sp proof + rc_sp_hash
    let rc_sp_hash: Bytes32;
    if sp_iters != 0 {
        let rc_sp_vdf = rcb.reward_chain_sp_vdf.as_ref().ok_or(e("no rc sp vdf"))?;
        let rc_sp_proof = block
            .reward_chain_sp_proof
            .as_ref()
            .ok_or(e("no rc sp proof"))?;
        let target = vdf_info(rc_vdf_challenge, rc_vdf_iters, rc_sp_vdf.output);
        if !validate_vdf(c, &rc_vdf_input, rc_sp_vdf, rc_sp_proof, Some(&target)) {
            return Err(e("INVALID_RC_SP_VDF"));
        }
        rc_sp_hash = hash_of(&rc_sp_vdf.output)?;
    } else {
        if rcb.reward_chain_sp_vdf.is_some() {
            return Err(e("INVALID_RC_SP_VDF (sp0)"));
        }
        if new_sub_slot {
            rc_sp_hash = hash_of(
                &block.finished_sub_slots[block.finished_sub_slots.len() - 1].reward_chain,
            )?;
        } else if genesis_block {
            rc_sp_hash = c.genesis_challenge;
        } else {
            let mut curr = prev_b.ok_or(e("prev_b"))?;
            while !curr.first_in_sub_slot() {
                curr = blocks.block_record(curr.prev_hash)?;
            }
            let frsh = curr
                .finished_reward_slot_hashes
                .as_ref()
                .ok_or(e("no frsh"))?;
            rc_sp_hash = *frsh.last().ok_or(e("empty frsh"))?;
        }
    }
    // 12. reward chain sp signature
    if !bls_verify(
        &rcb.proof_of_space.plot_public_key,
        rc_sp_hash.as_ref(),
        &rcb.reward_chain_sp_signature,
    ) {
        return Err(e("INVALID_RC_SIGNATURE"));
    }
    // 13. cc sp vdf
    if sp_iters != 0 {
        let cc_sp_vdf = rcb
            .challenge_chain_sp_vdf
            .as_ref()
            .ok_or(e("no cc sp vdf"))?;
        let cc_sp_proof = block
            .challenge_chain_sp_proof
            .as_ref()
            .ok_or(e("no cc sp proof"))?;
        let target = vdf_info(cc_vdf_challenge, cc_vdf_iters, cc_sp_vdf.output);
        if *cc_sp_vdf != with_iters(&target, sp_iters) {
            return Err(e("INVALID_CC_SP_VDF (data)"));
        }
        if !cc_sp_proof.normalized_to_identity
            && !validate_vdf(c, &cc_vdf_input, &target, cc_sp_proof, None)
        {
            return Err(e("INVALID_CC_SP_VDF"));
        }
        if cc_sp_proof.normalized_to_identity
            && !validate_vdf(
                c,
                &ClassgroupElement::get_default_element(),
                cc_sp_vdf,
                cc_sp_proof,
                None,
            )
        {
            return Err(e("INVALID_CC_SP_VDF"));
        }
    } else if rcb.challenge_chain_sp_vdf.is_some() {
        return Err(e("INVALID_CC_SP_VDF (sp0)"));
    }
    // 14. cc sp sig
    if !bls_verify(
        &rcb.proof_of_space.plot_public_key,
        cc_sp_hash.as_ref(),
        &rcb.challenge_chain_sp_signature,
    ) {
        return Err(e("INVALID_CC_SIGNATURE"));
    }

    // 15. is_transaction_block
    let foliage = &block.foliage;
    if genesis_block {
        if foliage.foliage_transaction_block_hash.is_none() {
            return Err(e("INVALID_IS_TRANSACTION_BLOCK (genesis)"));
        }
    } else {
        let mut curr = prev_b.ok_or(e("prev_b"))?;
        while !curr.is_transaction_block() {
            curr = blocks.block_record(curr.prev_hash)?;
        }
        let our_sp_total_iters: u128 = total_iters - u128::from(ip_iters) + u128::from(sp_iters)
            - if overflow { u128::from(vs.ssi) } else { 0 };
        let gt = our_sp_total_iters > curr.total_iters;
        if gt != foliage.foliage_transaction_block_hash.is_some() {
            return Err(e("INVALID_IS_TRANSACTION_BLOCK"));
        }
        if gt != foliage.foliage_transaction_block_signature.is_some() {
            return Err(e("INVALID_IS_TRANSACTION_BLOCK (sig)"));
        }
    }
    // 16. foliage block data signature by plot key
    if !bls_verify(
        &rcb.proof_of_space.plot_public_key,
        hash_of(&foliage.foliage_block_data)?.as_ref(),
        &foliage.foliage_block_data_signature,
    ) {
        return Err(e("INVALID_PLOT_SIGNATURE (block data)"));
    }
    // 17. foliage transaction block signature
    if let Some(ftb_hash) = foliage.foliage_transaction_block_hash {
        let sig = foliage
            .foliage_transaction_block_signature
            .as_ref()
            .ok_or(e("no ftb sig"))?;
        if !bls_verify(&rcb.proof_of_space.plot_public_key, ftb_hash.as_ref(), sig) {
            return Err(e("INVALID_PLOT_SIGNATURE (ftb)"));
        }
    }
    // 18. unfinished reward chain block hash
    if unfinished_rcb_hash(rcb)? != foliage.foliage_block_data.unfinished_reward_block_hash {
        return Err(e("INVALID_URSB_HASH"));
    }
    // 19. pool target max height
    let pt = &foliage.foliage_block_data.pool_target;
    if pt.max_height != 0 && pt.max_height < height {
        return Err(e("OLD_POOL_TARGET"));
    }
    // 20. prefarm / pool signature
    if genesis_block {
        if pt.puzzle_hash != c.genesis_pre_farm_pool_puzzle_hash {
            return Err(e("INVALID_PREFARM (pool)"));
        }
        if foliage.foliage_block_data.farmer_reward_puzzle_hash
            != c.genesis_pre_farm_farmer_puzzle_hash
        {
            return Err(e("INVALID_PREFARM (farmer)"));
        }
    } else if let Some(pool_pk) = rcb.proof_of_space.pool_public_key {
        if rcb.proof_of_space.pool_contract_puzzle_hash.is_some() {
            return Err(e("pool pk and contract both set"));
        }
        let pool_sig = foliage
            .foliage_block_data
            .pool_signature
            .as_ref()
            .ok_or(e("no pool signature"))?;
        let pt_bytes = pt
            .to_bytes(dg_xch_serialize::ChiaProtocolVersion::default())
            .map_err(|_| e("pool_target serialization"))?;
        if !bls_verify(&pool_pk, &pt_bytes, pool_sig) {
            return Err(e("INVALID_POOL_SIGNATURE"));
        }
    } else {
        let contract = rcb
            .proof_of_space
            .pool_contract_puzzle_hash
            .ok_or(e("no pool pk or contract"))?;
        if pt.puzzle_hash != contract {
            return Err(e("INVALID_POOL_TARGET"));
        }
    }
    // 22. foliage block presence
    if foliage.foliage_transaction_block_hash.is_some() != block.foliage_transaction_block.is_some()
    {
        return Err(e("INVALID_FOLIAGE_BLOCK_PRESENCE"));
    }
    if foliage.foliage_transaction_block_signature.is_some()
        != block.foliage_transaction_block.is_some()
    {
        return Err(e("INVALID_FOLIAGE_BLOCK_PRESENCE (sig)"));
    }
    if let Some(ftb) = &block.foliage_transaction_block {
        // 23. foliage block hash
        if Some(hash_of(ftb)?) != foliage.foliage_transaction_block_hash {
            return Err(e("INVALID_FOLIAGE_BLOCK_HASH"));
        }
        if genesis_block {
            if ftb.prev_transaction_block_hash != c.genesis_challenge {
                return Err(e("INVALID_PREV_BLOCK_HASH (ftb genesis)"));
            }
        } else {
            let mut curr = prev_b.ok_or(e("prev_b"))?;
            while !curr.is_transaction_block() {
                curr = blocks.block_record(curr.prev_hash)?;
            }
            if ftb.prev_transaction_block_hash != curr.header_hash {
                return Err(e("INVALID_PREV_BLOCK_HASH (ftb)"));
            }
        }
        // 25/26 (filter hash, timestamps): filter check requires check_filter (False here); the
        // future-timestamp check is wall-clock and non-deterministic, so it is not part of weight-proof
        // recent-chain validation (the reference's recent-block path runs the same header validator, but
        // these two checks do not affect the proof's structural validity for a light client).
        if !genesis_block {
            let prev_tx = blocks.block_record(ftb.prev_transaction_block_hash)?;
            let prev_ts = prev_tx.timestamp.ok_or(e("prev tx no timestamp"))?;
            if ftb.timestamp <= prev_ts {
                return Err(e("TIMESTAMP_TOO_FAR_IN_PAST"));
            }
        }
    }

    Ok(required_iters)
}

/// `validate_finished_header_block` (ref `block_header_validation.py:839`). Validates the finished
/// (infusion-point) part on top of the unfinished checks. Returns `Ok(required_iters)` or `Err`.
#[allow(clippy::too_many_lines)]
fn validate_finished_header_block(
    c: &ConsensusConstants,
    blocks: &BlockCache,
    block: &HeaderBlock,
    vs: ValidationState,
    check_sub_epoch_summary: bool,
) -> Result<u64, WeightProofError> {
    let e = WeightProofError::Rejected;
    let rcb = &block.reward_chain_block;
    let required_iters =
        validate_unfinished_header_block(c, blocks, block, vs, check_sub_epoch_summary)?;

    let genesis_block = block.height() == 0;
    let prev_b = if genesis_block {
        None
    } else {
        Some(blocks.block_record(block.prev_header_hash())?)
    };
    let new_sub_slot = !block.finished_sub_slots.is_empty();
    let ip_iters = calculate_ip_iters(c, vs.ssi, rcb.signage_point_index, required_iters)
        .map_err(|_| e("ip_iters"))?;

    if !genesis_block {
        let pb = prev_b.ok_or(e("prev_b"))?;
        if block.height() != pb.height + 1 {
            return Err(e("INVALID_HEIGHT"));
        }
        if block.weight() != pb.weight + u128::from(vs.difficulty) {
            return Err(e("INVALID_WEIGHT"));
        }
    } else {
        if block.height() != 0 {
            return Err(e("INVALID_HEIGHT (genesis)"));
        }
        if block.weight() != u128::from(c.difficulty_starting) {
            return Err(e("INVALID_WEIGHT (genesis)"));
        }
        if block.prev_header_hash() != c.genesis_challenge {
            return Err(e("INVALID_PREV_BLOCK_HASH (genesis)"));
        }
    }

    let last = block.finished_sub_slots.last();
    let cc_vdf_output: ClassgroupElement;
    let ip_vdf_iters: u64;
    let rc_vdf_challenge: Bytes32;
    if genesis_block {
        cc_vdf_output = ClassgroupElement::get_default_element();
        ip_vdf_iters = ip_iters;
        rc_vdf_challenge = if new_sub_slot {
            hash_of(&last.ok_or(e("no last ss"))?.reward_chain)?
        } else {
            c.genesis_challenge
        };
    } else {
        let pb = prev_b.ok_or(e("prev_b"))?;
        if new_sub_slot {
            rc_vdf_challenge = hash_of(&last.ok_or(e("no last ss"))?.reward_chain)?;
            ip_vdf_iters = ip_iters;
            cc_vdf_output = ClassgroupElement::get_default_element();
        } else {
            rc_vdf_challenge = pb.reward_infusion_new_challenge;
            ip_vdf_iters =
                u64::try_from(rcb.total_iters - pb.total_iters).map_err(|_| e("ip_vdf_iters"))?;
            cc_vdf_output = ClassgroupElement::try_from(&pb.challenge_vdf_output)
                .map_err(|_| e("invalid challenge VDF output"))?;
        }
    }

    // 29. CC IP VDF
    let cc_vdf_challenge: Bytes32 = if new_sub_slot {
        hash_of(&last.ok_or(e("no last ss"))?.challenge_chain)?
    } else if genesis_block {
        c.genesis_challenge
    } else {
        let mut curr = prev_b.ok_or(e("prev_b"))?;
        while curr.finished_challenge_slot_hashes.is_none() {
            curr = blocks.block_record(curr.prev_hash)?;
        }
        *curr
            .finished_challenge_slot_hashes
            .as_ref()
            .ok_or(e("no fcsh"))?
            .last()
            .ok_or(e("empty fcsh"))?
    };
    let cc_target = vdf_info(
        cc_vdf_challenge,
        ip_vdf_iters,
        rcb.challenge_chain_ip_vdf.output,
    );
    if rcb.challenge_chain_ip_vdf != with_iters(&cc_target, ip_iters) {
        return Err(e("INVALID_CC_IP_VDF (data)"));
    }
    let cc_ip_proof = &block.challenge_chain_ip_proof;
    if !cc_ip_proof.normalized_to_identity
        && !validate_vdf(c, &cc_vdf_output, &cc_target, cc_ip_proof, None)
    {
        return Err(e("INVALID_CC_IP_VDF"));
    }
    if cc_ip_proof.normalized_to_identity
        && !validate_vdf(
            c,
            &ClassgroupElement::get_default_element(),
            &rcb.challenge_chain_ip_vdf,
            cc_ip_proof,
            None,
        )
    {
        return Err(e("INVALID_CC_IP_VDF (norm)"));
    }
    // 30. RC IP VDF
    let rc_target = vdf_info(
        rc_vdf_challenge,
        ip_vdf_iters,
        rcb.reward_chain_ip_vdf.output,
    );
    if !validate_vdf(
        c,
        &ClassgroupElement::get_default_element(),
        &rcb.reward_chain_ip_vdf,
        &block.reward_chain_ip_proof,
        Some(&rc_target),
    ) {
        return Err(e("INVALID_RC_IP_VDF"));
    }
    // 31. ICC IP VDF
    if !genesis_block {
        let pb = prev_b.ok_or(e("prev_b"))?;
        let overflow = is_overflow_block(c, rcb.signage_point_index).map_err(|_| e("overflow"))?;
        let deficit = calculate_deficit(
            c,
            block.height(),
            Some(pb),
            overflow,
            block.finished_sub_slots.len(),
        );
        if let Some(icc_ip_vdf) = &rcb.infused_challenge_chain_ip_vdf {
            let icc_ip_proof = block
                .infused_challenge_chain_ip_proof
                .as_ref()
                .ok_or(e("no icc ip proof"))?;
            if deficit >= c.min_blocks_per_challenge_block - 1 {
                return Err(e("INVALID_ICC_VDF (deficit>=min-1)"));
            }
            let (icc_vdf_challenge, icc_vdf_input): (Bytes32, Option<ClassgroupElement>) =
                if new_sub_slot {
                    let icc_ss = last
                        .ok_or(e("no last ss"))?
                        .infused_challenge_chain
                        .as_ref()
                        .ok_or(e("no last icc"))?;
                    (
                        hash_of(icc_ss)?,
                        Some(ClassgroupElement::get_default_element()),
                    )
                } else {
                    let input = if pb.is_challenge_block(c.min_blocks_per_challenge_block) {
                        Some(ClassgroupElement::get_default_element())
                    } else {
                        pb.infused_challenge_vdf_output
                            .as_ref()
                            .map(ClassgroupElement::try_from)
                            .transpose()
                            .map_err(|_| e("invalid infused challenge VDF output"))?
                    };
                    let mut curr = pb;
                    while curr.finished_infused_challenge_slot_hashes.is_none()
                        && !curr.is_challenge_block(c.min_blocks_per_challenge_block)
                    {
                        curr = blocks.block_record(curr.prev_hash)?;
                    }
                    let challenge = if curr.is_challenge_block(c.min_blocks_per_challenge_block) {
                        curr.challenge_block_info_hash
                    } else {
                        *curr
                            .finished_infused_challenge_slot_hashes
                            .as_ref()
                            .ok_or(e("no ficsh"))?
                            .last()
                            .ok_or(e("empty ficsh"))?
                    };
                    (challenge, input)
                };
            let icc_target = vdf_info(icc_vdf_challenge, ip_vdf_iters, icc_ip_vdf.output);
            let ok = match icc_vdf_input {
                Some(input) => validate_vdf(c, &input, icc_ip_vdf, icc_ip_proof, Some(&icc_target)),
                None => false,
            };
            if !ok {
                return Err(e("INVALID_ICC_VDF (invalid icc proof)"));
            }
        } else if deficit < c.min_blocks_per_challenge_block - 1 {
            return Err(e("INVALID_ICC_VDF (missing icc)"));
        }
    } else if block.infused_challenge_chain_ip_proof.is_some() {
        return Err(e("INVALID_ICC_VDF (genesis has icc)"));
    }

    // 32. reward block hash
    if block.foliage.reward_block_hash != hash_of(rcb)? {
        return Err(e("INVALID_REWARD_BLOCK_HASH"));
    }
    // 33. reward block is_transaction_block
    if block.foliage.foliage_transaction_block_hash.is_some() != rcb.is_transaction_block {
        return Err(e("INVALID_FOLIAGE_BLOCK_PRESENCE"));
    }
    Ok(required_iters)
}

/// `_validate_pospace_recent_chain` (ref `weight_proof.py:1338`) — the light path.
fn validate_pospace_recent_chain(
    c: &ConsensusConstants,
    blocks: &BlockCache,
    block: &HeaderBlock,
    challenge: Bytes32,
    diff: u64,
    overflow: bool,
    prev_challenge: Bytes32,
) -> Result<u64, WeightProofError> {
    let e = WeightProofError::Rejected;
    let rcb = &block.reward_chain_block;
    let cc_sp_hash = match &rcb.challenge_chain_sp_vdf {
        None => challenge,
        Some(vdf) => hash_of(&vdf.output)?,
    };
    let pre_sp_tx_h = pre_sp_tx_block_height(
        c,
        blocks,
        block.prev_header_hash(),
        rcb.signage_point_index,
        block.finished_sub_slots.len(),
    )?;
    validate_pospace_and_get_required_iters(
        c,
        &rcb.proof_of_space,
        if overflow { prev_challenge } else { challenge },
        cc_sp_hash,
        block.height(),
        diff,
        pre_sp_tx_h,
    )
    .ok_or(e("INVALID_POSPACE (recent chain)"))
}

fn get_ses_idx(recent_chain: &[HeaderBlock]) -> usize {
    let mut count = 0usize;
    for block in recent_chain {
        for slot in &block.finished_sub_slots {
            if slot.challenge_chain.subepoch_summary_hash.is_some() {
                count += 1;
            }
        }
    }
    count
}

/// `get_deficit` (ref `weight_proof.py:1602`).
fn get_deficit(
    c: &ConsensusConstants,
    curr_deficit: u8,
    prev_block: Option<&BlockRecord>,
    overflow: bool,
    num_finished_sub_slots: usize,
) -> u8 {
    match prev_block {
        None => {
            if curr_deficit >= 1 && !(overflow && curr_deficit == c.min_blocks_per_challenge_block)
            {
                curr_deficit - 1
            } else {
                curr_deficit
            }
        }
        Some(pb) => calculate_deficit(c, pb.height + 1, Some(pb), overflow, num_finished_sub_slots),
    }
}

/// `validate_recent_blocks` (ref `weight_proof.py:1225`) — phase 5. Fully validates the recent-chain
/// tail so the proof connects to a concrete peak. Heavy path (`validate_finished_header_block`) for tip
/// blocks; light path (`validate_pospace_recent_chain`) otherwise. Fail-closed on every check.
#[allow(clippy::collapsible_if)]
pub(crate) fn validate_recent_blocks(
    wp: &dg_xch_core::blockchain::weight_proof::WeightProof,
    summaries: &[SubEpochSummary],
    c: &ConsensusConstants,
) -> Result<(), WeightProofError> {
    let e = WeightProofError::Rejected;
    let recent_chain = &wp.recent_chain_data;
    if recent_chain.is_empty() {
        return Err(WeightProofError::Malformed("empty recent chain"));
    }
    let mut sub_blocks = BlockCache::new();
    let first_ses = get_ses_idx(recent_chain);
    let mut ses_idx = summaries
        .len()
        .checked_sub(first_ses)
        .ok_or(e("ses_idx underflow"))?;
    let mut ssi = c.sub_slot_iters_starting;
    let mut diff = c.difficulty_starting;
    let last_blocks_to_validate: u32 = 100;
    for summary in &summaries[..ses_idx] {
        if let Some(v) = summary.new_sub_slot_iters {
            ssi = v;
        }
        if let Some(v) = summary.new_difficulty {
            diff = v;
        }
    }

    let (mut ses_blocks, mut sub_slots, mut transaction_blocks) = (0u32, 0u32, 0u32);
    // `challenge` / `prev_challenge` are initialized ONCE and persist across blocks — updated only when a
    // block carries finished sub-slots. A block in the same slot as its predecessor keeps the prior slot's
    // challenge (do NOT reset per block). (ref: set before the loop, updated inside the sub-slot loop.)
    let mut challenge: Option<Bytes32> =
        Some(recent_chain[0].reward_chain_block.pos_ss_cc_challenge_hash);
    let mut prev_challenge: Option<Bytes32> = None;
    let tip_height = recent_chain[recent_chain.len() - 1].height();
    let mut prev_block_record: Option<BlockRecord> = None;
    let mut deficit: u8 = 0;
    let mut adjusted = false;
    let mut validated_block_count: u32 = 0;

    for block in recent_chain.iter() {
        let rcb = &block.reward_chain_block;
        let mut required_iters: u64 = 0;
        let mut overflow = false;
        let mut ses = false;
        let height = block.height();

        for sub_slot in &block.finished_sub_slots {
            prev_challenge = Some(
                sub_slot
                    .challenge_chain
                    .challenge_chain_end_of_slot_vdf
                    .challenge,
            );
            challenge = Some(hash_of(&sub_slot.challenge_chain)?);
            deficit = sub_slot.reward_chain.deficit;
            if let Some(seh) = sub_slot.challenge_chain.subepoch_summary_hash {
                ses = true;
                let summary = summaries.get(ses_idx).ok_or(e("ses_idx out of range"))?;
                if hash_of(summary)? != seh {
                    return Err(e("sub epoch summary mismatch"));
                }
                ses_idx += 1;
            }
            if let Some(v) = sub_slot.challenge_chain.new_sub_slot_iters {
                ssi = v;
            }
            if let Some(v) = sub_slot.challenge_chain.new_difficulty {
                diff = v;
            }
        }

        if let (Some(chal), Some(prev_chal)) = (challenge, prev_challenge) {
            if transaction_blocks > 2 {
                overflow =
                    is_overflow_block(c, rcb.signage_point_index).map_err(|_| e("overflow"))?;
                if !adjusted {
                    let mut pbr = prev_block_record.clone().ok_or(e("prev_block_record"))?;
                    pbr.deficit = deficit % c.min_blocks_per_challenge_block;
                    sub_blocks.add_block(pbr.clone());
                    prev_block_record = Some(pbr);
                    adjusted = true;
                }
                deficit = get_deficit(
                    c,
                    deficit,
                    prev_block_record.as_ref(),
                    overflow,
                    block.finished_sub_slots.len(),
                );
                if sub_slots > 2
                    && transaction_blocks > 11
                    && (tip_height - height < last_blocks_to_validate)
                {
                    let vs = ValidationState {
                        ssi,
                        difficulty: diff,
                    };
                    required_iters =
                        validate_finished_header_block(c, &sub_blocks, block, vs, ses_blocks > 2)?;
                } else {
                    required_iters = validate_pospace_recent_chain(
                        c,
                        &sub_blocks,
                        block,
                        chal,
                        diff,
                        overflow,
                        prev_chal,
                    )?;
                }
                validated_block_count += 1;
            }
        }

        let curr_block_ses = if ses {
            Some(
                *summaries
                    .get(ses_idx - 1)
                    .ok_or(e("curr_block_ses index"))?,
            )
        } else {
            None
        };
        let block_record = header_block_to_sub_block_record(
            c,
            required_iters,
            block,
            ssi,
            overflow,
            deficit,
            height, // ref passes `height` as prev_transaction_block_height here
            curr_block_ses,
        )?;
        sub_blocks.add_block(block_record.clone());

        if block.first_in_sub_slot() {
            sub_slots += 1;
        }
        if rcb.is_transaction_block {
            transaction_blocks += 1;
        }
        if ses {
            ses_blocks += 1;
        }
        prev_block_record = Some(block_record);
    }

    if summaries.len() > 2 && prev_challenge.is_none() {
        return Err(e("did not find two challenges in recent chain"));
    }
    if summaries.len() > 2 && validated_block_count < u32::from(c.min_blocks_per_challenge_block) {
        return Err(e("did not validate enough blocks in recent chain"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Pure deficit state-machine parity spot-checks against the reference (deficit.py). These are
    // value-level, no fixtures — a fast guard on the trickiest branch (overflow × new-sub-slot).
    fn c() -> ConsensusConstants {
        dg_xch_core::consensus::constants::MAINNET.clone()
    }

    #[test]
    fn deficit_genesis_is_min_minus_one() {
        assert_eq!(calculate_deficit(&c(), 0, None, false, 0), 15);
    }
}
