use super::chia_block_filter;
use crate::utils::hash_256;

fn sha256(bytes: &[u8]) -> Vec<u8> {
    hash_256(bytes).to_vec()
}

// Reference vector: the filter over sha256("abc"), sha256("xyz"), sha256("123") encodes
// to [3, 174, 90, 204, 224, 219, 7, 253, 91]. The leading 3 is the compact-size N; this
// pins P, M, the zero siphash key, map-into-range and the MSB-first bit writer at once.
#[test]
fn chia_block_filter_matches_chiabip158_vector() {
    let items = vec![sha256(b"abc"), sha256(b"xyz"), sha256(b"123")];
    assert_eq!(
        chia_block_filter(&items),
        vec![3u8, 174, 90, 204, 224, 219, 7, 253, 91],
        "must match chiabip158 rust-bindings test_filter vector byte-for-byte"
    );
}

// Genesis / no-tx-content: the empty element set encodes to [0], so
// filter_hash == sha256([0]).
#[test]
fn empty_filter_is_single_zero_byte() {
    assert_eq!(chia_block_filter(&[]), vec![0u8]);
    assert_eq!(hash_256(chia_block_filter(&[])), hash_256(vec![0u8]));
}

// Duplicate raw elements collapse: the N prefix counts distinct elements, so a filter
// over [x, x] equals the filter over [x].
#[test]
fn duplicate_elements_are_deduplicated() {
    let x = sha256(b"dup");
    let once = chia_block_filter(std::slice::from_ref(&x));
    let twice = chia_block_filter(&[x.clone(), x]);
    assert_eq!(
        once, twice,
        "duplicate elements must collapse to one (N distinct)"
    );
    assert_eq!(once[0], 1, "N == 1 distinct element");
}

// Element order does not change the encoding: the hashed values are sorted before
// Golomb-Rice encoding, so a permutation of the same set yields identical bytes.
#[test]
fn element_order_does_not_matter() {
    let a = sha256(b"abc");
    let b = sha256(b"xyz");
    let c = sha256(b"123");
    let forward = chia_block_filter(&[a.clone(), b.clone(), c.clone()]);
    let shuffled = chia_block_filter(&[c, a, b]);
    assert_eq!(forward, shuffled);
}
