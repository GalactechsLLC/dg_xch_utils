//! Eval-loop, environment-traversal and cost-accounting tests. The traversal-cost
//! constants are the canonical CLVM path-lookup costs (TRAVERSE_BASE_COST 40 +
//! 4 per zero byte + 4 per bit).
use super::*;
use crate::clvm::program::{Program, SerializedProgram};
use crate::clvm::sexp::SExp;
use crate::clvm::utils::INFINITE_COST;
use num_bigint::BigInt;

// factorial
const FACTORIAL_HEX: &str = "ff02ffff01ff02ff02ffff04ff02ffff04ff05ff80808080ffff04ffff01ff02\
ffff03ffff09ff05ffff010180ffff01ff0101ffff01ff12ff05ffff02ff02ff\
ff04ff02ffff04ffff11ff05ffff010180ff808080808080ff0180ff018080";

fn run_path(path: SExp<'static>, args: SExp<'static>) -> (u64, SExp<'static>) {
    let mut runtime = ClvmRuntime::new(INFINITE_COST, 0);
    let program = Program::new(path);
    let arguments = Program::new(args);
    let (cost, out) = runtime.run(program.sexp(), arguments.sexp()).unwrap();
    (cost, out)
}

#[test]
fn quote_returns_operand_at_quote_cost() {
    let program = Program::to((1_u8, 100_u8));
    let args = Program::default();
    let (cost, out) = program.run(INFINITE_COST, 0, &args).unwrap();
    assert_eq!(out.as_int().unwrap(), BigInt::from(100));
    // QUOTE_COST only
    assert_eq!(cost, 20);
}

#[test]
fn path_1_returns_whole_environment() {
    let args = SExp::from(vec![SExp::from(10), SExp::from(20), SExp::from(30)]);
    let (cost, out) = run_path(SExp::from(1_u8), args.clone());
    assert_eq!(out, args);
    assert_eq!(cost, 44); // TRAVERSE_BASE_COST + one bit
}

#[test]
fn path_2_returns_first_and_path_3_returns_rest() {
    let args = SExp::from(vec![SExp::from(10), SExp::from(20), SExp::from(30)]);
    let (cost2, first) = run_path(SExp::from(2_u8), args.clone());
    assert_eq!(first.atom().unwrap().as_int(), BigInt::from(10));
    assert_eq!(cost2, 48);
    let (cost3, rest) = run_path(SExp::from(3_u8), args.clone());
    assert_eq!(rest, SExp::from(vec![SExp::from(20), SExp::from(30)]));
    assert_eq!(cost3, 48);
}

#[test]
fn path_5_returns_second_element() {
    let args = SExp::from(vec![SExp::from(10), SExp::from(20), SExp::from(30)]);
    let (cost, out) = run_path(SExp::from(5_u8), args);
    assert_eq!(out.atom().unwrap().as_int(), BigInt::from(20));
    assert_eq!(cost, 52);
}

#[test]
fn cost_limit_is_enforced() {
    let program = Program::to((1_u8, 100_u8));
    let args = Program::default();
    // QUOTE_COST (20) exceeds a budget of 5.
    let err = program.run(5, 0, &args).unwrap_err();
    assert!(matches!(err, ClvmError::CostExceeded(_, _)), "got {err:?}");
}

// factorial(5) == 120
#[test]
fn factorial_of_five_is_120() {
    let serial = SerializedProgram::from_hex(FACTORIAL_HEX).unwrap();
    let program = serial.to_program().unwrap();
    // args "ff0580" == (5)
    let args_serial = SerializedProgram::from_hex("ff0580").unwrap();
    let args = args_serial.to_program().unwrap();
    let (_cost, out) = program.run(INFINITE_COST, 0, &args).unwrap();
    assert_eq!(out.as_int().unwrap(), BigInt::from(120));
}

// Red-first: the pair-operator form pushes an Apply frame with no checkpoint, so a
// self-contained result on that path pops the ENCLOSING frame's checkpoint and rewinds the
// already-evaluated sibling out from under the pending cons. Operands evaluate right to
// left, so the sibling materializes first and sits live across the second apply.
#[test]
fn inner_atom_operator_leaves_live_siblings_intact() {
    use crate::clvm::assemble::assemble_text;
    let nil = SExp::Atom(crate::clvm::sexp::AtomBuf::new(vec![]));
    let run = |src: &str| {
        let prog: Program = assemble_text(src).unwrap();
        let mut rt = ClvmRuntime::new(INFINITE_COST, 0);
        rt.run(prog.sexp(), &nil).unwrap().1
    };
    let left = run("((sha256))");
    let right = run("(sha256 (q . 1))");
    let combined = run("(c ((sha256)) (sha256 (q . 1)))");
    let expect = SExp::Pair(crate::clvm::sexp::PairBuf::from((left, right)));
    assert_eq!(combined.to_string(), expect.to_string());
}

// Red-first: the apply branch returns without releasing its frame's checkpoint, so every
// `a` application leaks one entry for the rest of the run and later frames pop mispaired
// checkpoints. The factorial program applies `a` once per recursion step.
#[test]
fn every_apply_frame_releases_its_checkpoint() {
    let serial = SerializedProgram::from_hex(FACTORIAL_HEX).unwrap();
    let program = serial.to_program().unwrap();
    let args_serial = SerializedProgram::from_hex("ff0580").unwrap();
    let args = args_serial.to_program().unwrap();
    let mut runtime = ClvmRuntime::new(INFINITE_COST, 0);
    let (_cost, out) = runtime.run(program.sexp(), args.sexp()).unwrap();
    assert_eq!(out.as_int().unwrap().to_u64(), Some(120));
    assert!(
        runtime.checkpoint_stack.is_empty(),
        "{} checkpoint frames leaked across the run",
        runtime.checkpoint_stack.len()
    );
}
