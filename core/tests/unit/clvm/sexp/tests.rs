//! SExp construction, environment traversal and tree-hash tests.
//! The canonical CLVM `()` tree hash is sha256 of `0x01`.
use super::*;

#[test]
fn nil_tree_hash_is_canonical() {
    assert_eq!(
        hex::encode(SExp::default().tree_hash().bytes()),
        "4bf5122f344554c53bde2ebb8cd2b7e3d1600ad631c385a5d7cce23c7785459a"
    );
}

#[test]
fn tree_hash_is_deterministic_and_shape_sensitive() {
    let a = SExp::from(vec![SExp::from(1), SExp::from(2), SExp::from(3)]);
    let b = SExp::from(vec![SExp::from(1), SExp::from(2), SExp::from(3)]);
    assert_eq!(a.tree_hash(), b.tree_hash());
    let c = SExp::from(vec![SExp::from(1), SExp::from(2)]);
    assert_ne!(a.tree_hash(), c.tree_hash());
}

#[test]
fn first_rest_split_and_nullp() {
    let list = SExp::from(vec![SExp::from(10), SExp::from(20)]);
    assert_eq!(list.first().unwrap().atom().unwrap().as_int(), 10.into());
    assert_eq!(*list.rest().unwrap(), SExp::from(vec![SExp::from(20)]));
    assert!(SExp::default().nullp());
    assert!(!list.nullp());
}

#[test]
fn as_atom_list_flattens_atoms() {
    let list = SExp::from(vec![SExp::from(1), SExp::from(2), SExp::from(3)]);
    assert_eq!(list.as_atom_list(), vec![vec![1u8], vec![2u8], vec![3u8]]);
}

#[test]
fn arg_count_and_arg_count_is() {
    let list = SExp::from(vec![SExp::from(1), SExp::from(2), SExp::from(3)]);
    assert_eq!(list.arg_count(10), 3);
    assert!(list.arg_count_is(3));
    assert!(!list.arg_count_is(2));
}

#[test]
fn int_round_trips_through_sexp() {
    for v in [
        0_i64,
        1,
        -1,
        127,
        128,
        -128,
        255,
        256,
        1000,
        -1000,
        i64::MAX,
    ] {
        let sexp = SExp::from(v);
        assert_eq!(sexp.atom().unwrap().as_int(), v.into());
    }
}

#[test]
fn small_ints_serialize_without_leading_zeros() {
    // 0 -> nil ; positive high-bit values keep a leading 0x00 sign byte.
    assert!(SExp::from(0_u8).nullp());
    assert_eq!(SExp::from(128_u32).atom().unwrap().as_ref(), &[0x00, 0x80]);
    assert_eq!(SExp::from(127_u32).atom().unwrap().as_ref(), &[0x7f]);
}
