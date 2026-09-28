// Consensus helpers for recent-chain validation.

use std::collections::HashMap;

use dg_xch_core::blockchain::block_record::BlockRecord;
use dg_xch_core::blockchain::class_group_element::ClassgroupElement;
use dg_xch_core::blockchain::header_block::HeaderBlock;
use dg_xch_core::blockchain::proof_of_space::ProofOfSpace;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::blockchain::sub_epoch_summary::SubEpochSummary;
use dg_xch_core::blockchain::vdf_info::VdfInfo;
use dg_xch_core::blockchain::vdf_proof::VdfProof;
use dg_xch_core::consensus::block_header_validation::{
    HeaderValidationVerifier, ValidationState, validate_finished_header_block,
    validate_pospace_and_get_required_iters,
};
use dg_xch_core::consensus::constants::ConsensusConstants;
use dg_xch_core::consensus::deficit::calculate_deficit;
use dg_xch_core::consensus::full_block_to_block_record::header_block_to_sub_block_record;
use dg_xch_core::consensus::get_block_challenge::pre_sp_tx_block_height;
use dg_xch_core::consensus::pot_iterations::is_overflow_block;
use dg_xch_pos::verify_and_get_quality_string;
use dg_xch_vdf::validate_vdf_info;

use crate::{WeightProofError, hash_of};

// The recent-chain block-record cache starts empty and fills as each block is validated, so every lookup
// resolves inside the recent chain itself. Wires the real VDF / proof-of-space verifiers into the
// dg_xch_core HeaderValidationVerifier seam.
pub(crate) struct RecentChainVerifier;

impl HeaderValidationVerifier for RecentChainVerifier {
    fn validate_vdf(
        &self,
        constants: &ConsensusConstants,
        input: &ClassgroupElement,
        info: &VdfInfo,
        proof: &VdfProof,
        target: Option<&VdfInfo>,
    ) -> bool {
        validate_vdf_info(constants, input, info, proof, target)
    }

    fn pospace_quality_string(
        &self,
        constants: &ConsensusConstants,
        proof_of_space: &ProofOfSpace,
        challenge: Bytes32,
        cc_sp_hash: Bytes32,
        height: u32,
    ) -> Option<Bytes32> {
        verify_and_get_quality_string(proof_of_space, constants, challenge, cc_sp_hash, height)
    }
}

// _validate_pospace_recent_chain (weight_proof.py:1338) — the light path.
fn validate_pospace_recent_chain(
    c: &ConsensusConstants,
    blocks: &HashMap<Bytes32, BlockRecord>,
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
    )
    .map_err(|_| e("pre_sp_tx_block_height"))?;
    validate_pospace_and_get_required_iters(
        &RecentChainVerifier,
        c,
        &rcb.proof_of_space,
        if overflow { prev_challenge } else { challenge },
        cc_sp_hash,
        block.height(),
        diff,
        pre_sp_tx_h,
    )
    .map_err(|_| e("validate_pospace_and_get_required_iters"))?
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

// get_deficit (weight_proof.py:1602).
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
    let mut sub_blocks: HashMap<Bytes32, BlockRecord> = HashMap::new();
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
                    sub_blocks.insert(pbr.header_hash, pbr.clone());
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
                    required_iters = validate_finished_header_block(
                        c,
                        &RecentChainVerifier,
                        &sub_blocks,
                        block,
                        vs,
                        ses_blocks > 2,
                    )
                    .map_err(|_| e("validate_finished_header_block"))?;
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
        )
        .map_err(|_| e("header_block_to_sub_block_record"))?;
        sub_blocks.insert(block_record.header_hash, block_record.clone());

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
#[path = "../tests/unit/recent_blocks/tests.rs"]
mod tests;
