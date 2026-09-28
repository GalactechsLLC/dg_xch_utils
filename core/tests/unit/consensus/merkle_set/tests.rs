use super::*;

fn hx(s: &str) -> [u8; 32] {
    let v = hex::decode(s).expect("hex");
    let mut out = [0u8; 32];
    out.copy_from_slice(&v);
    out
}

fn h2(buf1: &[u8], buf2: &[u8]) -> [u8; 32] {
    let mut buf = buf1.to_vec();
    buf.extend_from_slice(buf2);
    hash_256(buf)
}

fn hashdown(types: &[u8; 2], buf2: &[u8], buf3: &[u8]) -> [u8; 32] {
    let mut buf = vec![0u8; 30];
    buf.extend_from_slice(types);
    buf.extend_from_slice(buf2);
    buf.extend_from_slice(buf3);
    hash_256(buf)
}

// Root corpus: duplicates, rotations, singles/pairs/triples/quads, and the special-shape
// trees, asserted against MerkleSet::from_leafs().get_root().
#[allow(clippy::many_single_char_names)]
fn merkle_set_test_cases() -> Vec<([u8; 32], Vec<[u8; 32]>)> {
    let a = hx("7000000000000000000000000000000000000000000000000000000000000000");
    let b = hx("7100000000000000000000000000000000000000000000000000000000000000");
    let c = hx("8000000000000000000000000000000000000000000000000000000000000000");
    let d = hx("8100000000000000000000000000000000000000000000000000000000000000");

    let root4 = hashdown(
        &[2, 2],
        &hashdown(&[1, 1], &a, &b),
        &hashdown(&[1, 1], &c, &d),
    );
    let root3 = hashdown(&[2, 1], &hashdown(&[1, 1], &a, &b), &c);

    // merkle_tree_5
    let e5 = hx("5800000000000000000000000000000000000000000000000000000000000000");
    let b5 = hx("2300000000000000000000000000000000000000000000000000000000000000");
    let c5 = hx("2100000000000000000000000000000000000000000000000000000000000000");
    let d5 = hx("ca00000000000000000000000000000000000000000000000000000000000000");
    let a5 = hx("2000000000000000000000000000000000000000000000000000000000000000");
    let mut expected5 = hashdown(&[1, 1], &a5, &c5);
    expected5 = hashdown(&[2, 1], &expected5, &b5);
    expected5 = hashdown(&[2, 0], &expected5, &BLANK);
    expected5 = hashdown(&[2, 0], &expected5, &BLANK);
    expected5 = hashdown(&[2, 0], &expected5, &BLANK);
    expected5 = hashdown(&[0, 2], &BLANK, &expected5);
    expected5 = hashdown(&[2, 1], &expected5, &e5);
    expected5 = hashdown(&[2, 1], &expected5, &d5);
    let tree5 = (expected5, vec![e5, b5, c5, d5, a5]);

    // merkle_tree_left_edge
    let la = hx("8000000000000000000000000000000000000000000000000000000000000000");
    let lb = hx("0000000000000000000000000000000000000000000000000000000000000001");
    let lc = hx("0000000000000000000000000000000000000000000000000000000000000002");
    let ld = hx("0000000000000000000000000000000000000000000000000000000000000003");
    let mut le = hashdown(&[1, 1], &lc, &ld);
    le = hashdown(&[1, 2], &lb, &le);
    for _ in 0..253 {
        le = hashdown(&[2, 0], &le, &BLANK);
    }
    le = hashdown(&[2, 1], &le, &la);
    let left_edge = (le, vec![la, lb, lc, ld]);
    let left_edge_dups = (le, vec![la, lb, lc, ld, la, lb, lc, ld]);

    // merkle_tree_right_edge
    let ra = hx("4000000000000000000000000000000000000000000000000000000000000000");
    let rb = hx("ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff");
    let rc = hx("fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffe");
    let rd = hx("fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffd");
    let mut re = hashdown(&[1, 1], &rc, &rb);
    re = hashdown(&[1, 2], &rd, &re);
    for _ in 0..253 {
        re = hashdown(&[0, 2], &BLANK, &re);
    }
    re = hashdown(&[1, 2], &ra, &re);
    let right_edge = (re, vec![ra, rb, rc, rd]);

    vec![
        (BLANK, vec![]),
        (h2(&[1u8], &a), vec![a, a]),
        (h2(&[1u8], &a), vec![a, a, a, a]),
        (root4, vec![a, b, c, d, a]),
        (root4, vec![b, c, d, a, a]),
        (root4, vec![c, d, a, b, a]),
        (root4, vec![d, a, b, c, a]),
        (root4, vec![d, c, b, a, a]),
        (root4, vec![c, b, a, d, a]),
        (root4, vec![b, a, d, c, a]),
        (root4, vec![a, d, c, b, a]),
        (root4, vec![c, a, d, b, a]),
        (h2(&[1u8], &a), vec![a]),
        (h2(&[1u8], &b), vec![b]),
        (h2(&[1u8], &c), vec![c]),
        (h2(&[1u8], &d), vec![d]),
        (hashdown(&[1, 1], &a, &b), vec![a, b]),
        (hashdown(&[1, 1], &a, &b), vec![b, a]),
        (hashdown(&[1, 1], &a, &c), vec![a, c]),
        (hashdown(&[1, 1], &a, &c), vec![c, a]),
        (hashdown(&[1, 1], &a, &d), vec![a, d]),
        (hashdown(&[1, 1], &a, &d), vec![d, a]),
        (hashdown(&[1, 1], &b, &c), vec![b, c]),
        (hashdown(&[1, 1], &b, &c), vec![c, b]),
        (hashdown(&[1, 1], &b, &d), vec![b, d]),
        (hashdown(&[1, 1], &b, &d), vec![d, b]),
        (hashdown(&[1, 1], &c, &d), vec![c, d]),
        (hashdown(&[1, 1], &c, &d), vec![d, c]),
        (root3, vec![a, b, c]),
        (root3, vec![a, c, b]),
        (root3, vec![b, a, c]),
        (root3, vec![b, c, a]),
        (root3, vec![c, a, b]),
        (root3, vec![c, b, a]),
        (root4, vec![a, b, c, d]),
        (root4, vec![b, c, d, a]),
        (root4, vec![c, d, a, b]),
        (root4, vec![d, a, b, c]),
        (root4, vec![d, c, b, a]),
        (root4, vec![c, b, a, d]),
        (root4, vec![b, a, d, c]),
        (root4, vec![a, d, c, b]),
        (root4, vec![c, a, d, b]),
        tree5,
        left_edge,
        left_edge_dups,
        right_edge,
    ]
}

#[test]
fn corpus_roots_and_proofs_round_trip() {
    for (root, leafs) in merkle_set_test_cases() {
        let tree = MerkleSet::from_leafs(&mut leafs.clone());
        assert_eq!(tree.get_root(), root);

        for item in &leafs {
            let (included, proof) = tree.generate_proof(item).expect("proof");
            assert!(included);
            let rebuilt = MerkleSet::from_proof(&proof).expect("parse proof");
            assert_eq!(rebuilt.get_root(), root);
            let (included, new_proof) = rebuilt.generate_proof(item).expect("re-proof");
            assert!(included);
            assert_eq!(new_proof, Vec::<u8>::new());
            assert!(validate_merkle_proof(&proof, item, &root).expect("validate"));
        }

        // deterministic exclusion probes (xorshift64) — never part of the corpus leaves
        let mut s: u64 = 0xDEAD_BEEF_CAFE_F00D;
        for _ in 0..20 {
            let mut item = [0u8; 32];
            for chunk in item.chunks_mut(8) {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                chunk.copy_from_slice(&s.to_be_bytes());
            }
            if leafs.contains(&item) {
                continue;
            }
            let (included, proof) = tree.generate_proof(&item).expect("proof");
            assert!(!included);
            let rebuilt = MerkleSet::from_proof(&proof).expect("parse proof");
            assert_eq!(rebuilt.get_root(), root);
            assert!(!validate_merkle_proof(&proof, &item, &root).expect("validate"));
        }
    }
}

#[test]
fn complete_proof_vector_is_stable() {
    let a = hx("c000000000000000000000000000000000000000000000000000000000000000");
    let b = hx("c800000000000000000000000000000000000000000000000000000000000000");
    let c = hx("7000000000000000000000000000000000000000000000000000000000000000");
    let expected = "0200020002020201c00000000000000000000000000000000000000000000000000000000000000001c8000000000000000000000000000000000000000000000000000000000000000000";

    let tree = MerkleSet::from_leafs(&mut [a, b]);
    let (included, proof) = tree.generate_proof(&b).expect("proof");
    assert!(included);
    assert_eq!(hex::encode(&proof), expected);

    let (included, proof) = tree.generate_proof(&a).expect("proof");
    assert!(included);
    assert_eq!(hex::encode(&proof), expected);

    // proofs of exclusion are also complete
    let (included, proof) = tree.generate_proof(&c).expect("proof");
    assert!(!included);
    assert_eq!(hex::encode(&proof), expected);
}

// A deep MIDDLE-only chain must error (depth bound), not exhaust memory or panic.
#[test]
fn malicious_middle_chain_is_rejected() {
    let malicious_proof = vec![MIDDLE; 40000];
    assert!(MerkleSet::from_proof(&malicious_proof).is_err());
}

// A TERMINAL leaf on the wrong side of its bit route fails the position audit.
#[test]
fn mispositioned_leaf_fails_the_audit() {
    let mut bad_proof: Vec<u8> = Vec::new();
    bad_proof.push(MIDDLE);
    bad_proof.push(TRUNCATED);
    bad_proof.extend_from_slice(&[0x11u8; 32]);
    bad_proof.push(MIDDLE);
    bad_proof.push(TERMINAL);
    // high bit set => belongs on the right, presented on the left
    bad_proof.extend_from_slice(&hx(
        "8000000000000000000000000000000000000000000000000000000000000000",
    ));
    bad_proof.push(TERMINAL);
    bad_proof.extend_from_slice(&[0x00u8; 32]);
    assert_eq!(MerkleSet::from_proof(&bad_proof), Err(SetError));
}

// Truncated / trailing byte streams error (never panic), plus arbitrary garbage prefixes.
#[test]
fn truncated_and_garbage_proofs_error_not_panic() {
    let a = hx("c000000000000000000000000000000000000000000000000000000000000000");
    let b = hx("c800000000000000000000000000000000000000000000000000000000000000");
    let tree = MerkleSet::from_leafs(&mut [a, b]);
    let (_, proof) = tree.generate_proof(&a).expect("proof");
    // every proper prefix must fail (incomplete parse)
    for cut in 0..proof.len() {
        assert!(MerkleSet::from_proof(&proof[..cut]).is_err(), "cut {cut}");
    }
    // trailing garbage must fail (bytes left over)
    let mut extended = proof.clone();
    extended.push(0);
    assert!(MerkleSet::from_proof(&extended).is_err());
    // deterministic garbage streams
    let mut s: u64 = 0x1234_5678_9ABC_DEF0;
    for len in [1usize, 2, 7, 33, 64, 129] {
        let mut bytes = Vec::with_capacity(len);
        while bytes.len() < len {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            bytes.extend_from_slice(&s.to_be_bytes());
        }
        bytes.truncate(len);
        // must return (parse either way is fine only for a structurally valid stream; the
        // point is: no panic). Err is the overwhelmingly likely outcome.
        let _ = MerkleSet::from_proof(&bytes);
    }
}

// The proof-capable tree agrees bit-for-bit with the producer/validator-side collapsed root
// (block_generator.rs::merkle_set_root via canonical_removals_root) — one encoding, two
// implementations, zero drift.
#[cfg(feature = "bls")]
#[test]
fn root_matches_the_producer_side_merkle_set_root() {
    use crate::blockchain::sized_bytes::Bytes32;
    use crate::consensus::block_generator::canonical_removals_root;
    use crate::traits::SizedBytes;
    for (_, leafs) in merkle_set_test_cases() {
        let via_producer =
            canonical_removals_root(&leafs.iter().map(|l| Bytes32::new(*l)).collect::<Vec<_>>());
        let via_tree = MerkleSet::from_leafs(&mut leafs.clone()).get_root();
        assert_eq!(via_producer.bytes(), via_tree);
    }
}
