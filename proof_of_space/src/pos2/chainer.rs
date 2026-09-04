use crate::pos2::constants::{
    CHAIN_FACTOR_FRONT_LOAD_BITS, CHAIN_SET_BITS, CHAIN_STARTER_FILTER_BITS, NUM_CHAIN_LINKS,
    NUM_CHALLENGE_SETS,
};
use crate::pos2::core::ProofCore;
use crate::pos2::fragment::ProofFragment;
use crate::pos2::params::Range;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use std::sync::LazyLock;

/// The fragments a quality chain is built from, one per link.
pub type QualityChainLinks = [ProofFragment; NUM_CHAIN_LINKS];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chain {
    pub fragments: QualityChainLinks,
}

/// Cancels the Jensen bonus that Poisson distributed set sizes give the chain search.
///
/// Each of the `NUM_CHALLENGE_SETS` sets is drawn from `NUM_CHAIN_LINKS / NUM_CHALLENGE_SETS`
/// times, and `E[|S|^reuse] > E[|S|]^reuse`, so without correction a challenge would yield about
/// 1.44 chains instead of the design target of one. The last link pays that back by requiring its
/// upper bits to fall below this threshold.
static LAST_LINK_EXTRA_THRESHOLD: LazyLock<u64> = LazyLock::new(last_link_extra_threshold);

fn last_link_extra_threshold() -> u64 {
    const REUSE: usize = NUM_CHAIN_LINKS / NUM_CHALLENGE_SETS;
    // Stirling numbers of the second kind, S(4, j) for j in 0..=4. Regenerate if the reuse changes.
    const STIRLING: [f64; REUSE + 1] = [0.0, 1.0, 7.0, 6.0, 1.0];
    const { assert!(REUSE == 4, "Stirling coefficients assume a reuse of four") };

    let lambda = f64::from(1u32 << CHAIN_SET_BITS);
    let mut expected = 0.0f64;
    let mut lambda_pow = 1.0f64;
    for s in STIRLING {
        expected += s * lambda_pow;
        lambda_pow *= lambda;
    }
    let mut lambda_pow_reuse = 1.0f64;
    for _ in 0..REUSE {
        lambda_pow_reuse *= lambda;
    }
    let ratio = expected / lambda_pow_reuse;
    let mut bonus = 1.0f64;
    for _ in 0..NUM_CHALLENGE_SETS {
        bonus *= ratio;
    }
    let upper_bits_count = 64 - CHAIN_SET_BITS - CHAIN_FACTOR_FRONT_LOAD_BITS;
    let max_upper = (2.0f64).powi(upper_bits_count as i32);
    (max_upper / bonus) as u64
}

/// Validates a quality chain.
///
/// A chain is a walk over proof fragments where each link's hash must land on enough zero bits.
/// The fragments are not free to come from anywhere: link `i` must be drawn from the challenge set
/// `start_set + i` modulo the set count, which is what ties a chain to the challenge.
pub struct Chainer<'a> {
    core: &'a ProofCore,
    challenge: Bytes32,
}

impl<'a> Chainer<'a> {
    #[must_use]
    pub fn new(core: &'a ProofCore, challenge: Bytes32) -> Self {
        Self { core, challenge }
    }

    /// How many low zero bits a link's hash owes. The first link uses the starter filter, the last
    /// one is deliberately harder to keep the expected chain count at one.
    #[must_use]
    pub fn zero_bits_needed(iteration: usize) -> u32 {
        if iteration == 0 {
            CHAIN_STARTER_FILTER_BITS
        } else if iteration == NUM_CHAIN_LINKS - 1 {
            CHAIN_SET_BITS + CHAIN_FACTOR_FRONT_LOAD_BITS
        } else {
            CHAIN_SET_BITS
        }
    }

    #[must_use]
    pub fn passes_fast_filter(fast_challenge: u64, iteration: usize) -> bool {
        let zeros = Self::zero_bits_needed(iteration);
        if fast_challenge & ((1u64 << zeros) - 1) != 0 {
            return false;
        }
        if iteration == NUM_CHAIN_LINKS - 1 {
            // The low bits are known zero, so the upper bits are still uniform and can carry the
            // fractional part of the correction.
            return (fast_challenge >> zeros) < *LAST_LINK_EXTRA_THRESHOLD;
        }
        true
    }

    #[must_use]
    pub fn validate(&self, chain: &Chain, ranges: &[Range; NUM_CHALLENGE_SETS]) -> bool {
        // The selected sets do not overlap, so the first fragment fixes where the chain started.
        let Some(start_set) = ranges.iter().position(|r| r.contains(chain.fragments[0])) else {
            return false;
        };
        for (i, fragment) in chain.fragments.iter().enumerate() {
            if !ranges[(start_set + i) % NUM_CHALLENGE_SETS].contains(*fragment) {
                return false;
            }
        }

        let round_keys = self
            .core
            .hashing
            .chaining_challenge_with_plot_id_hash(self.challenge);
        let mut challenge = 0u64;
        for (i, fragment) in chain.fragments.iter().enumerate() {
            challenge = self
                .core
                .hashing
                .chain_hash(challenge ^ fragment ^ round_keys[i]);
            if !Self::passes_fast_filter(challenge, i) {
                return false;
            }
        }
        true
    }
}

#[cfg(test)]
#[path = "../../tests/unit/pos2/chainer/tests.rs"]
mod tests;
