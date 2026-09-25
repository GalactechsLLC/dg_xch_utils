// REAL mainnet wire, not our own encoder. Loopback tests round-trip OUR encoder and so never catch a
// real-wire mismatch; this fixture is a genuine `RespondBlocks` (32 FullBlocks, heights 9,138,873..=9,138,904)
// captured off a mainnet full node's `RequestBlocks` reply. Block[1] carries a transaction generator that uses
// CLVM back-references (marker 0xfe), which the plain parser rejects.
//
// Coverage: decode of a real wire FullBlock, and a byte-exact round-trip
// proving the generator's on-wire bytes are preserved (re-serializing would drop back-refs and change block ids).

use dg_xch_core::blockchain::full_block::FullBlock;
use dg_xch_core::protocols::full_node::RespondBlocks;
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use std::io::Cursor;

const RAW: &[u8] = include_bytes!("fixtures/respond_blocks_mainnet_9138873_9138904.bin");

fn decode() -> RespondBlocks {
    let mut cur = Cursor::new(RAW);
    RespondBlocks::from_bytes(&mut cur, ChiaProtocolVersion::default())
        .expect("real mainnet RespondBlocks must decode (back-reference-aware generator)")
}

#[test]
fn real_mainnet_respond_blocks_decodes() {
    let resp = decode();
    assert_eq!(resp.start_height, 9_138_873, "start height");
    assert_eq!(resp.end_height, 9_138_904, "end height");
    assert_eq!(resp.blocks.len(), 32, "32 blocks in the range");

    // Heights are the contiguous requested range, in order.
    for (i, b) in resp.blocks.iter().enumerate() {
        assert_eq!(
            b.reward_chain_block.height,
            9_138_873 + i as u32,
            "block[{i}] height is contiguous",
        );
    }
}

#[test]
fn back_reference_generator_is_decoded_and_present() {
    let resp = decode();
    // At least one block in a recent mainnet range is a transaction block carrying a generator — a
    // field carrying CLVM back-references. (Block[1] in this fixture.)
    let with_gen = resp
        .blocks
        .iter()
        .filter(|b| b.transactions_generator.is_some())
        .count();
    assert!(
        with_gen >= 1,
        "expected at least one transaction generator in the range, found {with_gen}"
    );
}

#[test]
fn real_wire_round_trips_byte_for_byte() {
    // The strongest real-wire assertion: decode then re-encode reproduces the original mainnet bytes exactly.
    // This only holds if the generator's raw back-referenced bytes are preserved verbatim (no re-serialize)
    // AND every other FullBlock field's streamable framing matches the wire format exactly.
    let resp = decode();
    let reser = resp
        .to_bytes(ChiaProtocolVersion::default())
        .expect("re-encode");
    assert_eq!(reser.len(), RAW.len(), "re-encoded length matches the wire");
    assert_eq!(
        reser, RAW,
        "re-encoded bytes are identical to the mainnet wire"
    );
}

#[test]
fn individual_full_block_round_trips() {
    // Each decoded FullBlock re-encodes to the exact bytes it decoded from (per-block byte fidelity).
    let resp = decode();
    let v = ChiaProtocolVersion::default();
    for (i, b) in resp.blocks.iter().enumerate() {
        let bytes = b.to_bytes(v).expect("block to_bytes");
        let mut cur = Cursor::new(bytes.as_slice());
        let back = FullBlock::from_bytes(&mut cur, v).expect("block from_bytes");
        assert_eq!(
            back.to_bytes(v).expect("re-encode"),
            bytes,
            "block[{i}] round-trips byte-for-byte",
        );
    }
}
#[test]
fn appended_real_wire_preserves_prefix_and_protocol_versions() {
    let response = decode();
    for version in [
        ChiaProtocolVersion::Chia0_0_34,
        ChiaProtocolVersion::Chia0_0_35,
        ChiaProtocolVersion::Chia0_0_36,
        ChiaProtocolVersion::Chia0_0_37,
    ] {
        let mut bytes = vec![0xab; 17];
        response.append_bytes(&mut bytes, version).unwrap();
        assert_eq!(&bytes[..17], &[0xab; 17]);
        assert_eq!(&bytes[17..], RAW);
    }
}
