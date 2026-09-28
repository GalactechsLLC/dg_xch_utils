use crate::clvm::assemble::assemble_text;
use crate::clvm::program::Program;
use crate::clvm::sexp::SExp;
use num_bigint::BigInt;

fn nested_list() -> Program<'static> {
    // Program.to([10, 20, 30, [15, 17], 40, 50])
    Program::new(SExp::from(vec![
        SExp::from(10),
        SExp::from(20),
        SExp::from(30),
        SExp::from(vec![SExp::from(15), SExp::from(17)]),
        SExp::from(40),
        SExp::from(50),
    ]))
}

#[test]
fn at_navigates_by_first_rest_path() {
    let p = nested_list();
    assert_eq!(p.at("f").unwrap().as_int().unwrap(), BigInt::from(10));
    assert_eq!(p.at("rrrfrf").unwrap().as_int().unwrap(), BigInt::from(17));
    // "q" is not a legal path character
    assert!(p.at("q").is_err());
    // "ff" walks into the first atom, which has no first()
    assert!(p.at("ff").is_err());
}

#[test]
fn run_div_positive_result() {
    let div = assemble_text("(/ 2 5)").unwrap();
    let (_c, out) = div
        .run(
            100_000,
            0,
            &Program::new(SExp::from(vec![SExp::from(10), SExp::from(5)])),
        )
        .unwrap();
    assert_eq!(out.as_vec(), Some(vec![0x02]));
}

// (/ 2 5) on [10, -5] returns 0xfe (-2) at cost 1107; requires signed
// two's-complement atom decode.
#[test]
fn run_div_negative_operand_result_and_cost() {
    let div = assemble_text("(/ 2 5)").unwrap();
    let (cost, out) = div
        .run(
            100_000,
            0,
            &Program::new(SExp::from(vec![SExp::from(10), SExp::from(-5)])),
        )
        .unwrap();
    assert_eq!(out.as_vec(), Some(vec![0xFE])); // -2 in two's complement
    assert_eq!(cost, 1107);
}

#[test]
fn uncurry_positive_case() {
    // (2 (q . (+ 2 5)) (c (q . 1) 1))
    let plus = assemble_text("(a (q 16 2 5) (c (q . 1) 1))").unwrap();
    let (f, args) = plus.uncurry().unwrap();
    assert_eq!(f, assemble_text("(+ 2 5)").unwrap());
    assert_eq!(args, Program::new(SExp::from(vec![SExp::from(1)])));
}

// curry then uncurry is idempotent
#[test]
fn curry_then_uncurry_round_trips() {
    let f = assemble_text("(+ 2 5)").unwrap();
    let curried = f.curry(&[Program::to(200), Program::to(30)]);
    let (f0, args0) = curried.uncurry().unwrap();
    assert_eq!(f0, f);
    assert_eq!(
        args0,
        Program::new(SExp::from(vec![SExp::from(200), SExp::from(30)]))
    );
}

// a program that was never curried uncurries to (self, nil) rather than erring
#[test]
fn uncurry_not_curried_returns_program_and_nil() {
    let plus = assemble_text("(+ 2 5)").unwrap();
    let (f, args) = plus.uncurry().unwrap();
    assert_eq!(f, plus);
    assert!(args.sexp().nullp());
}

// garbage at the end of the top-level list ⇒ (self, nil), never a partial uncurry
#[test]
fn uncurry_top_level_garbage_returns_program_and_nil() {
    let p = assemble_text("(2 (q . 1) (c (q . 1) (q . 1)) (q . 0x1337))").unwrap();
    let (f, args) = p.uncurry().unwrap();
    assert_eq!(f, p);
    assert!(args.sexp().nullp());
}

// the quoted-module slot is an atom, not a `(1 . <mod>)` pair ⇒ (self, nil)
#[test]
fn uncurry_not_pair_returns_program_and_nil() {
    let p = assemble_text("(2 1 (c (q . 1) (q . 1)))").unwrap();
    let (f, args) = p.uncurry().unwrap();
    assert_eq!(f, p);
    assert!(args.sexp().nullp());
}

// garbage at the end of an args cons ⇒ (self, nil)
#[test]
fn uncurry_args_garbage_returns_program_and_nil() {
    let p = assemble_text("(2 (q . 1) (c (q . 1) (q . 1) (q . 0x1337)))").unwrap();
    let (f, args) = p.uncurry().unwrap();
    assert_eq!(f, p);
    assert!(args.sexp().nullp());
}

// A plain atom and a plain (non-curry) pair both return (self, nil) rather
// than panicking on a short/absent top-level list.
#[test]
fn uncurry_plain_atom_and_plain_pair_return_program_and_nil() {
    let atom = Program::to(5);
    let (f, args) = atom.uncurry().unwrap();
    assert_eq!(f, atom);
    assert!(args.sexp().nullp());

    let nil = Program::to(0);
    let (f, args) = nil.uncurry().unwrap();
    assert_eq!(f, nil);
    assert!(args.sexp().nullp());

    // (16 2 5) is a proper 3-list whose head is not `\x02`: it superficially
    // resembles the apply shape but is not a curry ⇒ (self, nil).
    let plain = assemble_text("(16 2 5)").unwrap();
    let (f, args) = plain.uncurry().unwrap();
    assert_eq!(f, plain);
    assert!(args.sexp().nullp());
}
