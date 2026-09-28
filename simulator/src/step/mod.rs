use crate::stats::{Domain, derive_rng};
use rand_chacha::rand_core::Rng;

/// Block timestamps for a simulated chain.
///
/// Elapsed time accumulates as `f64` and is quantized to whole seconds only in [`Self::emit`],
/// which carries the sub-second remainder forward so a long run does not drift by the rounding
/// error of every block it produced. Emissions are anchored to the previously emitted timestamp
/// and are strictly increasing, which is what the consensus rules require of transaction blocks.
#[derive(Debug, Clone, PartialEq)]
pub struct TimestampEmitter {
    anchor: u64,
    offset: f64,
}

impl TimestampEmitter {
    #[must_use]
    pub fn new(genesis_timestamp: u64) -> Self {
        Self {
            anchor: genesis_timestamp,
            offset: 0.0,
        }
    }

    /// Accumulate `seconds` of chain time without emitting.
    pub fn advance(&mut self, seconds: f64) {
        self.offset += seconds;
    }

    /// The last emitted timestamp.
    #[must_use]
    pub fn anchor(&self) -> u64 {
        self.anchor
    }

    /// Emit the next timestamp, re-anchoring to it. A step of zero whole seconds is emitted as one
    /// second and borrowed from the accumulator, so the debt is repaid by a later block rather than
    /// silently inflating the chain's clock.
    pub fn emit(&mut self) -> u64 {
        let whole = self.offset.floor();
        let step = if whole >= 1.0 { whole as u64 } else { 1 };
        self.offset -= step as f64;
        self.anchor += step;
        self.anchor
    }
}

/// The seed for the `fork_ordinal`-th reorg of a run. Always distinct from `run_seed`: a fork
/// seeded identically to the mainline replays it block for block and never diverges.
#[must_use]
pub fn reorg_seed(run_seed: u64, fork_ordinal: u64) -> u64 {
    let mut rng = derive_rng(run_seed, Domain::Reorg, fork_ordinal);
    let mut seed = rng.next_u64();
    while seed == run_seed {
        seed = rng.next_u64();
    }
    seed
}

#[cfg(test)]
#[path = "../../tests/unit/step/mod/tests.rs"]
mod tests;
