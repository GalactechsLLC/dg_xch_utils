//! Per-entity RNG substreams for a simulation run. The ChaCha20 key comes from `(run_seed, domain)`
//! through BLAKE3 rather than `SeedableRng::seed_from_u64`, whose mixing is unspecified and may
//! change across `rand` releases; the entity index selects the ChaCha20 stream, so substreams
//! within a domain are disjoint rather than probabilistically distinct.

use rand_chacha::ChaCha20Rng;
use rand_chacha::rand_core::SeedableRng;

/// Framing tag for the key pre-image. Changing it renumbers every substream, hence the version
/// suffix.
const SUBSTREAM_DOMAIN_SEP: &[u8] = b"dg_xch_simulator/rng/v1";

/// The independent entropy sources in a run. One per consumer, so a change in how many samples one
/// draws cannot shift another's results. The names are mixed into the key: renaming a variant
/// renumbers its substream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Domain {
    /// Which farmer wins a signage point.
    PoSpace,
    /// Proof quality, which sets required iters.
    Quality,
    /// Per-link block propagation delay.
    NetLink,
    /// Transaction arrival and mempool composition.
    Mempool,
    /// Fork selection when a reorg is seeded.
    Reorg,
    /// Timelord speed jitter, churn, and drops.
    Timelord,
}

impl Domain {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Domain::PoSpace => "pospace",
            Domain::Quality => "quality",
            Domain::NetLink => "netlink",
            Domain::Mempool => "mempool",
            Domain::Reorg => "reorg",
            Domain::Timelord => "timelord",
        }
    }

    pub const ALL: [Domain; 6] = [
        Domain::PoSpace,
        Domain::Quality,
        Domain::NetLink,
        Domain::Mempool,
        Domain::Reorg,
        Domain::Timelord,
    ];
}

/// The ChaCha20 key for `(run_seed, domain)`. The domain name is length-prefixed so no two pairs
/// share a pre-image by sliding the boundary between the fields.
fn substream_key(run_seed: u64, domain: Domain) -> [u8; 32] {
    let name = domain.as_str().as_bytes();
    let mut hasher = blake3::Hasher::new();
    hasher.update(SUBSTREAM_DOMAIN_SEP);
    hasher.update(&(name.len() as u32).to_le_bytes());
    hasher.update(name);
    hasher.update(&run_seed.to_le_bytes());
    *hasher.finalize().as_bytes()
}

/// The substream for one entity. `index` is the entity's stable ordinal (farmer, link, timelord),
/// not a call counter: the same triple always returns a generator at the same keystream position.
#[must_use]
pub fn derive_rng(run_seed: u64, domain: Domain, index: u64) -> ChaCha20Rng {
    let mut rng = ChaCha20Rng::from_seed(substream_key(run_seed, domain));
    rng.set_stream(index);
    rng.set_word_pos(0);
    rng
}

#[cfg(test)]
#[path = "../../tests/unit/stats/rng/tests.rs"]
mod tests;
