use crate::pos2::chainer::{Chain, Chainer, QualityChainLinks};
use crate::pos2::constants::{NUM_CHAIN_LINKS, TOTAL_XS_IN_PROOF};
use crate::pos2::core::{ProofCore, T1Pairing, T2Pairing, T3Pairing};
use crate::pos2::params::ProofParams;
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
        let left = self.validate_table_2_pairs(&[xs[0], xs[1], xs[2], xs[3]])?;
        let right = self.validate_table_2_pairs(&[xs[4], xs[5], xs[6], xs[7]])?;
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
}

#[cfg(test)]
#[path = "../../tests/unit/pos2/validator/tests.rs"]
mod tests;
