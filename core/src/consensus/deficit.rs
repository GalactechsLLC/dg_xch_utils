//! Block-deficit state transitions.

use crate::blockchain::block_record::BlockRecord;
use crate::consensus::constants::ConsensusConstants;

/// Calculate the next deficit from the previous block and crossed sub-slots.
#[must_use]
pub fn calculate_deficit(
    constants: &ConsensusConstants,
    height: u32,
    prev_b: Option<&BlockRecord>,
    overflow: bool,
    num_finished_sub_slots: usize,
) -> u8 {
    let min = constants.min_blocks_per_challenge_block;
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

#[cfg(test)]
#[path = "../../tests/unit/consensus/deficit/tests.rs"]
mod tests;
