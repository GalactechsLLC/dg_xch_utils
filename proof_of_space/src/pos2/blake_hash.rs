use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::traits::SizedBytes;

/// The single block hash pos2 uses for challenge derivation.
///
/// One BLAKE3 compression over a 64 byte block emitting `state[i] ^ state[i + 8]` is exactly
/// BLAKE3's root output for a 64 byte input, so this defers to the BLAKE3 crate rather than
/// carrying a second copy of the permutation. The vectors in the tests pin the little endian word
/// packing.
#[must_use]
pub fn hash_block_256(block_words: &[u32; 16]) -> [u32; 8] {
    let mut bytes = [0u8; 64];
    for (word, chunk) in block_words.iter().zip(bytes.as_chunks_mut::<4>().0) {
        chunk.copy_from_slice(&word.to_le_bytes());
    }
    let digest = blake3::hash(&bytes);
    let digest = digest.as_bytes();
    std::array::from_fn(|i| {
        let mut word = [0u8; 4];
        word.copy_from_slice(&digest[i * 4..i * 4 + 4]);
        u32::from_le_bytes(word)
    })
}

#[must_use]
pub fn hash_block_64(block_words: &[u32; 16]) -> [u32; 2] {
    let full = hash_block_256(block_words);
    [full[0], full[1]]
}

/// Pack a 32 byte value into the low eight words of a block, little endian per word.
#[must_use]
pub fn words_from_bytes32(value: Bytes32) -> [u32; 8] {
    let bytes = value.bytes();
    std::array::from_fn(|i| {
        let mut word = [0u8; 4];
        word.copy_from_slice(&bytes[i * 4..i * 4 + 4]);
        u32::from_le_bytes(word)
    })
}

/// A block holding the plot id in its low half and eight caller supplied words in its high half.
#[must_use]
pub fn block_with_plot_id(plot_id: Bytes32, data: &[u32; 8]) -> [u32; 16] {
    let head = words_from_bytes32(plot_id);
    std::array::from_fn(|i| if i < 8 { head[i] } else { data[i - 8] })
}

#[cfg(test)]
#[path = "../../tests/unit/pos2/blake_hash/tests.rs"]
mod tests;
