use crate::chainer::{Chain, Chainer, QualityChainLinks};
use crate::constants::{NUM_CHAIN_LINKS, TOTAL_XS_IN_PROOF};
use crate::core::{ProofCore, T1Pairing, T2Pairing, T3Pairing};
use crate::params::ProofParams;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use std::io::Error;

/// Verifies a proof of space 2 proof.
///
/// A proof is 128 x values in sixteen groups of eight. Each group must pair all the way up through
/// tables 1, 2 and 3, and the sixteen fragments those groups encode to must then form a valid
/// quality chain against the challenge. The plot filter is the caller's job, not this one's.
#[derive(Debug, Clone)]
pub struct ProofValidator {
    core: ProofCore,
}

impl ProofValidator {
    pub fn new(params: ProofParams) -> Result<Self, Error> {
        Ok(Self {
            core: ProofCore::new(params)?,
        })
    }

    #[must_use]
    pub fn core(&self) -> &ProofCore {
        &self.core
    }

    /// Two x values pair into table 2 when their `match_info` relation holds and the pairing's own
    /// filter accepts them.
    #[must_use]
    pub fn validate_table_1_pair(&self, xs: &[u32; 2]) -> Option<T1Pairing> {
        let match_info_l = self.core.hashing.g(xs[0]);
        let match_info_r = self.core.hashing.g(xs[1]);
        self.validate_table_1_with_matches(xs, match_info_l, match_info_r)
    }

    fn validate_table_1_with_matches(
        &self,
        xs: &[u32; 2],
        match_info_l: u32,
        match_info_r: u32,
    ) -> Option<T1Pairing> {
        if xs
            .iter()
            .any(|value| u64::from(*value) >= (1u64 << self.core.params().k()))
        {
            return None;
        }
        if !self
            .core
            .validate_match_info_pairing(1, u64::from(xs[0]), match_info_l, match_info_r)
        {
            return None;
        }
        self.core.pairing_t1(xs[0], xs[1])
    }

    #[must_use]
    pub fn validate_table_2_pairs(&self, xs: &[u32; 4]) -> Option<T2Pairing> {
        let left = self.validate_table_1_pair(&[xs[0], xs[1]])?;
        let right = self.validate_table_1_pair(&[xs[2], xs[3]])?;
        self.validate_table_2_with_pairs(left, right)
    }

    fn validate_table_2_with_pairs(&self, left: T1Pairing, right: T1Pairing) -> Option<T2Pairing> {
        if !self
            .core
            .validate_match_info_pairing(2, left.meta, left.match_info, right.match_info)
        {
            return None;
        }
        self.core.pairing_t2(left.meta, right.meta)
    }

    #[must_use]
    pub fn validate_table_3_pairs(&self, xs: &[u32; 8]) -> Option<T3Pairing> {
        let mut matches = [0; 8];
        self.core.hashing.g_batch(xs, &mut matches);
        let pair = |offset: usize| {
            self.validate_table_1_with_matches(
                &[xs[offset], xs[offset + 1]],
                matches[offset],
                matches[offset + 1],
            )
        };
        let left = self.validate_table_2_with_pairs(pair(0)?, pair(2)?)?;
        let right = self.validate_table_2_with_pairs(pair(4)?, pair(6)?)?;
        if !self
            .core
            .validate_match_info_pairing(3, left.meta, left.match_info, right.match_info)
        {
            return None;
        }
        self.core
            .pairing_t3(left.meta, right.meta, left.x_bits, right.x_bits)
    }

    /// The whole proof. Returns the quality chain links when it holds.
    #[must_use]
    pub fn validate_full_proof(
        &self,
        proof: &[u32; TOTAL_XS_IN_PROOF],
        challenge: Bytes32,
    ) -> Option<QualityChainLinks> {
        let mut fragments = [0u64; NUM_CHAIN_LINKS];
        for (i, slot) in fragments.iter_mut().enumerate() {
            let mut xs = [0u32; 8];
            xs.copy_from_slice(&proof[i * 8..i * 8 + 8]);
            self.validate_table_3_pairs(&xs)?;
            *slot = self.core.fragment_codec.encode(&xs);
        }
        let sets = self.core.select_challenge_sets(challenge);
        if !Chainer::new(&self.core, challenge).validate(&Chain { fragments }, &sets.ranges) {
            return None;
        }
        Some(fragments)
    }

    pub fn validate_packed_proof(
        &self,
        proof: &[u8],
        challenge: Bytes32,
    ) -> Option<QualityChainLinks> {
        if proof.len() != TOTAL_XS_IN_PROOF * usize::from(self.core.params().k()) / 8 {
            return None;
        }
        let values = crate::bits::expand_bits(proof, self.core.params().k())?;
        self.validate_full_proof(&values.try_into().ok()?, challenge)
    }
}

#[cfg(test)]
#[path = "../tests/unit/validator/tests.rs"]
mod tests;
