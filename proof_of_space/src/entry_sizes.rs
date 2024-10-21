use crate::constants::{
    byte_align, ucdiv, BITS_PER_INTERVAL, K_CHECKPOINT1INTERVAL, K_ENTRIES_PER_PARK, K_EXTRA_BITS,
    K_MAX_AVERAGE_DELTA, K_MAX_AVERAGE_DELTA_TABLE1, K_OFFSET_SIZE, K_STUB_MINUS_BITS,
    K_VECTOR_LENS,
};
use dg_xch_core::plots::PlotTable;
use std::cmp::max;

pub struct EntrySizes {}
impl EntrySizes {
    #[must_use]
    pub fn get_max_entry_size(k: u8, table_index: u8, phase_1_size: bool) -> u32 {
        match table_index {
            1 => {
                if phase_1_size {
                    byte_align(u32::from(k + K_EXTRA_BITS + k)) / 8
                } else {
                    // After computing matches, table 1 is rewritten without the f1, which
                    // is useless after phase1.
                    byte_align(u32::from(k)) / 8
                }
            }
            // Represents f1, x
            2..=6 => {
                if phase_1_size {
                    // If we are in phase 1, use the max size, with metadata.
                    // Represents f, pos, offset, and metadata
                    byte_align(
                        u32::from(k)
                            + u32::from(K_EXTRA_BITS)
                            + u32::from(k)
                            + K_OFFSET_SIZE
                            + u32::from(k) * u32::from(K_VECTOR_LENS[(table_index + 1) as usize]),
                    ) / 8
                } else {
                    // If we are past phase 1, we can use a smaller size, the smaller between
                    // phases 2 and 3. Represents either:
                    //    a:  sort_key, pos, offset        or
                    //    b:  line_point, sort_key
                    byte_align(max(2 * u32::from(k) + K_OFFSET_SIZE, u32::from(3 * k - 1))) / 8
                }
            }
            _ => byte_align(u32::from(3 * k - 1)) / 8,
        }
    }

    // Get size of entries containing (sort_key, pos, offset). Such entries are
    // written to table 7 in phase 1 and to tables 2-7 in phase 2.
    #[must_use]
    pub fn get_key_pos_offset_size(k: u8) -> u32 {
        ucdiv(2 * u32::from(k) + K_OFFSET_SIZE, 8)
    }

    // Calculates the size of one C3 park. This will store bits for each f7 between
    // two C1 checkpoints, depending on how many times that f7 is present. For low
    // values of k, we need extra space to account for the additional variability.
    #[must_use]
    pub fn calculate_c3size(k: u32) -> u32 {
        if k < 20 {
            ucdiv(8 * K_CHECKPOINT1INTERVAL, 8)
        } else {
            ucdiv(BITS_PER_INTERVAL, 8)
        }
    }
    #[must_use]
    pub fn calculate_park7_size(k: u32) -> u32 {
        ucdiv((k + 1) * K_ENTRIES_PER_PARK, 8)
    }
    // This is the full size of the deltas section in a park. However, it will not be fully filled
    #[allow(clippy::cast_possible_truncation)]
    #[allow(clippy::cast_sign_loss)]
    #[must_use]
    pub fn calculate_max_deltas_size(table: PlotTable) -> u32 {
        assert!(table < PlotTable::Table7);
        if table == PlotTable::Table1 {
            ucdiv(
                (f64::from(K_ENTRIES_PER_PARK - 1) * K_MAX_AVERAGE_DELTA_TABLE1) as u32,
                8,
            )
        } else {
            ucdiv(
                (f64::from(K_ENTRIES_PER_PARK - 1) * K_MAX_AVERAGE_DELTA) as u32,
                8,
            )
        }
    }

    #[must_use]
    pub fn calculate_stubs_size(k: u32) -> u32 {
        byte_align((K_ENTRIES_PER_PARK - 1) * (k - u32::from(K_STUB_MINUS_BITS))) / 8
    }

    #[must_use]
    pub fn calculate_park_size(table: PlotTable, k: u32) -> u32 {
        Self::line_point_size_bytes(k)
            + ucdiv(
                (K_ENTRIES_PER_PARK - 1) * (k - u32::from(K_STUB_MINUS_BITS)),
                8,
            )
            + Self::calculate_max_deltas_size(table)
    }
    #[must_use]
    pub fn line_point_size_bits(k: u32) -> u32 {
        k * 2
    }
    #[must_use]
    pub fn line_point_size_bytes(k: u32) -> u32 {
        ucdiv(Self::line_point_size_bits(k), 8)
    }
    #[must_use]
    pub fn round_up_to_next_boundary(value: usize, boundary: usize) -> usize {
        value + (boundary - (value % boundary)) % boundary
    }
}
