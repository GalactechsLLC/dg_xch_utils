// Header-block validation: unfinished and finished header checks.
// VDF (dg_xch_vdf) and proof-of-space (dg_xch_pos) verification is injected via HeaderValidationVerifier
// to avoid a dependency cycle (both crates depend on dg_xch_core).

use crate::blockchain::block_record::BlockRecord;
use crate::blockchain::class_group_element::ClassgroupElement;
use crate::blockchain::header_block::HeaderBlock;
use crate::blockchain::proof_of_space::{ProofOfSpace, is_proof_version_active, is_v1_phased_out};
use crate::blockchain::sized_bytes::{Bytes32, Bytes48, Bytes96};
use crate::blockchain::vdf_info::VdfInfo;
use crate::blockchain::vdf_proof::VdfProof;
use crate::clvm::bls_bindings::verify_signature;
use crate::consensus::constants::ConsensusConstants;
use crate::consensus::deficit::calculate_deficit;
use crate::consensus::difficulty_adjustment::can_finish_sub_and_full_epoch;
use crate::consensus::get_block_challenge::{get_block_challenge, pre_sp_tx_block_height};
use crate::consensus::make_sub_epoch_summary::make_sub_epoch_summary;
use crate::consensus::pot_iterations::{
    calculate_ip_iters, calculate_iterations_quality_for_proof, calculate_sp_interval_iters,
    calculate_sp_iters, is_overflow_block,
};
use crate::consensus::vdf_info_computation::get_signage_point_vdf_info;
use crate::consensus::{missing, rejected};
use blst::min_pk::{PublicKey, Signature};
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use std::collections::HashMap;
use std::io::Error;

// Verifier seam so dg_xch_core need not depend on dg_xch_vdf / dg_xch_pos.
// Which of the five finished-header BLS signature gates a `verify_bls_sig` call is, so the window
// pipeline's deferred drain can reproduce the exact rejection string for the failing block
// instead of a generic "bad sig" (the VDF drain loses its sub-error; this one does not).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HeaderSigTag {
    RewardChainSp,
    ChallengeChainSp,
    FoliageBlockData,
    FoliageTransactionBlock,
    Pool,
}

impl HeaderSigTag {
    // The byte-identical rejection string the inline site would have produced.
    #[must_use]
    pub fn rejection(self) -> &'static str {
        match self {
            Self::RewardChainSp => "INVALID_RC_SIGNATURE",
            Self::ChallengeChainSp => "INVALID_CC_SIGNATURE",
            Self::FoliageBlockData => "INVALID_PLOT_SIGNATURE (block data)",
            Self::FoliageTransactionBlock => "INVALID_PLOT_SIGNATURE (ftb)",
            Self::Pool => "INVALID_POOL_SIGNATURE",
        }
    }
}

pub trait HeaderValidationVerifier {
    // VDF validation (basic, non normalization-aware form).
    fn validate_vdf(
        &self,
        constants: &ConsensusConstants,
        input: &ClassgroupElement,
        info: &VdfInfo,
        proof: &VdfProof,
        target: Option<&VdfInfo>,
    ) -> bool;

    // Proof-of-space quality-string verification.
    fn pospace_quality_string(
        &self,
        constants: &ConsensusConstants,
        proof_of_space: &ProofOfSpace,
        challenge: Bytes32,
        cc_sp_hash: Bytes32,
        height: u32,
    ) -> Option<Bytes32>;

    fn verify_bls_sig(&self, pk: &Bytes48, msg: &[u8], sig: &Bytes96, _tag: HeaderSigTag) -> bool {
        bls_verify(pk, msg, sig)
    }
}

impl<T: HeaderValidationVerifier + ?Sized> HeaderValidationVerifier for &T {
    fn validate_vdf(
        &self,
        constants: &ConsensusConstants,
        input: &ClassgroupElement,
        info: &VdfInfo,
        proof: &VdfProof,
        target: Option<&VdfInfo>,
    ) -> bool {
        (*self).validate_vdf(constants, input, info, proof, target)
    }

    fn pospace_quality_string(
        &self,
        constants: &ConsensusConstants,
        proof_of_space: &ProofOfSpace,
        challenge: Bytes32,
        cc_sp_hash: Bytes32,
        height: u32,
    ) -> Option<Bytes32> {
        (*self).pospace_quality_string(constants, proof_of_space, challenge, cc_sp_hash, height)
    }

    fn verify_bls_sig(&self, pk: &Bytes48, msg: &[u8], sig: &Bytes96, tag: HeaderSigTag) -> bool {
        (*self).verify_bls_sig(pk, msg, sig, tag)
    }
}

// Validation state; prev_ses_block omitted (general-node path is always prev_ses_block=None).
#[derive(Clone, Copy)]
pub struct ValidationState {
    pub ssi: u64,
    pub difficulty: u64,
}

#[must_use]
pub fn bls_verify(pk: &Bytes48, msg: &[u8], sig: &Bytes96) -> bool {
    match Signature::try_from(sig) {
        Ok(s) => verify_signature(&PublicKey::from(pk), msg, &s),
        Err(_) => false,
    }
}

// Validate the proof of space and derive required iters. Ok(None) on invalid proof of space.
#[allow(clippy::unnecessary_wraps, clippy::too_many_arguments)]
pub fn validate_pospace_and_get_required_iters(
    verifier: &impl HeaderValidationVerifier,
    constants: &ConsensusConstants,
    proof_of_space: &ProofOfSpace,
    challenge: Bytes32,
    cc_sp_hash: Bytes32,
    height: u32,
    difficulty: u64,
    prev_transaction_block_height: u32,
) -> Result<Option<u64>, Error> {
    if !is_proof_version_active(
        proof_of_space.version,
        prev_transaction_block_height,
        constants,
    ) {
        return Ok(None);
    }
    // A v1 proof past the phase-out window is no longer a valid proof of space.
    if proof_of_space.version == 0
        && is_v1_phased_out(
            proof_of_space.proof.as_ref(),
            prev_transaction_block_height,
            constants,
        )
    {
        return Ok(None);
    }
    let Some(q_str) =
        verifier.pospace_quality_string(constants, proof_of_space, challenge, cc_sp_hash, height)
    else {
        return Ok(None);
    };
    Ok(Some(calculate_iterations_quality_for_proof(
        constants,
        proof_of_space,
        q_str,
        difficulty,
        cc_sp_hash,
    )))
}

// The pre-infusion half of a block — every field an UnfinishedBlock already carries. Both the
// finished and unfinished validators run the same checks over this view, so the two paths can
// never drift.
pub struct UnfinishedParts<'a> {
    pub finished_sub_slots: &'a [crate::blockchain::subslot_bundle::SubSlotBundle],
    pub reward_chain_block:
        &'a crate::blockchain::reward_chain_block_unfinished::RewardChainBlockUnfinished,
    pub challenge_chain_sp_proof: &'a Option<VdfProof>,
    pub reward_chain_sp_proof: &'a Option<VdfProof>,
    pub foliage: &'a crate::blockchain::foliage::Foliage,
    pub foliage_transaction_block:
        &'a Option<crate::blockchain::foliage_transaction_block::FoliageTransactionBlock>,
}

impl UnfinishedParts<'_> {
    fn prev_header_hash(&self) -> Bytes32 {
        self.foliage.prev_block_hash
    }
}

/// Validate an unfinished header block — everything EXCEPT the infusion-point VDFs:
/// finished sub-slots, signage-point VDFs, proof of space,
/// foliage signatures/bindings, and the pre-infusion difficulty context. The
/// pre-infusion pipeline validates gossiped unfinished blocks through this before caching.
///
/// # Errors
/// Fail-closed: any violation is `Err` with the rejection name.
pub fn validate_unfinished_header_block(
    constants: &ConsensusConstants,
    verifier: &impl HeaderValidationVerifier,
    blocks: &HashMap<Bytes32, BlockRecord>,
    block: &crate::blockchain::unfinished_header_block::UnfinishedHeaderBlock,
    vs: ValidationState,
    check_sub_epoch_summary: bool,
) -> Result<u64, Error> {
    validate_unfinished_parts(
        constants,
        verifier,
        blocks,
        &UnfinishedParts {
            finished_sub_slots: &block.finished_sub_slots,
            reward_chain_block: &block.reward_chain_block,
            challenge_chain_sp_proof: &block.challenge_chain_sp_proof,
            reward_chain_sp_proof: &block.reward_chain_sp_proof,
            foliage: &block.foliage,
            foliage_transaction_block: &block.foliage_transaction_block,
        },
        vs,
        check_sub_epoch_summary,
    )
}

// Recent-chain specialization (skip_overflow_last_ss_validation=false,
// skip_vdf_is_valid=false). Fail-closed: any violation is Err.
#[allow(clippy::too_many_lines)]
pub fn validate_unfinished_parts(
    constants: &ConsensusConstants,
    verifier: &impl HeaderValidationVerifier,
    blocks: &HashMap<Bytes32, BlockRecord>,
    block: &UnfinishedParts<'_>,
    vs: ValidationState,
    check_sub_epoch_summary: bool,
) -> Result<u64, Error> {
    let c = constants;
    let rcb = &block.reward_chain_block;
    let e = rejected;

    // 6. check signage point index
    if u32::from(rcb.signage_point_index) >= c.num_sps_sub_slot {
        return Err(e("INVALID_SP_INDEX"));
    }
    // 1. previous block / genesis
    let prev_b = blocks.get(&block.prev_header_hash());
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
        height = pb.height.checked_add(1).ok_or(e("INVALID_HEIGHT"))?;
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
                        curr = blocks
                            .get(&curr.prev_hash)
                            .ok_or_else(|| missing(curr.prev_hash))?;
                    }
                    let fcsh = curr
                        .finished_challenge_slot_hashes
                        .as_ref()
                        .ok_or(e("no fcsh"))?;
                    if *fcsh.last().ok_or(e("empty fcsh"))? != challenge_hash {
                        return Err(e("INVALID_PREV_CHALLENGE_SLOT_HASH"));
                    }
                }
            } else if block.finished_sub_slots[n - 1].challenge_chain.hash()? != challenge_hash {
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
                            curr = blocks
                                .get(&curr.prev_hash)
                                .ok_or_else(|| missing(curr.prev_hash))?;
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
                            icc_vdf_input = pb.infused_challenge_vdf_output;
                        }
                    } else if block.finished_sub_slots[n - 1].reward_chain.deficit
                        < c.min_blocks_per_challenge_block
                    {
                        let finished_ss = &block.finished_sub_slots[n - 1];
                        let icc_ss = finished_ss
                            .infused_challenge_chain
                            .as_ref()
                            .ok_or(e("no prev icc"))?;
                        icc_challenge_hash = Some(icc_ss.hash()?);
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
                    let target = VdfInfo::new(icc_challenge_hash, icc_iters_proof, icc_eos.output);
                    if *icc_eos != target.with_iters(icc_iters_committed) {
                        return Err(e("INVALID_ICC_EOS_VDF"));
                    }
                    if !icc_proof.normalized_to_identity
                        && !verifier.validate_vdf(c, &icc_vdf_input, &target, icc_proof, None)
                    {
                        return Err(e("INVALID_ICC_EOS_VDF"));
                    }
                    if icc_proof.normalized_to_identity
                        && !verifier.validate_vdf(
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
                        if Some(icc.hash()?) != cc.infused_challenge_chain_sub_slot_hash {
                            return Err(e("INVALID_ICC_HASH_CC"));
                        }
                    } else if cc.infused_challenge_chain_sub_slot_hash.is_some() {
                        // 2h.
                        return Err(e("INVALID_ICC_HASH_CC"));
                    }
                    // 2i. icc hash in reward sub-slot
                    if Some(icc.hash()?) != rc.infused_challenge_chain_sub_slot_hash {
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
            if cc.hash()? != rc.challenge_chain_sub_slot_hash {
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
                    rc_eos_vdf_challenge = block.finished_sub_slots[n - 1].reward_chain.hash()?;
                }
            } else {
                let pb = prev_b.ok_or(e("prev_b"))?;
                if n == 0 {
                    rc_eos_vdf_challenge = pb.reward_infusion_new_challenge;
                    eos_vdf_iters =
                        pb.sub_slot_iters - pb.ip_iters(c).map_err(|_| e("ip_iters"))?;
                    cc_start_element = pb.challenge_vdf_output;
                } else {
                    rc_eos_vdf_challenge = block.finished_sub_slots[n - 1].reward_chain.hash()?;
                }
            }

            // 2p. end of reward slot VDF
            let rc_target = VdfInfo::new(
                rc_eos_vdf_challenge,
                eos_vdf_iters,
                rc.end_of_slot_vdf.output,
            );
            if !verifier.validate_vdf(
                c,
                &ClassgroupElement::get_default_element(),
                &rc.end_of_slot_vdf,
                &sub_slot.proofs.reward_chain_slot_proof,
                Some(&rc_target),
            ) {
                return Err(e("INVALID_RC_EOS_VDF"));
            }

            // 2q. challenge chain sub-slot VDF
            let partial_cc = VdfInfo::new(
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
            if cc.challenge_chain_end_of_slot_vdf != partial_cc.with_iters(cc_eos_vdf_info_iters) {
                return Err(e("INVALID_CC_EOS_VDF (data)"));
            }
            let cc_proof = &sub_slot.proofs.challenge_chain_slot_proof;
            if !cc_proof.normalized_to_identity
                && !verifier.validate_vdf(c, &cc_start_element, &partial_cc, cc_proof, None)
            {
                return Err(e("INVALID_CC_EOS_VDF"));
            }
            if cc_proof.normalized_to_identity
                && !verifier.validate_vdf(
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
                let prev_prev = blocks
                    .get(&pb.prev_hash)
                    .ok_or_else(|| missing(pb.prev_hash))?;
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
                )
                .map_err(|_| e("make_sub_epoch_summary"))?;
                if expected.hash()? != seh {
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
            curr = blocks
                .get(&curr.prev_hash)
                .ok_or_else(|| missing(curr.prev_hash))?;
        }
        if num_blocks > c.max_sub_slot_blocks {
            return Err(e("TOO_MANY_BLOCKS"));
        }
    }

    // 5a. proof of space challenge
    let challenge = get_block_challenge(
        c,
        block.finished_sub_slots,
        block.prev_header_hash(),
        blocks,
        genesis_block,
        overflow,
        false,
    )?;
    if challenge != rcb.pos_ss_cc_challenge_hash {
        return Err(e("INVALID_CC_CHALLENGE"));
    }
    // 5b.
    let cc_sp_hash = match &rcb.challenge_chain_sp_vdf {
        None => challenge,
        Some(vdf) => vdf.output.hash()?,
    };
    let pre_sp_tx_h = pre_sp_tx_block_height(
        c,
        blocks,
        block.prev_header_hash(),
        rcb.signage_point_index,
        block.finished_sub_slots.len(),
    )?;
    let required_iters = validate_pospace_and_get_required_iters(
        verifier,
        c,
        &rcb.proof_of_space,
        challenge,
        cc_sp_hash,
        height,
        vs.difficulty,
        pre_sp_tx_h,
    )?
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
        block.finished_sub_slots,
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
        let target = VdfInfo::new(rc_vdf_challenge, rc_vdf_iters, rc_sp_vdf.output);
        if !verifier.validate_vdf(c, &rc_vdf_input, rc_sp_vdf, rc_sp_proof, Some(&target)) {
            return Err(e("INVALID_RC_SP_VDF"));
        }
        rc_sp_hash = rc_sp_vdf.output.hash()?;
    } else {
        if rcb.reward_chain_sp_vdf.is_some() {
            return Err(e("INVALID_RC_SP_VDF (sp0)"));
        }
        if new_sub_slot {
            rc_sp_hash = block.finished_sub_slots[block.finished_sub_slots.len() - 1]
                .reward_chain
                .hash()?;
        } else if genesis_block {
            rc_sp_hash = c.genesis_challenge;
        } else {
            let mut curr = prev_b.ok_or(e("prev_b"))?;
            while !curr.first_in_sub_slot() {
                curr = blocks
                    .get(&curr.prev_hash)
                    .ok_or_else(|| missing(curr.prev_hash))?;
            }
            let frsh = curr
                .finished_reward_slot_hashes
                .as_ref()
                .ok_or(e("no frsh"))?;
            rc_sp_hash = *frsh.last().ok_or(e("empty frsh"))?;
        }
    }
    // 12. reward chain sp signature
    if !verifier.verify_bls_sig(
        &rcb.proof_of_space.plot_public_key,
        rc_sp_hash.as_ref(),
        &rcb.reward_chain_sp_signature,
        HeaderSigTag::RewardChainSp,
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
        let target = VdfInfo::new(cc_vdf_challenge, cc_vdf_iters, cc_sp_vdf.output);
        if *cc_sp_vdf != target.with_iters(sp_iters) {
            return Err(e("INVALID_CC_SP_VDF (data)"));
        }
        if !cc_sp_proof.normalized_to_identity
            && !verifier.validate_vdf(c, &cc_vdf_input, &target, cc_sp_proof, None)
        {
            return Err(e("INVALID_CC_SP_VDF"));
        }
        if cc_sp_proof.normalized_to_identity
            && !verifier.validate_vdf(
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
    if !verifier.verify_bls_sig(
        &rcb.proof_of_space.plot_public_key,
        cc_sp_hash.as_ref(),
        &rcb.challenge_chain_sp_signature,
        HeaderSigTag::ChallengeChainSp,
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
            curr = blocks
                .get(&curr.prev_hash)
                .ok_or_else(|| missing(curr.prev_hash))?;
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
    if !verifier.verify_bls_sig(
        &rcb.proof_of_space.plot_public_key,
        foliage.foliage_block_data.hash()?.as_ref(),
        &foliage.foliage_block_data_signature,
        HeaderSigTag::FoliageBlockData,
    ) {
        return Err(e("INVALID_PLOT_SIGNATURE (block data)"));
    }
    // 17. foliage transaction block signature
    if let Some(ftb_hash) = foliage.foliage_transaction_block_hash {
        let sig = foliage
            .foliage_transaction_block_signature
            .as_ref()
            .ok_or(e("no ftb sig"))?;
        if !verifier.verify_bls_sig(
            &rcb.proof_of_space.plot_public_key,
            ftb_hash.as_ref(),
            sig,
            HeaderSigTag::FoliageTransactionBlock,
        ) {
            return Err(e("INVALID_PLOT_SIGNATURE (ftb)"));
        }
    }
    // 18. unfinished reward chain block hash
    if rcb.hash()? != foliage.foliage_block_data.unfinished_reward_block_hash {
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
            .to_bytes(ChiaProtocolVersion::default())
            .map_err(|_| e("pool_target serialization"))?;
        if !verifier.verify_bls_sig(&pool_pk, &pt_bytes, pool_sig, HeaderSigTag::Pool) {
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
        if Some(ftb.hash()?) != foliage.foliage_transaction_block_hash {
            return Err(e("INVALID_FOLIAGE_BLOCK_HASH"));
        }
        if genesis_block {
            if ftb.prev_transaction_block_hash != c.genesis_challenge {
                return Err(e("INVALID_PREV_BLOCK_HASH (ftb genesis)"));
            }
        } else {
            let mut curr = prev_b.ok_or(e("prev_b"))?;
            while !curr.is_transaction_block() {
                curr = blocks
                    .get(&curr.prev_hash)
                    .ok_or_else(|| missing(curr.prev_hash))?;
            }
            if ftb.prev_transaction_block_hash != curr.header_hash {
                return Err(e("INVALID_PREV_BLOCK_HASH (ftb)"));
            }
        }
        // 25/26 (filter hash, timestamps): filter check requires check_filter (False here); the
        // future-timestamp check is wall-clock and non-deterministic, so it is not part of
        // recent-chain header validation — neither affects the proof's structural validity
        // for a light client.
        if !genesis_block {
            let prev_tx = blocks
                .get(&ftb.prev_transaction_block_hash)
                .ok_or_else(|| missing(ftb.prev_transaction_block_hash))?;
            let prev_ts = prev_tx.timestamp.ok_or(e("prev tx no timestamp"))?;
            if ftb.timestamp <= prev_ts {
                return Err(e("TIMESTAMP_TOO_FAR_IN_PAST"));
            }
        }
    }

    Ok(required_iters)
}

// Infusion-point checks on top of the unfinished ones.
#[allow(clippy::too_many_lines)]
pub fn validate_finished_header_block(
    constants: &ConsensusConstants,
    verifier: &impl HeaderValidationVerifier,
    blocks: &HashMap<Bytes32, BlockRecord>,
    block: &HeaderBlock,
    vs: ValidationState,
    check_sub_epoch_summary: bool,
) -> Result<u64, Error> {
    let c = constants;
    let e = rejected;
    let rcb = &block.reward_chain_block;
    let unfinished_rcb = block.reward_chain_block.get_unfinished();
    let required_iters = validate_unfinished_parts(
        c,
        verifier,
        blocks,
        &UnfinishedParts {
            finished_sub_slots: &block.finished_sub_slots,
            reward_chain_block: &unfinished_rcb,
            challenge_chain_sp_proof: &block.challenge_chain_sp_proof,
            reward_chain_sp_proof: &block.reward_chain_sp_proof,
            foliage: &block.foliage,
            foliage_transaction_block: &block.foliage_transaction_block,
        },
        vs,
        check_sub_epoch_summary,
    )?;

    let genesis_block = block.height() == 0;
    let prev_b = if genesis_block {
        None
    } else {
        Some(
            blocks
                .get(&block.prev_header_hash())
                .ok_or_else(|| missing(block.prev_header_hash()))?,
        )
    };
    let new_sub_slot = !block.finished_sub_slots.is_empty();
    let ip_iters = calculate_ip_iters(c, vs.ssi, rcb.signage_point_index, required_iters)
        .map_err(|_| e("ip_iters"))?;

    if !genesis_block {
        let pb = prev_b.ok_or(e("prev_b"))?;
        if pb.height.checked_add(1) != Some(block.height()) {
            return Err(e("INVALID_HEIGHT"));
        }
        if pb.weight.checked_add(u128::from(vs.difficulty)) != Some(block.weight()) {
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
            last.ok_or(e("no last ss"))?.reward_chain.hash()?
        } else {
            c.genesis_challenge
        };
    } else {
        let pb = prev_b.ok_or(e("prev_b"))?;
        if new_sub_slot {
            rc_vdf_challenge = last.ok_or(e("no last ss"))?.reward_chain.hash()?;
            ip_vdf_iters = ip_iters;
            cc_vdf_output = ClassgroupElement::get_default_element();
        } else {
            rc_vdf_challenge = pb.reward_infusion_new_challenge;
            ip_vdf_iters = u64::try_from(
                rcb.total_iters
                    .checked_sub(pb.total_iters)
                    .ok_or(e("ip_vdf_iters"))?,
            )
            .map_err(|_| e("ip_vdf_iters"))?;
            cc_vdf_output = pb.challenge_vdf_output;
        }
    }

    // 29. CC IP VDF
    let cc_vdf_challenge: Bytes32 = if new_sub_slot {
        last.ok_or(e("no last ss"))?.challenge_chain.hash()?
    } else if genesis_block {
        c.genesis_challenge
    } else {
        let mut curr = prev_b.ok_or(e("prev_b"))?;
        while curr.finished_challenge_slot_hashes.is_none() {
            curr = blocks
                .get(&curr.prev_hash)
                .ok_or_else(|| missing(curr.prev_hash))?;
        }
        *curr
            .finished_challenge_slot_hashes
            .as_ref()
            .ok_or(e("no fcsh"))?
            .last()
            .ok_or(e("empty fcsh"))?
    };
    let cc_target = VdfInfo::new(
        cc_vdf_challenge,
        ip_vdf_iters,
        rcb.challenge_chain_ip_vdf.output,
    );
    if rcb.challenge_chain_ip_vdf != cc_target.with_iters(ip_iters) {
        return Err(e("INVALID_CC_IP_VDF (data)"));
    }
    let cc_ip_proof = &block.challenge_chain_ip_proof;
    if !cc_ip_proof.normalized_to_identity
        && !verifier.validate_vdf(c, &cc_vdf_output, &cc_target, cc_ip_proof, None)
    {
        return Err(e("INVALID_CC_IP_VDF"));
    }
    if cc_ip_proof.normalized_to_identity
        && !verifier.validate_vdf(
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
    let rc_target = VdfInfo::new(
        rc_vdf_challenge,
        ip_vdf_iters,
        rcb.reward_chain_ip_vdf.output,
    );
    if !verifier.validate_vdf(
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
                        icc_ss.hash()?,
                        Some(ClassgroupElement::get_default_element()),
                    )
                } else {
                    let input = if pb.is_challenge_block(c.min_blocks_per_challenge_block) {
                        Some(ClassgroupElement::get_default_element())
                    } else {
                        pb.infused_challenge_vdf_output
                    };
                    let mut curr = pb;
                    while curr.finished_infused_challenge_slot_hashes.is_none()
                        && !curr.is_challenge_block(c.min_blocks_per_challenge_block)
                    {
                        curr = blocks
                            .get(&curr.prev_hash)
                            .ok_or_else(|| missing(curr.prev_hash))?;
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
            let icc_target = VdfInfo::new(icc_vdf_challenge, ip_vdf_iters, icc_ip_vdf.output);
            let ok = match icc_vdf_input {
                Some(input) => {
                    verifier.validate_vdf(c, &input, icc_ip_vdf, icc_ip_proof, Some(&icc_target))
                }
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
    if block.foliage.reward_block_hash != rcb.hash()? {
        return Err(e("INVALID_REWARD_BLOCK_HASH"));
    }
    // 33. reward block is_transaction_block
    if block.foliage.foliage_transaction_block_hash.is_some() != rcb.is_transaction_block {
        return Err(e("INVALID_FOLIAGE_BLOCK_PRESENCE"));
    }
    Ok(required_iters)
}
