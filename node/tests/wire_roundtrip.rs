// Wire round-trip + hostile-decoder conformance (test class 2, reconstructed design). The
// streamable surface has no property/fuzz coverage anywhere in the workspace: nothing owes
// "decode(encode(x)) == x" beyond hand-picked unit values, and nothing at all owes "a hostile
// buffer errors instead of panicking or over-allocating". This file establishes both as a test
// surface over the REAL mainnet corpus the repo already commits (the strongest available
// generator — every branch a synthetic value would miss, a real 14 MB weight proof hits):
//
// 1. byte-identity: decode -> encode of the committed corpus blobs reproduces the exact source
//    bytes (the producer-differential discipline of c371f9b applied to the codec itself);
// 2. truncation: every strict prefix of a valid encoding must Err (never panic, never Ok);
// 3. mutation: a single flipped byte anywhere may change the value but must never panic;
// 4. garbage: seeded-random buffers must Err deterministically, never panic;
// 5. hostile lengths: over-claimed String/Vec length prefixes and invalid Option/bool tags
//    must Err fast (the Vec path is fail-fast by element; the String path pre-allocates the
//    claimed length before reading -- see the class report -- so the pin here is Err-not-panic
//    on a bounded over-claim).

mod common;

use dg_xch_core::blockchain::block_record::BlockRecord;
use dg_xch_core::blockchain::full_block::FullBlock;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::blockchain::weight_proof::{RecentChainData, WeightProof};
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use std::io::Cursor;
use std::panic::{AssertUnwindSafe, catch_unwind};

fn version() -> ChiaProtocolVersion {
    ChiaProtocolVersion::default()
}

const RECENT_CHAIN_96: &[u8] = include_bytes!("fixtures/recent_chain_mainnet_9054524_9054620.bin");
const WEIGHT_PROOF: &[u8] =
    include_bytes!("../../weight-proof/tests/fixtures/weight_proof_mainnet_9054698.bin");

// Deterministic xorshift64* — no new dependency; a failing case reproduces from the fixed seed.
struct XorShift(u64);
impl XorShift {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
}

// 1 — The committed corpus round-trips byte-identical: RecentChainData (96 real headers) and the
// full 14 MB mainnet weight proof decode with exact-fit framing (from_bytes_exact: every byte
// consumed) and re-encode to the exact source bytes. JSON-sourced fixtures (no original network
// bytes) pin encode->decode->encode fixed-point plus structural equality instead.
#[test]
fn real_corpus_round_trips_byte_identical() {
    let chain = RecentChainData::from_bytes_exact(RECENT_CHAIN_96, version())
        .expect("recent-chain blob decodes with exact-fit framing");
    assert!(chain.recent_chain_data.len() > 80, "real slice present");
    assert_eq!(
        chain.to_bytes(version()).expect("re-encode"),
        RECENT_CHAIN_96,
        "RecentChainData re-encodes to the exact source bytes"
    );

    let wp = WeightProof::from_bytes_exact(WEIGHT_PROOF, version())
        .expect("weight proof decodes with exact-fit framing");
    assert_eq!(
        wp.to_bytes(version()).expect("re-encode"),
        WEIGHT_PROOF,
        "the 14 MB weight proof re-encodes to the exact source bytes"
    );

    for height in [5_000_000u32, 5_000_004] {
        let block = common::load_full_block(height);
        let e1 = block.to_bytes(version()).expect("encode");
        let back = FullBlock::from_bytes_exact(&e1, version()).expect("decode");
        assert_eq!(back, block, "FullBlock {height} structural round-trip");
        let e2 = back.to_bytes(version()).expect("re-encode");
        assert_eq!(e1, e2, "FullBlock {height} byte fixed-point");
    }

    for (i, record) in common::load_records().iter().enumerate() {
        let e1 = record.to_bytes(version()).expect("encode");
        let back = BlockRecord::from_bytes_exact(&e1, version()).expect("decode");
        assert_eq!(&back, record, "BlockRecord[{i}] structural round-trip");
        assert_eq!(
            back.to_bytes(version()).expect("re-encode"),
            e1,
            "BlockRecord[{i}] byte fixed-point"
        );
    }

    // Synthetic Option/SES variants (the sweep chain covers Some/None branches the fixtures may
    // not): every record in a boundary-spanning synthetic chain round-trips.
    let blocks = common::sweep::chain(4_575_000, 4_575_800, u32::MAX);
    for record in blocks.values() {
        let e1 = record.to_bytes(version()).expect("encode");
        let back = BlockRecord::from_bytes_exact(&e1, version()).expect("decode");
        assert_eq!(
            &back, record,
            "synthetic record round-trip @{}",
            record.height
        );
    }
}

// 2 — Every strict prefix of a valid encoding errors — never panics, never Ok. A prefix that
// decoded Ok would be a framing hole (a message boundary the decoder cannot see).
#[test]
fn truncated_corpus_always_errors_never_panics() {
    let len = RECENT_CHAIN_96.len();
    let mut cuts: Vec<usize> = (0..8).collect();
    cuts.extend((1..=256).map(|i| len - i));
    cuts.extend((8..len).step_by(4093));
    for cut in cuts {
        let prefix = &RECENT_CHAIN_96[..cut];
        let out = catch_unwind(AssertUnwindSafe(|| {
            RecentChainData::from_bytes_exact(prefix, version())
        }))
        .unwrap_or_else(|_| panic!("decoder PANICKED on truncation at {cut}/{len}"));
        assert!(out.is_err(), "truncation at {cut}/{len} must error, got Ok");
    }
}

// 3 — A single flipped byte may decode to a different value or error, but must never panic: the
// decoder's failure mode on corruption is Err, not process death. Sweeps the front densely (all
// framing-critical territory) and the rest sampled.
#[test]
fn mutated_corpus_never_panics() {
    let base = common::load_full_block(5_000_000)
        .to_bytes(version())
        .expect("encode");
    let mut positions: Vec<usize> = (0..base.len().min(512)).collect();
    positions.extend((512..base.len()).step_by(1031));
    let mut buf = base.clone();
    for pos in positions {
        buf[pos] ^= 0xff;
        let out = catch_unwind(AssertUnwindSafe(|| {
            FullBlock::from_bytes_exact(&buf, version())
        }));
        assert!(
            out.is_ok(),
            "decoder PANICKED on a flipped byte at {pos}/{}",
            base.len()
        );
        buf[pos] = base[pos];
    }
}

// 4 — Seeded-random garbage errors deterministically for every compound type — never panics,
// never Ok (with this fixed seed; a failing case reproduces exactly).
#[test]
fn random_garbage_always_errors_never_panics() {
    let mut rng = XorShift(0x5eed_1777_c1a5_5002);
    for case in 0..256u32 {
        let len = (rng.next() % 4096) as usize;
        let buf: Vec<u8> = (0..len).map(|_| (rng.next() & 0xff) as u8).collect();
        macro_rules! must_err {
            ($ty:ty) => {
                let out = catch_unwind(AssertUnwindSafe(|| {
                    <$ty>::from_bytes_exact(&buf, version())
                }))
                .unwrap_or_else(|_| {
                    panic!(
                        "{} PANICKED on garbage case {case} (len {len})",
                        stringify!($ty)
                    )
                });
                assert!(
                    out.is_err(),
                    "{} decoded garbage case {case} (len {len}) as Ok",
                    stringify!($ty)
                );
            };
        }
        must_err!(FullBlock);
        must_err!(BlockRecord);
        must_err!(RecentChainData);
        must_err!(WeightProof);
    }
}

// 5 — Hostile framing values fail closed: an over-claimed String length errors (Err, not panic —
// the claimed 64 MiB is bounded here because the String path pre-allocates the claim before
// reading; the Vec path is fail-fast by element and errors on the first missing element), and the
// strict tags (Option in {0,1}, bool in {0,1}) reject everything else.
#[test]
fn hostile_lengths_and_tags_error_fast() {
    // String claiming 64 MiB with no payload.
    let mut hostile = 0x0400_0000u32.to_be_bytes().to_vec();
    hostile.extend([0u8; 8]);
    assert!(
        String::from_bytes(&mut Cursor::new(&hostile[..]), version()).is_err(),
        "over-claimed String length must error"
    );

    // Vec<Bytes32> claiming 16M elements with no payload: fail-fast on the first element.
    let hostile = 0x00ff_ffffu32.to_be_bytes().to_vec();
    assert!(
        Vec::<Bytes32>::from_bytes(&mut Cursor::new(&hostile[..]), version()).is_err(),
        "over-claimed Vec length must fail on the first missing element"
    );

    // The Option presence tag must be exactly 0 or 1.
    for tag in [2u8, 0x80, 0xff] {
        let buf = [tag, 0, 0, 0];
        assert!(
            Option::<u8>::from_bytes(&mut Cursor::new(&buf[..]), version()).is_err(),
            "Option tag {tag:#x} must be rejected"
        );
        assert!(
            bool::from_bytes(&mut Cursor::new(&buf[..]), version()).is_err(),
            "bool {tag:#x} must be rejected"
        );
    }
}
