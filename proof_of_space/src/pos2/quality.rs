use crate::pos2::chainer::QualityChainLinks;
use crate::pos2::constants::NUM_CHAIN_LINKS;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::traits::SizedBytes;
use dg_xch_core::utils::hash_256;

/// The quality commitment a v2 proof is hashed under: one byte of strength, then each chain link
/// little endian. The full proof is the witness to this commitment, which is why a v2
/// `ProofOfSpace` hashes this rather than its proof bytes.
#[must_use]
pub fn serialize_quality(
    fragments: &QualityChainLinks,
    strength: u8,
) -> [u8; NUM_CHAIN_LINKS * 8 + 1] {
    let mut out = [0u8; NUM_CHAIN_LINKS * 8 + 1];
    out[0] = strength;
    for (i, fragment) in fragments.iter().enumerate() {
        out[1 + i * 8..1 + (i + 1) * 8].copy_from_slice(&fragment.to_le_bytes());
    }
    out
}

/// The consensus quality string: the hash of the serialized quality commitment.
#[must_use]
pub fn quality_hash(fragments: &QualityChainLinks, strength: u8) -> Bytes32 {
    Bytes32::new(hash_256(serialize_quality(fragments, strength)))
}

#[cfg(test)]
#[path = "../../tests/unit/pos2/quality/tests.rs"]
mod tests;
