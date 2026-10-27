// Challenge-chain lookup and pre-signage-point transaction-block traversal.

use crate::blockchain::block_record::BlockRecord;
use crate::blockchain::sized_bytes::Bytes32;
use crate::consensus::constants::ConsensusConstants;
use crate::consensus::missing;
use crate::consensus::pot_iterations::is_overflow_block;
use std::collections::HashMap;
use std::io::{Error, ErrorKind};

/// Whether an earlier overflow block already included the final slot boundary.
/// Matches Chia's `final_eos_is_already_included` for unfinished blocks.
pub fn final_eos_is_already_included(
    finished_sub_slots: usize,
    prev_block_hash: Bytes32,
    total_iters: u128,
    sub_slot_iters: u64,
    blocks: &HashMap<Bytes32, BlockRecord>,
) -> Result<bool, Error> {
    if finished_sub_slots > 0 {
        return Ok(false);
    }
    let mut curr = blocks
        .get(&prev_block_hash)
        .ok_or_else(|| missing(prev_block_hash))?;
    let nearby_overflow = |record: &BlockRecord| {
        record.overflow
            && total_iters
                .checked_sub(record.total_iters)
                .is_some_and(|delta| delta < u128::from(sub_slot_iters / 2))
    };
    let mut seen_overflow = nearby_overflow(curr);
    while !curr.first_in_sub_slot() && curr.height != 0 {
        seen_overflow |= nearby_overflow(curr);
        curr = blocks
            .get(&curr.prev_hash)
            .ok_or_else(|| missing(curr.prev_hash))?;
    }
    Ok(curr.first_in_sub_slot() && seen_overflow)
}

pub fn get_block_challenge(
    constants: &ConsensusConstants,
    finished_sub_slots: &[crate::blockchain::subslot_bundle::SubSlotBundle],
    prev_block_hash: Bytes32,
    blocks: &HashMap<Bytes32, BlockRecord>,
    genesis_block: bool,
    overflow: bool,
    skip_overflow_last_ss_validation: bool,
) -> Result<Bytes32, Error> {
    let fss = finished_sub_slots;
    if !fss.is_empty() {
        let last = &fss[fss.len() - 1];
        let challenge = if overflow {
            if skip_overflow_last_ss_validation {
                last.challenge_chain.hash()?
            } else {
                last.challenge_chain
                    .challenge_chain_end_of_slot_vdf
                    .challenge
            }
        } else {
            last.challenge_chain.hash()?
        };
        return Ok(challenge);
    }
    if genesis_block {
        return Ok(constants.genesis_challenge);
    }
    let challenges_to_look_for: usize = if overflow && !skip_overflow_last_ss_validation {
        2
    } else {
        1
    };
    let mut reversed_challenge_hashes: Vec<Bytes32> = Vec::new();
    let mut curr = blocks
        .get(&prev_block_hash)
        .ok_or_else(|| missing(prev_block_hash))?;
    while reversed_challenge_hashes.len() < challenges_to_look_for {
        if curr.first_in_sub_slot() {
            let hashes = curr
                .finished_challenge_slot_hashes
                .as_ref()
                .ok_or_else(|| {
                    Error::new(ErrorKind::InvalidData, "no finished_challenge_slot_hashes")
                })?;
            reversed_challenge_hashes.extend(hashes.iter().rev().copied());
            if reversed_challenge_hashes.len() >= challenges_to_look_for {
                break;
            }
        }
        if curr.height == 0 {
            let hashes = curr
                .finished_challenge_slot_hashes
                .as_ref()
                .ok_or_else(|| {
                    Error::new(
                        ErrorKind::InvalidData,
                        "genesis no finished_challenge_slot_hashes",
                    )
                })?;
            if hashes.is_empty() {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "genesis empty challenge hashes",
                ));
            }
            break;
        }
        curr = blocks
            .get(&curr.prev_hash)
            .ok_or_else(|| missing(curr.prev_hash))?;
    }
    reversed_challenge_hashes
        .get(challenges_to_look_for - 1)
        .copied()
        .ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidData,
                "get_block_challenge: not enough challenges",
            )
        })
}

/// Return the transaction block preceding the signage point.
pub fn pre_sp_tx_block<'a>(
    constants: &ConsensusConstants,
    blocks: &'a HashMap<Bytes32, BlockRecord>,
    prev_b_hash: Bytes32,
    sp_index: u8,
    finished_sub_slots: usize,
) -> Result<Option<&'a BlockRecord>, Error> {
    if prev_b_hash == constants.genesis_challenge {
        return Ok(None);
    }
    let mut curr = blocks
        .get(&prev_b_hash)
        .ok_or_else(|| missing(prev_b_hash))?;
    let overflow = is_overflow_block(constants, sp_index)?;
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
        curr = blocks
            .get(&curr.prev_hash)
            .ok_or_else(|| missing(curr.prev_hash))?;
    }
    Ok(Some(curr))
}

pub fn pre_sp_tx_block_height(
    constants: &ConsensusConstants,
    blocks: &HashMap<Bytes32, BlockRecord>,
    prev_b_hash: Bytes32,
    sp_index: u8,
    finished_sub_slots: usize,
) -> Result<u32, Error> {
    Ok(
        pre_sp_tx_block(constants, blocks, prev_b_hash, sp_index, finished_sub_slots)?
            .map_or(0, |b| b.height),
    )
}
