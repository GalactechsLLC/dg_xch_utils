use crate::constants::*;
use std::cmp::max;

pub struct EntrySizes {}
impl EntrySizes {
    pub fn get_max_entry_size(k: u8, table_index: u8, phase_1_size: bool) -> u32 {
        match table_index {
            1 => {
                if phase_1_size {
                    byte_align((k + K_EXTRA_BITS + k) as u32) / 8
                } else {
                    // After computing matches, table 1 is rewritten without the f1, which
                    // is useless after phase1.
                    byte_align(k as u32) / 8
                }
            }
            // Represents f1, x
            2 | 3 | 4 | 5 | 6 => {
                if phase_1_size {
                    // If we are in phase 1, use the max size, with metadata.
                    // Represents f, pos, offset, and metadata
                    byte_align(
                        k as u32
                            + K_EXTRA_BITS as u32
                            + k as u32
                            + K_OFFSET_SIZE
                            + k as u32 * K_VECTOR_LENS[(table_index + 1) as usize] as u32,
                    ) / 8
                } else {
                    // If we are past phase 1, we can use a smaller size, the smaller between
                    // phases 2 and 3. Represents either:
                    //    a:  sort_key, pos, offset        or
                    //    b:  line_point, sort_key
                    byte_align(max(2 * k as u32 + K_OFFSET_SIZE, (3 * k - 1) as u32)) / 8
                }
            }
            _ => byte_align((3 * k - 1) as u32) / 8,
        }
    }

    // Get size of entries containing (sort_key, pos, offset). Such entries are
    // written to table 7 in phase 1 and to tables 2-7 in phase 2.
    pub fn get_key_pos_offset_size(k: u8) -> u32 {
        ucdiv(2 * k as u32 + K_OFFSET_SIZE, 8)
    }

    // Calculates the size of one C3 park. This will store bits for each f7 between
    // two C1 checkpoints, depending on how many times that f7 is present. For low
    // values of k, we need extra space to account for the additional variability.
    pub fn calculate_c3size(k: u8) -> u32 {
        if k < 20 {
            byte_align(8 * K_CHECKPOINT1INTERVAL) / 8
        } else {
            byte_align((K_C3BITS_PER_ENTRY * K_CHECKPOINT1INTERVAL as f64) as u32) / 8
        }
    }

    pub fn calculate_line_point_size(k: u8) -> u32 {
        byte_align(2 * k as u32) / 8
    }

    // This is the full size of the deltas section in a park. However, it will not be fully filled
    pub fn calculate_max_deltas_size(table_index: u8) -> u32 {
        if table_index == 1 {
            byte_align(((K_ENTRIES_PER_PARK - 1) as f64 * K_MAX_AVERAGE_DELTA_TABLE1) as u32) / 8
        } else {
            byte_align(((K_ENTRIES_PER_PARK - 1) as f64 * K_MAX_AVERAGE_DELTA) as u32) / 8
        }
    }

    pub fn calculate_stubs_size(k: u32) -> u32 {
        byte_align((K_ENTRIES_PER_PARK - 1) * (k - K_STUB_MINUS_BITS as u32)) / 8
    }

    pub fn calculate_park_size(k: u8, table_index: u8) -> u32 {
        Self::calculate_line_point_size(k)
            + Self::calculate_stubs_size(k as u32)
            + Self::calculate_max_deltas_size(table_index)
    }
}
