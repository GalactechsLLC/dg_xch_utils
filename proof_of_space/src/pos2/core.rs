use crate::pos2::constants::NUM_CHALLENGE_SETS;
use crate::pos2::fragment::{ProofFragment, ProofFragmentCodec};
use crate::pos2::hashing::ProofHashing;
use crate::pos2::params::{ProofParams, Range};
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use std::io::Error;

/// A surviving table 1 pairing: the two x values' combined metadata and the `match_info` it
/// projects into table 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct T1Pairing {
    pub meta: u64,
    pub match_info: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct T2Pairing {
    pub meta: u64,
    pub match_info: u32,
    pub x_bits: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct T3Pairing {
    pub proof_fragment: ProofFragment,
}

/// The chaining sets a challenge selects, and the fragment ranges they cover.
///
/// Each index is forced to its own residue modulo `NUM_CHALLENGE_SETS`, so the four sets are
/// mutually exclusive and a chain that starts in one cycles through all of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectedChallengeSets {
    pub indexes: [u32; NUM_CHALLENGE_SETS],
    pub ranges: [Range; NUM_CHALLENGE_SETS],
}

/// The matching rules.
///
/// Two entries pair when three things hold: their sections are the matching pair for that table,
/// the right entry's `match_target` equals the target the left one's metadata projects, and the
/// pairing's own filter bits come out zero.
#[derive(Debug, Clone)]
pub struct ProofCore {
    pub hashing: ProofHashing,
    pub fragment_codec: ProofFragmentCodec,
    params: ProofParams,
}

impl ProofCore {
    pub fn new(params: ProofParams) -> Result<Self, Error> {
        let fragment_codec = ProofFragmentCodec::new(params.plot_id(), u32::from(params.k()))?;
        Ok(Self {
            hashing: ProofHashing::new(params.clone()),
            fragment_codec,
            params,
        })
    }

    #[must_use]
    pub fn params(&self) -> &ProofParams {
        &self.params
    }

    #[must_use]
    pub fn matching_target(&self, table_id: usize, meta: u64, match_key: u32) -> u32 {
        let bits = self.params.num_match_target_bits(table_id);
        self.hashing
            .matching_target(table_id as u32, match_key, meta, bits)
    }

    /// The section a left entry in `section` must find its partner in.
    #[must_use]
    pub fn matching_section(&self, section: u32) -> u32 {
        let bits = self.params.num_section_bits();
        let count = self.params.num_sections();
        let rotated = (section << 1) | (section >> (bits - 1));
        let bumped = (rotated + 1) & (count - 1);
        ((bumped >> 1) | (bumped << (bits - 1))) & (count - 1)
    }

    /// The other section that maps onto `section`, for walking the relation backwards.
    #[must_use]
    pub fn inverse_matching_section(&self, section: u32) -> u32 {
        let bits = self.params.num_section_bits();
        let count = self.params.num_sections();
        let rotated = ((section << 1) | (section >> (bits - 1))) & (count - 1);
        let lowered = rotated.wrapping_sub(1) & (count - 1);
        ((lowered >> 1) | (lowered << (bits - 1))) & (count - 1)
    }

    /// Whether a right entry's `match_info` really pairs with a left entry's.
    #[must_use]
    pub fn validate_match_info_pairing(
        &self,
        table_id: usize,
        meta_l: u64,
        match_info_l: u32,
        match_info_r: u32,
    ) -> bool {
        let section_l = self.params.extract_section(match_info_l);
        let section_r = self.params.extract_section(match_info_r);
        if section_r != self.matching_section(section_l) {
            return false;
        }
        let match_key_r = self.params.extract_match_key(table_id, match_info_r);
        let match_target_r = self
            .params
            .extract_match_target(table_id, u64::from(match_info_r));
        match_target_r == self.matching_target(table_id, meta_l, match_key_r)
    }

    /// Pair two x values into table 2. `None` when the pairing's filter bits reject it.
    #[must_use]
    pub fn pairing_t1(&self, x_l: u32, x_r: u32) -> Option<T1Pairing> {
        let test_bits = self.params.num_match_key_bits(1);
        let pair = self.hashing.pairing_t1(
            u64::from(x_l),
            u64::from(x_r),
            u32::from(self.params.k()),
            self.params.num_pairing_meta_bits(),
            test_bits,
        );
        if pair.test != 0 {
            return None;
        }
        Some(T1Pairing {
            meta: (u64::from(x_l) << self.params.k()) | u64::from(x_r),
            match_info: pair.match_info,
        })
    }

    #[must_use]
    pub fn pairing_t2(&self, meta_l: u64, meta_r: u64) -> Option<T2Pairing> {
        let test_bits = self.params.num_match_key_bits(2);
        let pair = self.hashing.pairing_t2(
            meta_l,
            meta_r,
            u32::from(self.params.k()),
            self.params.num_pairing_meta_bits(),
            test_bits,
        );
        if pair.test != 0 {
            return None;
        }
        let k = u32::from(self.params.k());
        let half_k = k / 2;
        // The metadata holds two x values; only their top halves carry into the fragment.
        let x_bits_l = ((meta_l >> k) >> half_k) as u32;
        let x_bits_r = ((meta_r >> k) >> half_k) as u32;
        Some(T2Pairing {
            meta: pair.meta,
            match_info: pair.match_info,
            x_bits: (x_bits_l << half_k) | x_bits_r,
        })
    }

    #[must_use]
    pub fn pairing_t3(
        &self,
        meta_l: u64,
        meta_r: u64,
        x_bits_l: u32,
        x_bits_r: u32,
    ) -> Option<T3Pairing> {
        let test_bits = self.params.num_match_key_bits(3);
        if self.hashing.pairing_t3(meta_l, meta_r, test_bits).test != 0 {
            return None;
        }
        let all_x_bits = (u64::from(x_bits_l) << self.params.k()) | u64::from(x_bits_r);
        Some(T3Pairing {
            proof_fragment: self.fragment_codec.encode_bits(all_x_bits),
        })
    }

    /// The four chaining sets this challenge opens.
    #[must_use]
    pub fn select_challenge_sets(&self, challenge: Bytes32) -> SelectedChallengeSets {
        let hash = self.hashing.challenge_with_plot_id_hash(challenge);
        let bits = self.params.num_chaining_sets_bits();
        let sets_mask = (1u32 << bits) - 1;
        let high_bits_mask = sets_mask & !(NUM_CHALLENGE_SETS as u32 - 1);
        let mut indexes = [0u32; NUM_CHALLENGE_SETS];
        let mut ranges = [Range { start: 0, end: 0 }; NUM_CHALLENGE_SETS];
        for i in 0..NUM_CHALLENGE_SETS {
            // A different digest word per set keeps the high bits independent, while the low bits
            // pin each index to its own residue.
            let index = (hash[i] & high_bits_mask) | i as u32;
            indexes[i] = index;
            ranges[i] = self.params.chaining_set_range(u64::from(index));
        }
        SelectedChallengeSets { indexes, ranges }
    }
}

#[cfg(test)]
#[path = "../../tests/unit/pos2/core/tests.rs"]
mod tests;
