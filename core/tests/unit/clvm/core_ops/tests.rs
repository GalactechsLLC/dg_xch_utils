//! Behavioral tests for the core CLVM operators. Operands are quoted so the
//! eval loop pre-evaluates them to literals before the operator is applied.
use crate::clvm::program::Program;
use crate::clvm::sexp::SExp;
use crate::clvm::utils::INFINITE_COST;
use crate::errors::ClvmError;
use num_bigint::BigInt;

// Build and run `(op (q . a0) (q . a1) ...)` against a nil environment.
fn run_op(op: u8, args: &[SExp<'static>]) -> Result<SExp<'static>, ClvmError> {
    let mut items = vec![SExp::from(op)];
    for a in args {
        items.push(SExp::from((1_u8, a.clone())));
    }
    let program = Program::new(SExp::from(items));
    program
        .run(INFINITE_COST, 0, &Program::default())
        .map(|(_c, out)| out.sexp().to_owned())
}

fn int(sexp: &SExp) -> BigInt {
    sexp.atom().unwrap().as_int()
}

#[test]
fn op_if_selects_branch_by_truthiness() {
    // (i 1 20 30) -> 20 ; (i () 20 30) -> 30
    let t = run_op(3, &[SExp::from(1), SExp::from(20), SExp::from(30)]).unwrap();
    assert_eq!(int(&t), BigInt::from(20));
    let f = run_op(3, &[SExp::default(), SExp::from(20), SExp::from(30)]).unwrap();
    assert_eq!(int(&f), BigInt::from(30));
}

#[test]
fn op_cons_builds_pair() {
    // (c 1 2) -> (1 . 2)
    let out = run_op(4, &[SExp::from(1), SExp::from(2)]).unwrap();
    assert_eq!(out, SExp::from((1_u8, 2_u8)));
}

#[test]
fn op_first_and_rest() {
    let list = SExp::from(vec![SExp::from(10), SExp::from(20)]);
    let first = run_op(5, std::slice::from_ref(&list)).unwrap();
    assert_eq!(int(&first), BigInt::from(10));
    let rest = run_op(6, &[list]).unwrap();
    assert_eq!(rest, SExp::from(vec![SExp::from(20)]));
}

#[test]
fn op_listp_distinguishes_pairs_from_atoms() {
    let is_pair = run_op(7, &[SExp::from(vec![SExp::from(1), SExp::from(2)])]).unwrap();
    assert_eq!(int(&is_pair), BigInt::from(1));
    let is_atom = run_op(7, &[SExp::from(5)]).unwrap();
    assert!(is_atom.nullp());
}

#[test]
fn op_eq_compares_atoms() {
    let equal = run_op(9, &[SExp::from(5), SExp::from(5)]).unwrap();
    assert_eq!(int(&equal), BigInt::from(1));
    let unequal = run_op(9, &[SExp::from(5), SExp::from(6)]).unwrap();
    assert!(unequal.nullp());
}

#[test]
fn op_raise_errors() {
    let err = run_op(8, &[SExp::from(1)]).unwrap_err();
    assert!(matches!(err, ClvmError::Raise(_)), "got {err:?}");
}

#[test]
fn op_first_wrong_arg_count_errors() {
    // (f 1 2) — first takes exactly one argument.
    let err = run_op(5, &[SExp::from(1), SExp::from(2)]).unwrap_err();
    assert!(
        matches!(err, ClvmError::InvalidOperandArgs("f", 1)),
        "got {err:?}"
    );
}

#[test]
fn op_first_on_atom_errors() {
    let err = run_op(5, &[SExp::from(5)]).unwrap_err();
    assert!(
        matches!(err, ClvmError::ExpectedPairGotAtom(_)),
        "got {err:?}"
    );
}
