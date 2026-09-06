use super::*;

#[test]
fn decode_round_trips_and_matches() {
    let items: Vec<Vec<u8>> = (0u8..100).map(|i| vec![i; 32]).collect();
    let filter = chia_block_filter(&items);
    let decoded = decode_chia_block_filter(&filter).expect("well-formed filter decodes");
    assert_eq!(decoded.len(), 100, "N survives the round trip");
    for item in &items {
        assert!(
            chia_block_filter_match(&decoded, item),
            "every encoded member matches"
        );
    }
    // A non-member misses (false-positive odds 1 in M = 2^20 per probe).
    assert!(!chia_block_filter_match(&decoded, &[0xAB; 33]));
}

#[test]
fn decode_is_defensive_on_garbage() {
    // truncated CompactSize
    assert!(decode_chia_block_filter(&[0xfd]).is_none());
    // empty filter: the single zero byte
    assert_eq!(decode_chia_block_filter(&[0]), Some(Vec::new()));
    assert!(decode_chia_block_filter(&[]).is_none());
    // element count with a body too short to carry it
    assert!(decode_chia_block_filter(&[5, 0x01]).is_none());
}
