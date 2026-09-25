use super::{hash_coin_ids, merkle_set_root};
use crate::traits::SizedBytes;

// hashdown(buf) = sha256(bytes([0] * 30) + buf)
fn hashdown(buf: &[u8]) -> [u8; 32] {
    let mut input = vec![0u8; 30];
    input.extend_from_slice(buf);
    super::hash_256(input)
}

fn leaf(first: u8) -> [u8; 32] {
    let mut v = [0u8; 32];
    v[0] = first;
    v
}

// the empty set roots to all-zero bytes32
#[test]
fn empty_set_root_is_zero() {
    assert_eq!(merkle_set_root(vec![]).bytes(), [0u8; 32]);
}

// a single leaf roots to sha256(b"\1" + key)
#[test]
fn single_leaf_root() {
    let a = leaf(0x80);
    let mut buf = vec![1u8];
    buf.extend_from_slice(&a);
    assert_eq!(merkle_set_root(vec![a]).bytes(), super::hash_256(buf));
}

// duplicates collapse to the single-leaf root
#[test]
fn duplicate_leaf_root() {
    let a = leaf(0x80);
    let mut buf = vec![1u8];
    buf.extend_from_slice(&a);
    assert_eq!(merkle_set_root(vec![a, a]).bytes(), super::hash_256(buf));
}

// two leaves -> hashdown(\1\1 + b + a) (b's bit clear -> left; order-independent)
#[test]
fn two_leaf_root() {
    let a = leaf(0x80);
    let b = leaf(0x70);
    let mut buf = vec![1u8, 1u8];
    buf.extend_from_slice(&b);
    buf.extend_from_slice(&a);
    let expected = hashdown(&buf);
    assert_eq!(merkle_set_root(vec![a, b]).bytes(), expected);
    assert_eq!(merkle_set_root(vec![b, a]).bytes(), expected);
}

// hashdown(\2\1 + hashdown(\1\1 + b + c) + a) — a MiddleDbl {b,c} collapses through
// the shared-bit levels rather than emitting empty children.
#[test]
fn three_leaf_root_collapses_pair() {
    let a = leaf(0x80);
    let b = leaf(0x70);
    let c = leaf(0x71);
    let mut bc = vec![1u8, 1u8];
    bc.extend_from_slice(&b);
    bc.extend_from_slice(&c);
    let bc = hashdown(&bc);
    let mut top = vec![2u8, 1u8];
    top.extend_from_slice(&bc);
    top.extend_from_slice(&a);
    let expected = hashdown(&top);
    assert_eq!(merkle_set_root(vec![a, b, c]).bytes(), expected);
    assert_eq!(merkle_set_root(vec![c, b, a]).bytes(), expected);
}

// two MiddleDbl subtrees -> hashdown(\2\2 + hashdown(\1\1+b+c) + hashdown(\1\1+a+d))
#[test]
fn four_leaf_root() {
    let a = leaf(0x80);
    let b = leaf(0x70);
    let c = leaf(0x71);
    let d = leaf(0x81);
    let mut bc = vec![1u8, 1u8];
    bc.extend_from_slice(&b);
    bc.extend_from_slice(&c);
    let bc = hashdown(&bc);
    let mut ad = vec![1u8, 1u8];
    ad.extend_from_slice(&a);
    ad.extend_from_slice(&d);
    let ad = hashdown(&ad);
    let mut top = vec![2u8, 2u8];
    top.extend_from_slice(&bc);
    top.extend_from_slice(&ad);
    assert_eq!(merkle_set_root(vec![a, b, c, d]).bytes(), hashdown(&top));
}

// exercises the empty-child chain a genuine `Middle` subtree emits through
// shared-bit levels (the case a `MiddleDbl` collapses but a `Middle` does not)
#[test]
fn five_leaf_root_emits_empty_children() {
    const BLANK: [u8; 32] = [0u8; 32];
    let a = leaf(0x58);
    let b = leaf(0x23);
    let c = leaf(0x21);
    let d = leaf(0xCA);
    let e = leaf(0x20);

    let cat = |tags: [u8; 2], l: &[u8; 32], r: &[u8; 32]| {
        let mut buf = vec![tags[0], tags[1]];
        buf.extend_from_slice(l);
        buf.extend_from_slice(r);
        hashdown(&buf)
    };
    let mut expected = cat([1, 1], &e, &c);
    expected = cat([2, 1], &expected, &b);
    expected = cat([2, 0], &expected, &BLANK);
    expected = cat([2, 0], &expected, &BLANK);
    expected = cat([2, 0], &expected, &BLANK);
    expected = cat([0, 2], &BLANK, &expected);
    expected = cat([2, 1], &expected, &a);
    expected = cat([2, 1], &expected, &d);

    assert_eq!(merkle_set_root(vec![a, b, c, d, e]).bytes(), expected);
    assert_eq!(merkle_set_root(vec![e, d, c, b, a]).bytes(), expected);
}

// hash_coin_ids: single -> sha256(id); multiple -> sort descending, concat, sha256.
#[test]
fn hash_coin_ids_matches_chia() {
    let x = leaf(0x11);
    let y = leaf(0x22);
    // Single: no prefix, just sha256 of the id.
    assert_eq!(hash_coin_ids(&[x]), super::hash_256(x));
    // Multiple: descending sort (y before x), concat, sha256 — regardless of input order.
    let mut buf = Vec::new();
    buf.extend_from_slice(&y);
    buf.extend_from_slice(&x);
    let expected = super::hash_256(buf);
    assert_eq!(hash_coin_ids(&[x, y]), expected);
    assert_eq!(hash_coin_ids(&[y, x]), expected);
}
