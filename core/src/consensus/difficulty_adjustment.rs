// Epoch-boundary difficulty and sub-slot-iteration retargeting.

use crate::blockchain::block_record::BlockRecord;
use crate::blockchain::sized_bytes::Bytes32;
use crate::consensus::constants::ConsensusConstants;
use crate::consensus::missing;
use std::collections::HashMap;
use std::io::{Error, ErrorKind};

// Truncate to significant bits.
#[must_use]
pub fn truncate_to_significant_bits(value: u128, num_significant_bits: u64) -> u128 {
    let bit_length = u64::from(128 - value.leading_zeros());
    if num_significant_bits >= bit_length {
        return value;
    }
    let lower = bit_length - num_significant_bits;
    (value >> lower) << lower
}

// Count significant bits.
#[must_use]
pub fn count_significant_bits(value: u128) -> u64 {
    if value == 0 {
        return 0;
    }
    u64::from(128 - value.leading_zeros() - value.trailing_zeros())
}

#[must_use]
pub fn height_can_be_first_in_epoch(constants: &ConsensusConstants, height: u32) -> bool {
    (height - (height % constants.sub_epoch_blocks)).is_multiple_of(constants.epoch_blocks)
}

pub fn can_finish_sub_and_full_epoch(
    constants: &ConsensusConstants,
    blocks: &HashMap<Bytes32, BlockRecord>,
    height: u32,
    prev_header_hash: Bytes32,
    deficit: u8,
    block_at_height_included_ses: bool,
) -> Result<(bool, bool), Error> {
    if height < constants.sub_epoch_blocks - 1 {
        return Ok((false, false));
    }
    if deficit > 0 {
        return Ok((false, false));
    }
    if block_at_height_included_ses {
        return Ok((false, false));
    }
    if (height + 1) % constants.sub_epoch_blocks > 1 {
        let mut curr = blocks
            .get(&prev_header_hash)
            .ok_or_else(|| missing(prev_header_hash))?;
        while curr.height % constants.sub_epoch_blocks > 0 {
            if curr.sub_epoch_summary_included.is_some() {
                return Ok((false, false));
            }
            curr = blocks
                .get(&curr.prev_hash)
                .ok_or_else(|| missing(curr.prev_hash))?;
        }
        if curr.sub_epoch_summary_included.is_some() {
            return Ok((false, false));
        }
    }
    Ok((true, height_can_be_first_in_epoch(constants, height + 1)))
}

pub fn get_second_to_last_transaction_block_in_previous_epoch<'a>(
    constants: &ConsensusConstants,
    blocks: &'a HashMap<Bytes32, BlockRecord>,
    last_b: &'a BlockRecord,
) -> Result<&'a BlockRecord, Error> {
    let height_in_next_epoch = last_b.height
        + 2 * constants.max_sub_slot_blocks
        + u32::from(constants.min_blocks_per_challenge_block)
        + 5;
    let height_epoch_surpass =
        height_in_next_epoch - (height_in_next_epoch % constants.epoch_blocks);
    let height_prev_epoch_surpass = height_epoch_surpass - constants.epoch_blocks;

    if height_in_next_epoch - height_epoch_surpass >= 5 * constants.max_sub_slot_blocks {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "height_in_next_epoch too far past the epoch boundary",
        ));
    }

    if height_prev_epoch_surpass == 0 {
        let mut curr = last_b;
        while curr.height > 0 {
            curr = blocks
                .get(&curr.prev_hash)
                .ok_or_else(|| missing(curr.prev_hash))?;
        }
        return Ok(curr);
    }

    let mut by_height: HashMap<u32, &BlockRecord> = HashMap::new();
    let mut curr = last_b;
    loop {
        by_height.insert(curr.height, curr);
        if curr.height < height_prev_epoch_surpass {
            break;
        }
        curr = blocks
            .get(&curr.prev_hash)
            .ok_or_else(|| missing(curr.prev_hash))?;
    }

    let mut next_height = height_prev_epoch_surpass;
    loop {
        let next_b = by_height.get(&next_height).copied().ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidData,
                "previous-epoch sub-epoch-summary block not found in ancestor window",
            )
        })?;
        if next_b.sub_epoch_summary_included.is_some() {
            break;
        }
        next_height += 1;
    }
    let mut curr_b = *by_height.get(&(next_height - 1)).ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidData,
            "block before previous-epoch sub-epoch-summary block missing",
        )
    })?;

    let mut found_tx_block = u8::from(curr_b.is_transaction_block());
    while found_tx_block < 2 {
        curr_b = blocks
            .get(&curr_b.prev_hash)
            .ok_or_else(|| missing(curr_b.prev_hash))?;
        if curr_b.is_transaction_block() {
            found_tx_block += 1;
        }
    }
    Ok(curr_b)
}

#[allow(clippy::too_many_arguments)]
pub fn get_next_difficulty(
    constants: &ConsensusConstants,
    blocks: &HashMap<Bytes32, BlockRecord>,
    prev_header_hash: Bytes32,
    height: u32,
    current_difficulty: u64,
    deficit: u8,
    block_at_height_included_ses: bool,
    new_slot: bool,
    signage_point_total_iters: u128,
) -> Result<u64, Error> {
    let next_height = height + 1;

    if next_height < constants.epoch_blocks - 3 * constants.max_sub_slot_blocks {
        return Ok(constants.difficulty_starting);
    }

    let prev_b = blocks
        .get(&prev_header_hash)
        .ok_or_else(|| missing(prev_header_hash))?;

    let (_, can_finish_epoch) = can_finish_sub_and_full_epoch(
        constants,
        blocks,
        height,
        prev_header_hash,
        deficit,
        block_at_height_included_ses,
    )?;
    if !new_slot || !can_finish_epoch {
        return Ok(current_difficulty);
    }

    let last_block_prev =
        get_second_to_last_transaction_block_in_previous_epoch(constants, blocks, prev_b)?;

    let mut last_block_curr = prev_b;
    while last_block_curr.total_iters > signage_point_total_iters
        || !last_block_curr.is_transaction_block()
    {
        last_block_curr = blocks
            .get(&last_block_curr.prev_hash)
            .ok_or_else(|| missing(last_block_curr.prev_hash))?;
    }

    let curr_ts = last_block_curr
        .timestamp
        .ok_or_else(|| Error::new(ErrorKind::InvalidData, "last_block_curr has no timestamp"))?;
    let prev_ts = last_block_prev
        .timestamp
        .ok_or_else(|| Error::new(ErrorKind::InvalidData, "last_block_prev has no timestamp"))?;
    let actual_epoch_time = curr_ts
        .checked_sub(prev_ts)
        .filter(|t| *t > 0)
        .ok_or_else(|| Error::new(ErrorKind::InvalidData, "non-positive actual epoch time"))?;

    let prev_prev = blocks
        .get(&prev_b.prev_hash)
        .ok_or_else(|| missing(prev_b.prev_hash))?;
    let old_difficulty = u64::try_from(prev_b.weight - prev_prev.weight)
        .map_err(|_| Error::new(ErrorKind::InvalidData, "old difficulty exceeds u64"))?;

    let numerator = (last_block_curr.weight - last_block_prev.weight)
        * u128::from(constants.sub_slot_time_target);
    let denominator = u128::from(constants.slot_blocks_target) * u128::from(actual_epoch_time);
    let mut new_difficulty_precise = numerator / denominator;

    let max_diff = u128::from(constants.difficulty_change_max_factor) * u128::from(old_difficulty);
    let min_diff = u128::from(old_difficulty) / u128::from(constants.difficulty_change_max_factor);
    if new_difficulty_precise >= u128::from(old_difficulty) {
        new_difficulty_precise = new_difficulty_precise.min(max_diff);
    } else {
        new_difficulty_precise = new_difficulty_precise.max(1).max(min_diff);
    }

    let new_difficulty =
        truncate_to_significant_bits(new_difficulty_precise, constants.significant_bits);
    u64::try_from(new_difficulty)
        .map_err(|_| Error::new(ErrorKind::InvalidData, "new difficulty exceeds u64"))
}

#[allow(clippy::too_many_arguments)]
pub fn get_next_sub_slot_iters(
    constants: &ConsensusConstants,
    blocks: &HashMap<Bytes32, BlockRecord>,
    prev_header_hash: Bytes32,
    height: u32,
    curr_sub_slot_iters: u64,
    deficit: u8,
    block_at_height_included_ses: bool,
    new_slot: bool,
    signage_point_total_iters: u128,
) -> Result<u64, Error> {
    let next_height = height + 1;

    if next_height < constants.epoch_blocks {
        return Ok(constants.sub_slot_iters_starting);
    }

    blocks
        .get(&prev_header_hash)
        .ok_or_else(|| missing(prev_header_hash))?;

    let (_, can_finish_epoch) = can_finish_sub_and_full_epoch(
        constants,
        blocks,
        height,
        prev_header_hash,
        deficit,
        block_at_height_included_ses,
    )?;
    if !new_slot || !can_finish_epoch {
        return Ok(curr_sub_slot_iters);
    }

    let prev_b = blocks
        .get(&prev_header_hash)
        .ok_or_else(|| missing(prev_header_hash))?;
    let last_block_prev =
        get_second_to_last_transaction_block_in_previous_epoch(constants, blocks, prev_b)?;

    let mut last_block_curr = prev_b;
    while last_block_curr.total_iters > signage_point_total_iters
        || !last_block_curr.is_transaction_block()
    {
        last_block_curr = blocks
            .get(&last_block_curr.prev_hash)
            .ok_or_else(|| missing(last_block_curr.prev_hash))?;
    }

    let curr_ts = last_block_curr
        .timestamp
        .ok_or_else(|| Error::new(ErrorKind::InvalidData, "last_block_curr has no timestamp"))?;
    let prev_ts = last_block_prev
        .timestamp
        .ok_or_else(|| Error::new(ErrorKind::InvalidData, "last_block_prev has no timestamp"))?;
    let block_time = curr_ts
        .checked_sub(prev_ts)
        .filter(|t| *t > 0)
        .ok_or_else(|| Error::new(ErrorKind::InvalidData, "non-positive block time"))?;

    let numerator = u128::from(constants.sub_slot_time_target)
        * (last_block_curr.total_iters - last_block_prev.total_iters);
    let mut new_ssi_precise = numerator / u128::from(block_time);

    let curr_ssi = u128::from(last_block_curr.sub_slot_iters);
    let max_ssi = u128::from(constants.difficulty_change_max_factor) * curr_ssi;
    let min_ssi = curr_ssi / u128::from(constants.difficulty_change_max_factor);
    if new_ssi_precise >= curr_ssi {
        new_ssi_precise = new_ssi_precise.min(max_ssi);
    } else {
        new_ssi_precise = new_ssi_precise
            .max(u128::from(constants.num_sps_sub_slot))
            .max(min_ssi);
    }

    let truncated = truncate_to_significant_bits(new_ssi_precise, constants.significant_bits);
    let new_ssi = truncated - (truncated % u128::from(constants.num_sps_sub_slot));
    u64::try_from(new_ssi)
        .map_err(|_| Error::new(ErrorKind::InvalidData, "new sub_slot_iters exceeds u64"))
}

// Full-node entry point; returns (sub_slot_iters, difficulty).
pub fn get_next_sub_slot_iters_and_difficulty(
    constants: &ConsensusConstants,
    is_first_in_sub_slot: bool,
    prev_b: Option<&BlockRecord>,
    blocks: &HashMap<Bytes32, BlockRecord>,
) -> Result<(u64, u64), Error> {
    let Some(prev_b) = prev_b else {
        return Ok((
            constants.sub_slot_iters_starting,
            constants.difficulty_starting,
        ));
    };

    let prev_difficulty = if prev_b.height != 0 {
        let prev_prev = blocks
            .get(&prev_b.prev_hash)
            .ok_or_else(|| missing(prev_b.prev_hash))?;
        u64::try_from(prev_b.weight - prev_prev.weight)
            .map_err(|_| Error::new(ErrorKind::InvalidData, "previous difficulty exceeds u64"))?
    } else {
        u64::try_from(prev_b.weight)
            .map_err(|_| Error::new(ErrorKind::InvalidData, "genesis weight exceeds u64"))?
    };

    if prev_b.sub_epoch_summary_included.is_some() {
        return Ok((prev_b.sub_slot_iters, prev_difficulty));
    }

    let sp_total_iters = prev_b.sp_total_iters(constants)?;

    let difficulty = get_next_difficulty(
        constants,
        blocks,
        prev_b.prev_hash,
        prev_b.height,
        prev_difficulty,
        prev_b.deficit,
        false,
        is_first_in_sub_slot,
        sp_total_iters,
    )?;

    let sub_slot_iters = get_next_sub_slot_iters(
        constants,
        blocks,
        prev_b.prev_hash,
        prev_b.height,
        prev_b.sub_slot_iters,
        prev_b.deficit,
        false,
        is_first_in_sub_slot,
        sp_total_iters,
    )?;

    Ok((sub_slot_iters, difficulty))
}

#[must_use]
pub fn consensus_walk_window(constants: &ConsensusConstants) -> usize {
    (constants.epoch_blocks + constants.sub_epoch_blocks + 6 * constants.max_sub_slot_blocks)
        as usize
}

#[must_use]
pub fn difficulty_record_depth(constants: &ConsensusConstants, prev_height: u32) -> u32 {
    // The can_finish_sub_and_full_epoch walk floor: the last sub-epoch boundary at or below prev.
    let mut lowest = prev_height - (prev_height % constants.sub_epoch_blocks);
    if height_can_be_first_in_epoch(constants, prev_height.saturating_add(1)) {
        let height_in_next_epoch = prev_height
            + 2 * constants.max_sub_slot_blocks
            + u32::from(constants.min_blocks_per_challenge_block)
            + 5;
        let height_epoch_surpass =
            height_in_next_epoch - (height_in_next_epoch % constants.epoch_blocks);
        lowest = lowest.min(height_epoch_surpass.saturating_sub(constants.epoch_blocks));
    }
    let lowest = lowest.saturating_sub(4 * constants.max_sub_slot_blocks);
    (prev_height - lowest).saturating_add(1)
}

#[cfg(test)]
#[path = "../../tests/unit/consensus/difficulty_adjustment/tests.rs"]
mod tests;
