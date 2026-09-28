use super::*;
use crate::clvm::parser::{is_canonical_serialization, sexp_from_bytes_backrefs};
use crate::clvm::sexp::{AtomBuf, PairBuf};
use std::sync::Arc;

fn atom(bytes: &[u8]) -> SExp<'static> {
    SExp::Atom(AtomBuf::Owned(Arc::new(bytes.to_vec())))
}
fn cons(first: SExp<'static>, rest: SExp<'static>) -> SExp<'static> {
    SExp::Pair(PairBuf::Owned((Arc::new(first), Arc::new(rest))))
}

#[test]
fn repeated_subtrees_use_short_backrefs() {
    let leaf = atom(&[1, 2, 3, 4, 5]);
    let l1 = cons(leaf.clone(), leaf.clone());
    let l2 = cons(l1.clone(), l1.clone());
    let l3 = cons(l2.clone(), l2);
    let out = sexp_to_bytes_backrefs(&l3).expect("serialize");
    assert_eq!(
        out.as_ref(),
        &[255, 255, 255, 133, 1, 2, 3, 4, 5, 254, 2, 254, 2, 254, 2],
        "the repeated sibling must use the shortest path encoding"
    );
}

// The compressed encoding must round-trip through the back-reference DECODER to the identical
// tree, and be canonical.
#[test]
fn serialize_backrefs_round_trips_through_decoder() {
    let leaf = atom(&[9u8; 8]);
    let l1 = cons(leaf.clone(), leaf.clone());
    let l2 = cons(l1.clone(), l1.clone());
    let l3 = cons(l2.clone(), l2);
    let compressed = sexp_to_bytes_backrefs(&l3).expect("serialize");
    assert!(
        is_canonical_serialization(compressed.as_ref()),
        "back-ref serialization must be canonical"
    );
    let decoded = sexp_from_bytes_backrefs(&mut Cursor::new(compressed.as_ref())).expect("decode");
    assert_eq!(decoded, l3, "compressed must decode to the identical tree");
    assert_eq!(decoded.tree_hash(), l3.tree_hash());
}

// No repeated ≥4-byte subtree ⇒ compression is a byte-for-byte no-op: the back-ref encoder
// agrees with the plain encoder.
#[test]
fn serialize_backrefs_matches_plain_when_no_repeats() {
    use crate::clvm::parser::sexp_to_bytes;
    let tree = cons(
        atom(&[0xaa; 32]),
        cons(atom(&[0xbb; 31]), cons(atom(&[1]), atom(&[]))),
    );
    let plain = sexp_to_bytes(&tree).expect("plain");
    let compressed = sexp_to_bytes_backrefs(&tree).expect("compressed");
    assert_eq!(
        compressed.as_ref(),
        plain.as_ref(),
        "no repeats ⇒ back-ref encoder equals plain encoder"
    );
}

// The compressed form must never be larger than the plain form, and must always round-trip.
#[test]
fn serialize_backrefs_never_larger_and_round_trips_fuzz() {
    use crate::clvm::parser::sexp_to_bytes;
    let mut state: u32 = 0x2b3c_4d5e;
    let mut rng = || {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        state
    };
    for _ in 0..500 {
        // build a small random tree that deliberately reuses a couple of leaf atoms
        let shared_a = atom(&[(rng() as u8); 6]);
        let shared_b = atom(&[(rng() as u8); 10]);
        let mut node = atom(&[]);
        for _ in 0..(rng() % 12) {
            let pick = rng() % 4;
            let leaf = match pick {
                0 => shared_a.clone(),
                1 => shared_b.clone(),
                2 => atom(&(rng().to_le_bytes())),
                _ => cons(shared_a.clone(), shared_b.clone()),
            };
            node = cons(leaf, node);
        }
        let plain = sexp_to_bytes(&node).expect("plain");
        let compressed = sexp_to_bytes_backrefs(&node).expect("compressed");
        assert!(
            compressed.as_ref().len() <= plain.as_ref().len(),
            "compressed must never exceed plain"
        );
        let decoded =
            sexp_from_bytes_backrefs(&mut Cursor::new(compressed.as_ref())).expect("decode");
        assert_eq!(decoded, node, "compressed must round-trip to the same tree");
    }
}
