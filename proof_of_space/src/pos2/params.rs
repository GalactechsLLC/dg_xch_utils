use crate::pos2::constants::CHAIN_SET_BITS;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use std::io::{Error, ErrorKind};

/// An inclusive range of proof fragment values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    pub start: u64,
    pub end: u64,
}

impl Range {
    #[must_use]
    pub fn contains(&self, value: u64) -> bool {
        value >= self.start && value <= self.end
    }
}

/// The parameters a plot is proved and verified under.
///
/// `match_info` is `k` bits laid out as `[section | match_key | match_target]`, and the widths of
/// the first two fields are what `strength` tunes: a stronger plot spends more match key bits,
/// which costs the plotter time without costing the verifier anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProofParams {
    plot_id: Bytes32,
    k: u8,
    strength: u8,
    testnet: bool,
}

impl ProofParams {
    pub fn new(plot_id: Bytes32, k: u8, strength: u8, testnet: bool) -> Result<Self, Error> {
        if strength < 2 {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                format!("strength {strength} must be at least 2"),
            ));
        }
        if strength > 63 {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                format!("strength {strength} must be below 64"),
            ));
        }
        let params = Self {
            plot_id,
            k,
            strength,
            testnet,
        };
        let ceiling = u32::from(k)
            .checked_sub(params.num_section_bits() + 1)
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::InvalidInput,
                    format!("k {k} is too small for a plot"),
                )
            })?;
        if u32::from(strength) > ceiling {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                format!("strength {strength} must not exceed k - section_bits - 1 ({ceiling})"),
            ));
        }
        Ok(params)
    }

    #[must_use]
    pub fn plot_id(&self) -> Bytes32 {
        self.plot_id
    }

    #[must_use]
    pub fn k(&self) -> u8 {
        self.k
    }

    #[must_use]
    pub fn strength(&self) -> u8 {
        self.strength
    }

    #[must_use]
    pub fn is_testnet(&self) -> bool {
        self.testnet
    }

    #[must_use]
    pub fn num_section_bits(&self) -> u32 {
        if self.k < 28 {
            2
        } else {
            u32::from(self.k) - 26
        }
    }

    #[must_use]
    pub fn num_sections(&self) -> u32 {
        1u32 << self.num_section_bits()
    }

    /// Table 1 always spends two match key bits; later tables spend `strength`.
    #[must_use]
    pub fn num_match_key_bits(&self, table_id: usize) -> u32 {
        assert!(
            (1..=3).contains(&table_id),
            "table_id {table_id} out of range"
        );
        if table_id == 1 {
            2
        } else {
            u32::from(self.strength)
        }
    }

    #[must_use]
    pub fn num_match_keys(&self, table_id: usize) -> u64 {
        1u64 << self.num_match_key_bits(table_id)
    }

    #[must_use]
    pub fn num_match_target_bits(&self, table_id: usize) -> u32 {
        u32::from(self.k) - self.num_section_bits() - self.num_match_key_bits(table_id)
    }

    /// Table 1 carries one x worth of metadata; later tables carry a pair.
    #[must_use]
    pub fn num_meta_bits(&self, table_id: usize) -> u32 {
        if table_id == 1 {
            u32::from(self.k)
        } else {
            u32::from(self.k) * 2
        }
    }

    #[must_use]
    pub fn num_pairing_meta_bits(&self) -> u32 {
        2 * u32::from(self.k)
    }

    #[must_use]
    pub fn extract_section(&self, match_info: u32) -> u32 {
        match_info >> (u32::from(self.k) - self.num_section_bits())
    }

    #[must_use]
    pub fn extract_match_key(&self, table_id: usize, match_info: u32) -> u32 {
        let match_bits = self.num_match_key_bits(table_id);
        let shift = u32::from(self.k) - self.num_section_bits() - match_bits;
        (match_info >> shift) & ((1u32 << match_bits) - 1)
    }

    #[must_use]
    pub fn extract_match_target(&self, table_id: usize, match_info: u64) -> u32 {
        let bits = self.num_match_target_bits(table_id);
        (match_info & ((1u64 << bits) - 1)) as u32
    }

    #[must_use]
    pub fn chaining_set_bits(&self) -> u32 {
        CHAIN_SET_BITS
    }

    #[must_use]
    pub fn chaining_set_size(&self) -> u32 {
        1u32 << self.chaining_set_bits()
    }

    #[must_use]
    pub fn num_chaining_sets_bits(&self) -> u32 {
        u32::from(self.k) - self.chaining_set_bits()
    }

    #[must_use]
    pub fn num_chaining_sets(&self) -> u32 {
        1u32 << self.num_chaining_sets_bits()
    }

    #[must_use]
    pub fn chaining_set_range(&self, chaining_set_index: u64) -> Range {
        let width = 1u64 << (u32::from(self.k) + self.chaining_set_bits());
        let start = chaining_set_index * width;
        Range {
            start,
            end: start + width - 1,
        }
    }
}

#[cfg(test)]
#[path = "../../tests/unit/pos2/params/tests.rs"]
mod tests;
