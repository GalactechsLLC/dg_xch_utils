use super::*;

// Offset of challenge_vdf_output in both layouts: 32 (header_hash) + 32 (prev_hash) +
// 4 (height) + 16 (weight) + 16 (total_iters) + 1 (signage_point_index).
const VDF_OFFSET: usize = 101;

fn unhex(s: &str) -> Vec<u8> {
    assert!(s.len().is_multiple_of(2), "even-length hex");
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
        .collect()
}

/// Real mainnet record blobs (see core/tests/block_record_wire.rs for provenance).
fn goldens() -> Vec<Vec<u8>> {
    include_str!("../../../../core/tests/fixtures/block_record_mainnet_3000000.txt")
        .lines()
        .filter_map(|l| l.strip_prefix("RECORD "))
        .map(|h| unhex(h.trim()))
        .collect()
}

/// Rebuild the legacy blob for a wire-layout record byte-for-byte: the legacy encoder
/// wrote a u32-BE 0x64 length prefix ahead of each 100-byte VDF output and was otherwise
/// identical. Built structurally here, independent of the shipped legacy decoder, so the
/// compat path is proven against the layout itself and not against its own inverse.
fn legacy_blob_of(chia: &[u8]) -> Vec<u8> {
    const PREFIX: [u8; 4] = 100u32.to_be_bytes();
    let mut out = Vec::with_capacity(chia.len() + 8);
    out.extend_from_slice(&chia[..VDF_OFFSET]);
    out.extend_from_slice(&PREFIX);
    out.extend_from_slice(&chia[VDF_OFFSET..VDF_OFFSET + 100]);
    // Option tag for infused_challenge_vdf_output.
    let tag_at = VDF_OFFSET + 100;
    out.push(chia[tag_at]);
    let rest = if chia[tag_at] == 1 {
        out.extend_from_slice(&PREFIX);
        out.extend_from_slice(&chia[tag_at + 1..tag_at + 101]);
        &chia[tag_at + 101..]
    } else {
        &chia[tag_at + 1..]
    };
    out.extend_from_slice(rest);
    out
}

#[test]
fn chia_layout_blobs_decode_directly() {
    for blob in goldens() {
        let rec = decode_record(&blob).expect("chia-layout blob decodes");
        assert_eq!(rec.to_bytes(VERSION).expect("encode"), blob);
    }
}

#[test]
fn legacy_layout_blobs_decode_via_the_fallback() {
    for blob in goldens() {
        let legacy = legacy_blob_of(&blob);
        assert_ne!(legacy, blob, "legacy layout differs");
        let from_legacy = decode_record(&legacy).expect("legacy blob decodes");
        let from_chia = decode_record(&blob).expect("chia blob decodes");
        assert_eq!(from_legacy, from_chia, "both layouts land the same record");
        // A legacy record re-encodes in the wire layout — legacy blobs age out on rewrite.
        assert_eq!(from_legacy.to_bytes(VERSION).expect("encode"), blob);
    }
}

#[test]
fn garbage_blobs_error_on_both_paths() {
    assert!(decode_record(&[]).is_err());
    assert!(decode_record(&[0u8; 7]).is_err());
    // A truncated golden fails both exact-fit walks.
    let blob = goldens().remove(0);
    assert!(decode_record(&blob[..blob.len() - 1]).is_err());
    // Trailing garbage fails exact-fit on both paths.
    let mut extended = blob;
    extended.push(0);
    assert!(decode_record(&extended).is_err());
}
