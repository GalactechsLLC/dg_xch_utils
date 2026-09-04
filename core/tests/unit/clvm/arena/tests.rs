//! Round-trip and representation tests for the compact arena.
use super::*;
use crate::constants::{NULL_SEXP, ONE_SEXP};

#[test]
fn nil_and_one_are_inline() {
    let mut a = Arena::new();
    assert_eq!(a.new_atom(&[]).unwrap(), NodePtr::NIL);
    assert_eq!(a.new_atom(&[1]).unwrap(), NodePtr::ONE);
    assert!(a.nullp(NodePtr::NIL));
    assert!(a.non_nil(NodePtr::ONE));
    assert_eq!(a.atom(NodePtr::NIL).unwrap().as_ref(), &[] as &[u8]);
    assert_eq!(a.atom(NodePtr::ONE).unwrap().as_ref(), &[1]);
}

#[test]
fn small_atom_canonical_forms_only() {
    assert_eq!(fits_in_small_atom(&[]), Some(0));
    assert_eq!(fits_in_small_atom(&[0x7f]), Some(0x7f));
    assert_eq!(fits_in_small_atom(&[0x00, 0x80]), Some(0x80));
    assert_eq!(
        fits_in_small_atom(&[0x03, 0xff, 0xff, 0xff]),
        Some(0x03ff_ffff)
    );
    assert_eq!(fits_in_small_atom(&[0x00]), None); // non-canonical zero
    assert_eq!(fits_in_small_atom(&[0x80]), None); // negative
    assert_eq!(fits_in_small_atom(&[0x00, 0x7f]), None); // redundant leading zero
    assert_eq!(fits_in_small_atom(&[0x04, 0x00, 0x00, 0x00]), None); // > 26 bits
    assert_eq!(fits_in_small_atom(&[1, 2, 3, 4, 5]), None); // too long
    let mut a = Arena::new();
    for bytes in [
        vec![0x00],
        vec![0x80],
        vec![0x00, 0x7f],
        vec![0xff, 0xff],
        vec![1, 2, 3, 4, 5],
    ] {
        let ptr = a.new_atom(&bytes).unwrap();
        assert_eq!(a.atom(ptr).unwrap().as_ref(), &bytes[..], "{bytes:?}");
        assert_eq!(a.atom_len(ptr).unwrap(), bytes.len());
    }
}

#[test]
fn len_for_value_matches_canonical_encoding() {
    for v in [
        0u32,
        1,
        0x7f,
        0x80,
        0x7fff,
        0x8000,
        0x7f_ffff,
        0x80_0000,
        0x03ff_ffff,
    ] {
        let mut a = Arena::new();
        let ptr = a.new_i128(i128::from(v)).unwrap();
        assert_eq!(a.atom_len(ptr).unwrap(), len_for_value(v), "{v:#x}");
        match a.number(ptr).unwrap() {
            SExpNumber::I128(got) => assert_eq!(got, i128::from(v)),
            SExpNumber::BigInt(_) => panic!("small value decoded as bigint"),
        }
    }
}

#[test]
fn number_encode_matches_sexp_from() {
    // arena number encoding must be byte-identical to SExp::from's minimal signed big-endian
    let mut a = Arena::new();
    for v in [
        0i64,
        1,
        -1,
        127,
        128,
        -128,
        255,
        256,
        -256,
        65535,
        -65536,
        1 << 25,
        1 << 26,
        i64::MAX,
        i64::MIN,
    ] {
        let ptr = a.new_i128(i128::from(v)).unwrap();
        let expected = SExp::from(v);
        assert_eq!(
            a.atom(ptr).unwrap().as_ref(),
            expected.atom().unwrap().as_ref(),
            "{v}"
        );
    }
    for v in [
        BigInt::from(0),
        BigInt::from(1) << 200,
        -(BigInt::from(1) << 200i32),
    ] {
        let ptr = a.new_bigint(&v).unwrap();
        let expected = SExp::from(&v);
        assert_eq!(
            a.atom(ptr).unwrap().as_ref(),
            expected.atom().unwrap().as_ref(),
            "{v}"
        );
    }
}

#[test]
fn substr_views_and_small_atoms() {
    let mut a = Arena::new();
    let big = a.new_atom(b"hello world").unwrap();
    let sub = a.new_substr(big, 6, 11).unwrap();
    assert_eq!(a.atom(sub).unwrap().as_ref(), b"world");
    let small = a.new_atom(&[0x01, 0x02]).unwrap();
    let sub2 = a.new_substr(small, 1, 2).unwrap();
    assert_eq!(a.atom(sub2).unwrap().as_ref(), &[0x02]);
    let small3 = a.new_atom(&[0x01, 0x00]).unwrap();
    let sub3 = a.new_substr(small3, 1, 2).unwrap();
    assert_eq!(a.atom(sub3).unwrap().as_ref(), &[0x00]);
    assert!(a.new_substr(big, 12, 12).is_err());
    assert!(a.new_substr(big, 3, 2).is_err());
}

#[test]
fn concat_copies_all_terms() {
    let mut a = Arena::new();
    let x = a.new_atom(b"foo").unwrap();
    let y = a.new_atom(b"bar").unwrap();
    let small = a.new_atom(&[0x01]).unwrap();
    let cat = a.new_concat(7, &[x, y, small]).unwrap();
    assert_eq!(a.atom(cat).unwrap().as_ref(), b"foobar\x01");
    let same = a.new_concat(3, &[x]).unwrap();
    assert_eq!(same, x);
    assert!(a.new_concat(5, &[x, y]).is_err());
}

#[test]
fn import_export_round_trips_shape_and_bytes() {
    let tree = SExp::from(vec![
        SExp::from(1),
        SExp::from((2_u8, 3_u8)),
        SExp::from(b"some longer atom content".to_vec()),
        SExp::from(vec![SExp::from(-1), SExp::from(0), SExp::from(128)]),
    ]);
    let mut a = Arena::new();
    let ptr = a.import(&tree).unwrap();
    let back = a.export(ptr);
    assert_eq!(back, tree);
}

// 2,000 generated trees with mixed atom encodings (canonical, non-canonical, long)
// and nesting must survive import→export byte-identically.
#[test]
fn round_trip_property_pseudo_random() {
    fn xorshift(state: &mut u64) -> u64 {
        let mut x = *state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *state = x;
        x
    }
    fn gen_tree(state: &mut u64, depth: u32) -> SExp<'static> {
        let r = xorshift(state);
        if depth == 0 || r.is_multiple_of(3) {
            let len = (xorshift(state) % 40) as usize;
            let bytes: Vec<u8> = (0..len).map(|_| (xorshift(state) & 0xff) as u8).collect();
            SExp::Atom(AtomBuf::new(bytes))
        } else {
            let first = gen_tree(state, depth - 1);
            let rest = gen_tree(state, depth - 1);
            SExp::Pair(PairBuf::Owned((Arc::new(first), Arc::new(rest))))
        }
    }
    let mut state = 0x181_cafe_f00d_u64;
    for i in 0..2000 {
        let tree = gen_tree(&mut state, 6);
        let mut a = Arena::new();
        let ptr = a.import(&tree).unwrap();
        assert_eq!(a.export(ptr), tree, "case {i}");
    }
}

#[test]
fn arg_cursor_matches_sexp_iter_semantics() {
    let mut a = Arena::new();
    let list = a
        .import(&SExp::from(vec![
            SExp::from(1),
            SExp::from(2),
            SExp::from(3),
        ]))
        .unwrap();
    let mut cur = ArgCursor::new(list);
    let mut seen = Vec::new();
    while let Some(p) = cur.next(&a) {
        seen.push(a.number(p).unwrap());
    }
    assert_eq!(seen.len(), 3);
    let improper = a.import(&SExp::from((1_u8, (2_u8, 3_u8)))).unwrap();
    let mut cur = ArgCursor::new(improper);
    let mut count = 0;
    while cur.next(&a).is_some() {
        count += 1;
    }
    assert_eq!(count, 3);
    let nil = a.import(&NULL_SEXP).unwrap();
    let mut cur = ArgCursor::new(nil);
    assert!(cur.next(&a).is_none());
    let one = a.import(&ONE_SEXP).unwrap();
    let mut cur = ArgCursor::new(one);
    assert_eq!(cur.next(&a), Some(NodePtr::ONE));
    assert!(cur.next(&a).is_none());
}

#[test]
fn arg_count_and_atom_list() {
    let mut a = Arena::new();
    let list = a
        .import(&SExp::from(vec![
            SExp::from(1),
            SExp::from(2),
            SExp::from(3),
        ]))
        .unwrap();
    assert_eq!(a.arg_count(list, 10), 3);
    assert!(a.arg_count_is(list, 3));
    assert!(!a.arg_count_is(list, 2));
    assert_eq!(a.as_atom_list(list), vec![vec![1u8], vec![2u8], vec![3u8]]);
    let nested = a
        .import(&SExp::from(vec![
            SExp::from(vec![SExp::from(1)]),
            SExp::from(2),
        ]))
        .unwrap();
    assert!(a.as_atom_list(nested).is_empty());
}

#[test]
fn reset_reclaims_pools() {
    let mut a = Arena::new();
    let _ = a.new_atom(b"some content").unwrap();
    let x = a.new_atom(b"another atom here").unwrap();
    let _ = a.new_pair(x, NodePtr::NIL).unwrap();
    let before = a.counters();
    assert!(before.0 > 2 && before.1 > 0);
    a.reset();
    assert_eq!(a.counters(), (2, 0, 1));
}

#[test]
fn rewound_storage_stays_counted_against_the_ceilings() {
    let mut a = Arena::new();
    let cp = a.checkpoint();
    let atoms0 = a.atom_vec.len() + a.ghost_atoms;
    let pairs0 = a.pair_vec.len() + a.ghost_pairs;

    let x = a.new_atom(&[7u8; 40]).unwrap();
    let y = a.new_atom(&[9u8; 40]).unwrap();
    a.new_pair(x, y).unwrap();
    let atoms1 = a.atom_vec.len() + a.ghost_atoms;
    let pairs1 = a.pair_vec.len() + a.ghost_pairs;
    let heap1 = a.u8_vec.len() + a.ghost_heap;
    assert_eq!(atoms1, atoms0 + 2);
    assert_eq!(pairs1, pairs0 + 1);

    a.restore(cp);
    assert_eq!(
        a.atom_vec.len() + a.ghost_atoms,
        atoms1,
        "rewound atoms fell out of the count"
    );
    assert_eq!(
        a.pair_vec.len() + a.ghost_pairs,
        pairs1,
        "rewound pairs fell out of the count"
    );
    assert_eq!(
        a.u8_vec.len() + a.ghost_heap,
        heap1,
        "rewound heap fell out of the count"
    );
}
