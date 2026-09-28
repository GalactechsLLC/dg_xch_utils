use dg_xch_core::clvm::arena::Arena;
use dg_xch_core::clvm::runtime::ClvmRuntime;
use dg_xch_core::clvm::sexp::{PairBuf, SExp};
use std::sync::Arc;

#[test]
fn import_preserves_shared_subtrees_without_structural_deduplication() {
    for depth in [6, 8, 10] {
        let mut shared = Arc::new(SExp::from(1u8));
        for _ in 0..depth {
            shared = Arc::new(SExp::Pair(PairBuf::Owned((shared.clone(), shared))));
        }
        let mut arena = Arena::new();
        arena.import(&shared).unwrap();
        assert_eq!(arena.stored_pair_count(), depth);
        let distinct = SExp::Pair(PairBuf::Owned((
            Arc::new(SExp::from((1u8, 2u8))),
            Arc::new(SExp::from((1u8, 2u8))),
        )));
        let mut arena = Arena::new();
        arena.import(&distinct).unwrap();
        assert_eq!(arena.stored_pair_count(), 3);
    }
}

#[test]
fn shared_quoted_input_retains_cost_and_result() {
    let leaf = Arc::new(SExp::from((1u8, 2u8)));
    let expected = SExp::Pair(PairBuf::Owned((leaf.clone(), leaf)));
    let program = SExp::Pair(PairBuf::Owned((
        Arc::new(SExp::from(1u8)),
        Arc::new(expected.clone()),
    )));
    let mut runtime = ClvmRuntime::new(20, 0);
    let (cost, result) = runtime.run(&program, &SExp::default()).unwrap();
    assert_eq!(cost, 20);
    assert_eq!(result, expected);
}

#[test]
fn export_preserves_deep_shared_subtrees() {
    let mut arena = Arena::new();
    let mut node = arena.new_atom(&[42]).unwrap();
    for _ in 0..24 {
        node = arena.new_pair(node, node).unwrap();
    }
    let exported = arena.export(node);
    let mut current = &exported;
    for _ in 0..24 {
        let SExp::Pair(PairBuf::Owned((first, rest))) = current else {
            panic!("expected an owned pair");
        };
        assert!(Arc::ptr_eq(first, rest));
        current = first;
    }
    assert_eq!(current, &SExp::from(42u8));
    assert!(arena.display(node).contains("diagnostic limits"));
    assert!(arena.debug_fmt(node).contains("diagnostic limits"));
}

#[test]
fn diagnostics_bound_atom_size_and_depth_without_changing_small_values() {
    let mut arena = Arena::new();
    let small = arena.import(&SExp::from((1u8, 2u8))).unwrap();
    let exported = arena.export(small);
    assert_eq!(arena.display(small), exported.to_string());
    assert_eq!(arena.debug_fmt(small), format!("{exported:?}"));
    let large = arena.new_atom(&[42; 2048]).unwrap();
    assert!(arena.display(large).contains("diagnostic limits"));
    let mut deep = arena.new_atom(&[42]).unwrap();
    for _ in 0..80 {
        deep = arena
            .new_pair(deep, dg_xch_core::clvm::arena::NodePtr::NIL)
            .unwrap();
    }
    assert!(arena.debug_fmt(deep).contains("diagnostic limits"));
}

#[test]
fn atom_lengths_larger_than_u32_are_rejected_in_both_parsers() {
    use dg_xch_core::clvm::parser::{sexp_from_bytes, sexp_from_bytes_backrefs};
    use std::io::Cursor;

    let encoded = [0xf9, 0, 0, 0, 1, 42];
    assert!(sexp_from_bytes(&mut Cursor::new(encoded.as_slice())).is_err());
    assert!(sexp_from_bytes_backrefs(&mut Cursor::new(encoded.as_slice())).is_err());
}

#[test]
fn invalid_shared_operator_returns_a_bounded_error() {
    let mut operator = Arc::new(SExp::from(42u8));
    for _ in 0..24 {
        operator = Arc::new(SExp::Pair(PairBuf::Owned((operator.clone(), operator))));
    }
    let program = SExp::Pair(PairBuf::Owned((operator, Arc::new(SExp::default()))));
    let mut runtime = ClvmRuntime::new(1000, 0);
    let error = runtime
        .run(&program, &SExp::default())
        .unwrap_err()
        .to_string();
    assert!(error.contains("diagnostic limits"));
    assert!(error.len() < 256);
}
