use dg_xch_core::clvm::compile::COMPAT_CHIA;
use dg_xch_core::clvm::runtime::ClvmRuntime;
use dg_xch_core::clvm::utils::MEMPOOL_MODE;

#[test]
fn softfork_canonical_ints_rejects_noncanonical_cost() {
    use dg_xch_core::clvm::sexp::{AtomBuf, SExp};
    use dg_xch_core::clvm::utils::CANONICAL_INTS;

    // Build `(36 (1 . <cost>))` — softfork (op 36) applied to a quoted (op 1) cost atom.
    let build = |cost_bytes: Vec<u8>| -> SExp<'static> {
        let op36 = SExp::Atom(AtomBuf::new(vec![36]));
        let quote = SExp::Atom(AtomBuf::new(vec![1]));
        let cost = SExp::Atom(AtomBuf::new(cost_bytes));
        let quoted = quote.cons(cost); // (1 . cost)
        let args = quoted.cons(SExp::default()); // ((1 . cost))
        op36.cons(args) // (36 (1 . cost))
    };

    // Canonical cost 0x05: accepted with and without the flag.
    let canonical = build(vec![5]);
    assert!(
        ClvmRuntime::new(u64::MAX, 0)
            .run(&canonical, &SExp::default())
            .is_ok()
    );
    assert!(
        ClvmRuntime::new(u64::MAX, CANONICAL_INTS)
            .run(&canonical, &SExp::default())
            .is_ok()
    );

    // Non-canonical cost 0x00 0x05 (redundant leading zero): accepted pre-SF9 (flag clear),
    // rejected at/above SF9 (flag set).
    let noncanonical = build(vec![0, 5]);
    assert!(
        ClvmRuntime::new(u64::MAX, 0)
            .run(&noncanonical, &SExp::default())
            .is_ok(),
        "non-canonical softfork cost must be accepted below soft_fork9_height"
    );
    assert!(
        ClvmRuntime::new(u64::MAX, CANONICAL_INTS)
            .run(&noncanonical, &SExp::default())
            .is_err(),
        "non-canonical softfork cost must be rejected at/above soft_fork9_height"
    );
}

#[test]
fn test_mod() {
    use dg_xch_core::clvm::compile::Compiler;
    use std::borrow::Cow;
    const EXAMPLE_CLSP: &str = "
    (mod (num)
        (* num 25)
    )";
    let compiler = Compiler::new(Cow::Borrowed(EXAMPLE_CLSP.as_bytes()), 0, 0, &[]);
    let prog = compiler.compile().unwrap();
    assert_eq!("(* 2 (q . 25))", format!("{prog}"))
}

#[test]
fn test_classic_reference_bytes() {
    use dg_xch_core::clvm::compile::{COMPAT_CHIA, Compiler, OPT_REFERENCE};
    use std::borrow::Cow;

    // Reference compiler 0.4.5, -O. The first case is warp's bridging puzzle
    // with constants inlined from its include and an unused declaration.
    for (source, expected) in [
        (
            "(mod (amount)
                (defconstant UNUSED 99)
                (defconstant RESERVE_FEE 52)
                (defconstant ASSERT_MY_AMOUNT 73)
                (list (list ASSERT_MY_AMOUNT amount) (list RESERVE_FEE amount)))",
            "ff02ffff01ff04ffff04ff04ffff04ff05ff808080ffff04ffff04ff06ffff04ff05ff808080ff808080ffff04ffff01ff4934ff018080",
        ),
        (
            "(mod (X) (defconstant Z 3) (defconstant B 2) (defconstant A 1)
                (list Z B A X))",
            "ff02ffff01ff04ff0effff04ff0affff04ff04ffff04ff05ff8080808080ffff04ffff01ff01ff0203ff018080",
        ),
        (
            "(mod (X) (defconstant Z 3)
                (defun unused (X) (+ X 9))
                (defun F (X) (+ X Z)) (F X))",
            "ff02ffff01ff02ff04ffff04ff02ffff04ff05ff80808080ffff04ffff01ffff10ff05ff068003ff018080",
        ),
    ] {
        let compiler = Compiler::new(
            Cow::Borrowed(source.as_bytes()),
            COMPAT_CHIA,
            OPT_REFERENCE,
            &[],
        );
        let program = compiler.compile().unwrap();
        assert_eq!(
            hex::encode(program.serialized().unwrap().as_ref()),
            expected
        );
    }
}

#[test]
fn test_inline_destructuring_with_rest() {
    use dg_xch_core::clvm::assemble::assemble_text;
    use dg_xch_core::clvm::compile::{Compiler, OPT_REFERENCE};
    use dg_xch_core::clvm::utils::INFINITE_COST;
    use std::borrow::Cow;

    // Reference compiler 0.4.5, -O. Classic rest forms are applied as source;
    // CL23 rest arguments are lists. Empty rest arguments must not panic.
    for (source, expected, env, result) in [
        (
            "(mod (X Y)  (defun-inline F ((A . B) . R) (list A B R)) (F X Y))",
            "ff04ff04ffff04ff06ffff04ffff02ff05ffff04ff02ff808080ff80808080",
            "((4 . 5) (q . 9))",
            "(4 5 9)",
        ),
        (
            "(mod (X Y)  (defun-inline F ((A . B) . R) (list A B R)) (F X))",
            "ff04ff04ffff04ff06ffff01ff80808080",
            "((4 . 5) (q . 9))",
            "(4 5 ())",
        ),
        (
            "(mod (X Y)  (defun-inline F ((A . B) . R) (list A B R)) (F X Y Y))",
            "ff04ff04ffff04ff06ffff04ffff02ff05ffff04ff02ffff04ff05ff80808080ff80808080",
            "((4 . 5) (q . 9))",
            "(4 5 9)",
        ),
        (
            "(mod (X Y) (include *standard-cl-23*) (defun-inline F ((A . B) . R) (list A B R)) (F X Y))",
            "ff04ff04ffff04ff06ffff04ffff04ff05ff8080ff80808080",
            "((4 . 5) 9)",
            "(4 5 (9))",
        ),
        (
            "(mod (X Y) (include *standard-cl-23*) (defun-inline F ((A . B) . R) (list A B R)) (F X))",
            "ff04ff04ffff04ff06ffff01ff80808080",
            "((4 . 5) 9)",
            "(4 5 ())",
        ),
        (
            "(mod (X Y) (include *standard-cl-23*) (defun-inline F ((A . B) . R) (list A B R)) (F X Y Y))",
            "ff04ff04ffff04ff06ffff04ffff04ff05ffff04ff05ff808080ff80808080",
            "((4 . 5) 9)",
            "(4 5 (9 9))",
        ),
    ] {
        let compiler = Compiler::new(
            Cow::Borrowed(source.as_bytes()),
            COMPAT_CHIA,
            OPT_REFERENCE,
            &[],
        );
        let program = compiler.compile().unwrap();
        assert_eq!(
            hex::encode(program.serialized().unwrap().as_ref()),
            expected,
            "{source}"
        );
        assert_eq!(
            program
                .run(INFINITE_COST, 0, &assemble_text(env).unwrap())
                .unwrap()
                .1,
            assemble_text(result).unwrap(),
            "{source}"
        );
    }
}

#[test]
fn test_invalid_declaration_returns_error() {
    use dg_xch_core::clvm::compile::{Compiler, OPT_REFERENCE};
    use std::borrow::Cow;

    for source in ["(mod () () (list))", "(mod () ((x)) (list))"] {
        for flags in [0, COMPAT_CHIA] {
            let compiler =
                Compiler::new(Cow::Borrowed(source.as_bytes()), flags, OPT_REFERENCE, &[]);
            assert!(compiler.compile().is_err(), "{source}");
        }
    }
}

#[test]
fn test_cl21_cl23_reference_bytes_and_execution() {
    use dg_xch_core::clvm::assemble::assemble_text;
    use dg_xch_core::clvm::compile::{Compiler, OPT_REFERENCE};
    use dg_xch_core::clvm::utils::INFINITE_COST;
    use std::borrow::Cow;

    // Reference compiler 0.4.5, -O: CL21 code generation and CL23 closures.
    for (source, expected, env, result) in [
        (
            "(mod (X) (include *standard-cl-21*) (defun F (Y) (if Y (list (+ Y 1)) ())) (F X))",
            "ff02ffff01ff02ff02ffff04ff02ffff04ff05ff80808080ffff04ffff01ff02ffff03ff05ffff01ff02ffff01ff04ffff10ff05ffff010180ffff018080ff0180ffff01ff02ffff01ff0180ff018080ff0180ff018080",
            "(5)",
            "(6)",
        ),
        (
            "(mod (X) (include *standard-cl-21*) (defun F ((@ whole (A . B))) (list whole A B)) (F X 99))",
            "ff02ffff01ff02ff02ffff04ff02ffff04ff05ffff04ffff0163ff8080808080ffff04ffff01ff04ff05ffff04ff09ffff04ff0dff80808080ff018080",
            "((4 . 5))",
            "((4 . 5) 4 5)",
        ),
        (
            "(mod (X) (include *standard-cl-21*) (defun F (Y) (+ Y 1)) (let ((Y (F X))) (list Y X)))",
            "ff02ffff01ff04ffff02ff02ffff04ff02ffff04ff05ff80808080ffff04ffff05ffff06ff018080ffff01808080ffff04ffff01ff10ff05ffff010180ff018080",
            "(5)",
            "(6 5)",
        ),
        (
            "(mod (X) (include *standard-cl-23*) (defun F (Y) (+ Y 1)) (a F (list X)))",
            "ff02ffff01ff02ffff04ffff0102ffff04ffff04ffff0101ff0280ffff04ffff04ffff0104ffff04ffff04ffff0101ff0280ffff04ffff0101ff80808080ff80808080ffff04ff05ff808080ffff04ffff01ff10ff05ffff010180ff018080",
            "(5)",
            "6",
        ),
        (
            "(mod (X) (include *standard-cl-23*) (a (lambda ((& X) Y) (+ X Y)) (list 3)))",
            "ff02ffff01ff02ffff04ffff0102ffff04ffff04ffff0101ffff04ffff0102ffff04ffff04ffff0101ff0280ffff04ffff04ffff0104ffff04ffff04ffff0101ff0280ffff04ffff0101ff80808080ff8080808080ffff04ffff04ffff0104ffff04ffff04ffff0101ffff04ff05ff808080ffff04ffff0101ff80808080ff80808080ffff04ffff0103ff808080ffff04ffff01ff10ff09ff0b80ff018080",
            "(5)",
            "8",
        ),
        (
            "(mod (X) (include *standard-cl-23*) (a (lambda ((& X) Y) (assign Z (+ X Y) (list Z Z))) (list 3)))",
            "ff02ffff01ff02ffff04ffff0102ffff04ffff04ffff0101ffff04ffff0102ffff04ffff04ffff0101ff0280ffff04ffff04ffff0104ffff04ffff04ffff0101ff0280ffff04ffff0101ff80808080ff8080808080ffff04ffff04ffff0104ffff04ffff04ffff0101ffff04ff05ff808080ffff04ffff0101ff80808080ff80808080ffff04ffff0103ff808080ffff04ffff01ff04ffff10ff09ff0b80ffff04ffff10ff09ff0b80ff808080ff018080",
            "(5)",
            "(8 8)",
        ),
    ] {
        let compiler = Compiler::new(
            Cow::Borrowed(source.as_bytes()),
            COMPAT_CHIA,
            OPT_REFERENCE,
            &[],
        );
        let program = compiler.compile().unwrap();
        assert_eq!(
            hex::encode(program.serialized().unwrap().as_ref()),
            expected,
            "{source}"
        );
        assert_eq!(
            program
                .run(INFINITE_COST, 0, &assemble_text(env).unwrap())
                .unwrap()
                .1,
            assemble_text(result).unwrap(),
            "{source}"
        );
        let compiler = Compiler::new(Cow::Borrowed(source.as_bytes()), 0, OPT_REFERENCE, &[]);
        assert!(
            compiler.compile().is_err(),
            "Chia sigils require COMPAT_CHIA"
        );
    }
}

#[test]
fn test_invalid_cl21_cl23_forms() {
    use dg_xch_core::clvm::compile::{Compiler, OPT_REFERENCE};
    use std::borrow::Cow;

    for body in [
        "(let X 1)",
        "(let (X) 1)",
        "(let ((X)) X)",
        "(let ((X 1 2)) X)",
        "(lambda ())",
        "(lambda ((& (X)) Y) Y)",
    ] {
        let source = format!("(mod () (include *standard-cl-23*) {body})");
        let compiler = Compiler::new(
            Cow::Borrowed(source.as_bytes()),
            COMPAT_CHIA,
            OPT_REFERENCE,
            &[],
        );
        assert!(compiler.compile().is_err(), "{source}");
    }
    let source = "(mod ((@ X)) (include *standard-cl-21*) (list X))";
    let compiler = Compiler::new(
        Cow::Borrowed(source.as_bytes()),
        COMPAT_CHIA,
        OPT_REFERENCE,
        &[],
    );
    assert!(compiler.compile().is_err());
}

#[test]
fn test_classic_canonical_compatibility() {
    use dg_xch_core::clvm::compile::{Compiler, OPT_REFERENCE};
    use std::borrow::Cow;

    // Reference compiler 0.4.5, -O: opcode aliases, environment paths,
    // permissive function arity, numeric folding, and assembled constants.
    for (source, expected) in [
        (
            "(mod (X) (secp256k1_verify X X X))",
            "ff8413d61f00ff02ff02ff0280",
        ),
        (
            "(mod (X) (secp256r1_verify X X X))",
            "ff841c3a8f00ff02ff02ff0280",
        ),
        ("(mod (X Y) (f (r @)))", "05"),
        ("(mod ; comment\n ARGS (f ARGS))", "02"),
        (
            "(mod (X) (defun F (A B) A) (F X))",
            "ff02ffff01ff02ff02ffff04ff02ffff04ff05ff80808080ffff04ffff0105ff018080",
        ),
        (
            "(mod (X) (defun F (A) A) (F X 99))",
            "ff02ffff01ff02ff02ffff04ff02ffff04ff05ffff01ff6380808080ffff04ffff0105ff018080",
        ),
        (
            "(mod () (- (lsh 1 128) 1))",
            "ff019100ffffffffffffffffffffffffffffffff",
        ),
        (
            "(mod () (defconstant CODE (a (q . 1) 1)) (list CODE))",
            "ff02ffff01ff04ff02ff8080ffff04ffff01ff02ffff0101ff0180ff018080",
        ),
        (
            "(mod (X) (if X 1 0))",
            "ff02ffff03ff02ffff01ff0101ff8080ff0180",
        ),
    ] {
        let compiler = Compiler::new(
            Cow::Borrowed(source.as_bytes()),
            COMPAT_CHIA,
            OPT_REFERENCE,
            &[],
        );
        let program = compiler.compile().unwrap();
        assert_eq!(
            hex::encode(program.serialized().unwrap().as_ref()),
            expected,
            "{source}"
        );
    }
}

#[test]
fn test_classic_quasiquote() {
    use dg_xch_core::clvm::assemble::assemble_text;
    use dg_xch_core::clvm::compile::{Compiler, OPT_REFERENCE};
    use dg_xch_core::clvm::utils::INFINITE_COST;
    use std::borrow::Cow;

    // Reference compiler 0.4.5, -O, including canonical p2_conditions.clsp.
    for (source, expected, env, result) in [
        (
            "(mod (conditions) (qq (q . (unquote conditions))))",
            "ff04ffff0101ff0280",
            "(((51 0x1234 1)))",
            "(q . ((51 0x1234 1)))",
        ),
        (
            "(mod (X) (qq (1 (unquote (+ X 2)) . (unquote X))))",
            "ff04ffff0101ffff04ffff10ff02ffff010280ff028080",
            "(5)",
            "(1 7 . 5)",
        ),
        (
            "(mod () (qq (q \"q\" ())))",
            "ff01ff01ff71ff8080",
            "()",
            "(1 \"q\" ())",
        ),
    ] {
        let compiler = Compiler::new(
            Cow::Borrowed(source.as_bytes()),
            COMPAT_CHIA,
            OPT_REFERENCE,
            &[],
        );
        let program = compiler.compile().unwrap();
        assert_eq!(
            hex::encode(program.serialized().unwrap().as_ref()),
            expected
        );
        assert_eq!(
            program
                .run(INFINITE_COST, 0, &assemble_text(env).unwrap())
                .unwrap()
                .1,
            assemble_text(result).unwrap()
        );
    }
    for source in [
        "(mod (X) (qq))",
        "(mod (X) (qq X X))",
        "(mod (X) (qq (unquote)))",
        "(mod (X) (qq (unquote X X)))",
    ] {
        let compiler = Compiler::new(
            Cow::Borrowed(source.as_bytes()),
            COMPAT_CHIA,
            OPT_REFERENCE,
            &[],
        );
        assert!(compiler.compile().is_err(), "{source}");
    }
}

#[test]
fn test_classic_constant_folding() {
    use dg_xch_core::clvm::assemble::assemble_text;
    use dg_xch_core::clvm::compile::{Compiler, OPT_REFERENCE};
    use dg_xch_core::clvm::utils::INFINITE_COST;
    use std::borrow::Cow;

    // Reference compiler 0.4.5, -O: fold expressions without interpreting quoted data.
    for (source, expected, result) in [
        (
            "(mod (X) (list 0 (list 1 2) X))",
            "ff04ff80ffff04ffff01ff01ff0280ffff04ff02ff80808080",
            "(() (1 2) 5)",
        ),
        (
            "(mod (X) (if X () (x)))",
            "ff02ffff03ff02ff80ffff01ff088080ff0180",
            "()",
        ),
        (
            "(mod (X) (list (q . (1)) (q . (4 (1 . 3) (1 . 4))) X))",
            "ff04ffff01ff0180ffff04ffff01ff04ffff0103ffff010480ffff04ff02ff80808080",
            "((1) (4 (1 . 3) (1 . 4)) 5)",
        ),
    ] {
        for flags in [0, COMPAT_CHIA] {
            let compiler =
                Compiler::new(Cow::Borrowed(source.as_bytes()), flags, OPT_REFERENCE, &[]);
            let program = compiler.compile().unwrap();
            if flags == COMPAT_CHIA {
                assert_eq!(
                    hex::encode(program.serialized().unwrap().as_ref()),
                    expected
                );
            }
            assert_eq!(
                program
                    .run(INFINITE_COST, 0, &assemble_text("(5)").unwrap())
                    .unwrap()
                    .1,
                assemble_text(result).unwrap(),
            );
        }
    }
    let compiler = Compiler::new(
        Cow::Borrowed(b"(mod () (c (q . 1) (x)))"),
        COMPAT_CHIA,
        OPT_REFERENCE,
        &[],
    );
    assert!(
        compiler
            .compile()
            .unwrap()
            .run(INFINITE_COST, 0, &assemble_text("()").unwrap())
            .is_err()
    );
}

#[test]
fn test_classic_arguments_and_constants() {
    use dg_xch_core::clvm::assemble::assemble_text;
    use dg_xch_core::clvm::compile::{Compiler, OPT_REFERENCE};
    use dg_xch_core::clvm::utils::INFINITE_COST;
    use std::borrow::Cow;

    // Reference compiler 0.4.5, -O.
    for (source, expected, env, result) in [
        (
            "(mod (A B C D E F G) (list G (f G) (r G)))",
            "ff04ff81bfffff04ff82013fffff04ff8201bfff80808080",
            "(0 0 0 0 0 0 (9 . 10))",
            "((9 . 10) 9 10)",
        ),
        (
            "(mod (A (B . C) . D) (list A B C D))",
            "ff04ff02ffff04ff09ffff04ff0dffff04ff07ff8080808080",
            "(1 (2 . 3) 4 5)",
            "(1 2 3 (4 5))",
        ),
        (
            "(mod ((A B)) (defconstant K 7) (list A B K))",
            "ff02ffff01ff04ff09ffff04ff15ffff04ff02ff80808080ffff04ffff0107ff018080",
            "((1 2))",
            "(1 2 7)",
        ),
        (
            "(mod (X) (defconstant DATA (1 (4 . 5) word)) (list DATA X))",
            "ff02ffff01ff04ff02ffff04ff05ff808080ffff04ffff01ff01ffff0405ff84776f726480ff018080",
            "(9)",
            "((1 (4 . 5) word) 9)",
        ),
        (
            "(mod (X) (defconstant TREE (7 . 8)) (defun-inline F _noargs (f TREE)) (list (F) X))",
            "ff02ffff01ff04ff04ffff04ff05ff808080ffff04ffff01ff0708ff018080",
            "(9)",
            "(7 9)",
        ),
        (
            "(mod (X) (defun F ARGS (list ARGS)) (F X 2))",
            "ff02ffff01ff02ff02ffff04ff02ffff04ff05ffff01ff0280808080ffff04ffff01ff04ff03ff8080ff018080",
            "(9)",
            "((9 2))",
        ),
    ] {
        let compiler = Compiler::new(
            Cow::Borrowed(source.as_bytes()),
            COMPAT_CHIA,
            OPT_REFERENCE,
            &[],
        );
        let program = compiler.compile().unwrap();
        assert_eq!(
            hex::encode(program.serialized().unwrap().as_ref()),
            expected,
            "{source}"
        );
        assert_eq!(
            program
                .run(INFINITE_COST, 0, &assemble_text(env).unwrap())
                .unwrap()
                .1,
            assemble_text(result).unwrap(),
            "{source}",
        );
    }
}

#[test]
fn test_invalid_classic_arguments() {
    use dg_xch_core::clvm::compile::Compiler;
    use std::borrow::Cow;

    for source in [
        "(mod (A (B . C D)) (list A))",
        "(mod (A (B C) (list A))",
        "(mod (A) (defun (F) (X) X) (F A))",
        "(mod (A) (defun F) (F A))",
        "(mod () (defconstant DATA (1 . 2 3)) (list DATA))",
    ] {
        let compiler = Compiler::new(Cow::Borrowed(source.as_bytes()), COMPAT_CHIA, 0, &[]);
        assert!(compiler.compile().is_err(), "{source}");
    }
    {
        let source = "(mod () (defun F ARGS (list ARGS)) (F 1))";
        let compiler = Compiler::new(Cow::Borrowed(source.as_bytes()), 0, 0, &[]);
        assert!(compiler.compile().is_err(), "{source}");
    }
}

#[test]
fn test_classic_macros() {
    use dg_xch_core::clvm::assemble::assemble_text;
    use dg_xch_core::clvm::compile::{Compiler, OPT_REFERENCE};
    use dg_xch_core::clvm::utils::INFINITE_COST;
    use std::borrow::Cow;

    const MACROS: &str = r#"(defmacro assert items
    (if (r items) (list if (f items) (c assert (r items)) (q . (x))) (f items)))
(defmacro and ARGS
    (if ARGS (qq (if (unquote (f ARGS)) (unquote (c and (r ARGS))) ())) 1))
(defmacro or ARGS
    (if ARGS (qq (if (unquote (f ARGS)) 1 (unquote (c or (r ARGS))))) 0)) "#;
    // Reference compiler 0.4.5, -O. Expansion must retain quoted data and lazy branches.
    for (source, expected, env, result) in [
        (
            "(mod (X) $MACROS (assert X (+ X 1)))",
            "ff02ffff03ff02ffff01ff10ff02ffff010180ffff01ff088080ff0180",
            "(5)",
            "6",
        ),
        (
            "(mod (X Y) $MACROS (list (and X (x)) (or X Y)))",
            "ff04ffff02ffff03ff02ffff01ff02ffff03ffff0880ffff01ff0101ff8080ff0180ff8080ff0180ffff04ffff02ffff03ff02ffff01ff0101ffff01ff02ffff03ff05ffff01ff0101ff8080ff018080ff0180ff808080",
            "(() 7)",
            "(() 1)",
        ),
        (
            "(mod (X) (defmacro twice (V) (qq (list (unquote V) (unquote V)))) (twice (+ X 1)))",
            "ff04ffff10ff02ffff010180ffff04ffff10ff02ffff010180ff808080",
            "(5)",
            "(6 6)",
        ),
        (
            "(mod (X) (defmacro literal ARGS (q . (q . (1 (4 . 5) \"123\")))) (literal X))",
            "ff01ff01ffff0405ff8331323380",
            "(5)",
            "(1 (4 . 5) \"123\")",
        ),
        (
            "(mod (X) (defmacro plus (A . REST) (list + A (f REST))) (plus X 2))",
            "ff10ff02ffff010280",
            "(5)",
            "7",
        ),
        (
            "(mod (X) (defmacro keep (V) (if 1 V (x))) (keep X))",
            "02",
            "(5)",
            "5",
        ),
        (
            "(mod (X) (defun pick ((A . B)) (list B A)) (pick X))",
            "ff02ffff01ff02ff02ffff04ff02ffff04ff05ff80808080ffff04ffff01ff04ff0dffff04ff09ff808080ff018080",
            "((5 . 6))",
            "(6 5)",
        ),
        (
            "(mod (X) (defun-inline pick ((A . B)) (list B A)) (pick X))",
            "ff04ff06ffff04ff04ff808080",
            "((5 . 6))",
            "(6 5)",
        ),
    ] {
        let source = source.replace("$MACROS", MACROS);
        let compiler = Compiler::new(
            Cow::Borrowed(source.as_bytes()),
            COMPAT_CHIA,
            OPT_REFERENCE,
            &[],
        );
        let program = compiler.compile().unwrap();
        assert_eq!(
            hex::encode(program.serialized().unwrap().as_ref()),
            expected,
            "{source}"
        );
        assert_eq!(
            program
                .run(INFINITE_COST, 0, &assemble_text(env).unwrap())
                .unwrap()
                .1,
            assemble_text(result).unwrap(),
            "{source}",
        );
    }
    let source = format!(
        "(mod (X) {MACROS} ; preserve comments during expansion\n (assert X ; lazy failure\n (+ X 1)))"
    );
    let compiler = Compiler::new(
        Cow::Borrowed(source.as_bytes()),
        COMPAT_CHIA,
        OPT_REFERENCE,
        &[],
    );
    let program = compiler.compile().unwrap();
    assert!(
        program
            .run(INFINITE_COST, 0, &assemble_text("(())").unwrap())
            .is_err()
    );
    assert_eq!(
        program
            .run(INFINITE_COST, 0, &assemble_text("(5)").unwrap())
            .unwrap()
            .1,
        assemble_text("6").unwrap()
    );
}

#[test]
fn test_invalid_classic_macros() {
    use dg_xch_core::clvm::compile::Compiler;
    use std::borrow::Cow;

    for (source, expected) in [
        (
            "(mod () (defmacro loop ARGS (c loop ARGS)) (loop))",
            "expansion limit",
        ),
        ("(mod () (defmacro F (X) X) (F))", "Macro expansion"),
        ("(mod () (defmacro F (X) X) (F 1 2))", "argument count"),
        (
            "(mod () (defmacro F X (qq (unquote 1 2))) (F))",
            "unquote argument",
        ),
        (
            "(mod () (defmacro F X (unknown X)) (F))",
            "Unsupported classic macro operator",
        ),
        ("(mod () (defmacro F X 1 2) (F))", "end of macro"),
    ] {
        let compiler = Compiler::new(Cow::Borrowed(source.as_bytes()), COMPAT_CHIA, 0, &[]);
        assert!(
            compiler
                .compile()
                .unwrap_err()
                .to_string()
                .contains(expected),
            "{source}"
        );
    }
    {
        let (flags, sigil) = (0, "");
        let source = format!("(mod () {sigil} (defmacro F X 1) (F))");
        let compiler = Compiler::new(Cow::Borrowed(source.as_bytes()), flags, 0, &[]);
        assert!(
            compiler
                .compile()
                .unwrap_err()
                .to_string()
                .contains("classic Chia compatibility")
        );
    }
}

#[test]
fn test_classic_opcode_literals() {
    use dg_xch_core::clvm::assemble::assemble_text;
    use dg_xch_core::clvm::compile::Compiler;
    use dg_xch_core::clvm::utils::INFINITE_COST;
    use std::borrow::Cow;

    // Reference compiler 0.4.5, -O. Quoted strings resembling opcodes stay literal.
    for (source, expected, result) in [
        (
            "(mod (X) (defconstant OP #a) (list OP #q #c \"#a\" X))",
            "ff02ffff01ff04ff02ffff04ffff0101ffff04ffff0104ffff04ffff01822361ffff04ff05ff808080808080ffff04ffff0102ff018080",
            "(2 1 4 \"#a\" 5)",
        ),
        (
            "(mod (X) (defmacro keep (V) V) (keep (list #a \"#a\" (q . (#q \"#q\")) X)))",
            "ff04ffff0102ffff04ffff01822361ffff04ffff01ff01ff82237180ffff04ff02ff8080808080",
            "(2 \"#a\" (1 \"#q\") 5)",
        ),
    ] {
        let compiler = Compiler::new(Cow::Borrowed(source.as_bytes()), COMPAT_CHIA, 0, &[]);
        let program = compiler.compile().unwrap();
        assert_eq!(
            hex::encode(program.serialized().unwrap().as_ref()),
            expected
        );
        assert_eq!(
            program
                .run(INFINITE_COST, 0, &assemble_text("(5)").unwrap())
                .unwrap()
                .1,
            assemble_text(result).unwrap(),
        );
    }
    let compiler = Compiler::new(
        Cow::Borrowed(b"(mod () (list #unknown))"),
        COMPAT_CHIA,
        0,
        &[],
    );
    assert_eq!(
        compiler
            .compile()
            .unwrap()
            .run(INFINITE_COST, 0, &assemble_text("()").unwrap())
            .unwrap()
            .1,
        assemble_text("(0x756e6b6e6f776e)").unwrap(),
    );
    let compiler = Compiler::new(Cow::Borrowed(b"(mod () (list #a))"), 0, 0, &[]);
    assert_eq!(
        compiler
            .compile()
            .unwrap()
            .run(INFINITE_COST, 0, &assemble_text("()").unwrap())
            .unwrap()
            .1,
        assemble_text("(2)").unwrap(),
    );
}

#[test]
fn test_classic_reference_optimizations() {
    use dg_xch_core::clvm::assemble::assemble_text;
    use dg_xch_core::clvm::compile::Compiler;
    use dg_xch_core::clvm::utils::INFINITE_COST;
    use std::borrow::Cow;

    // Reference compiler 0.4.5, -O, including classic's discarded error expressions.
    for (source, expected, result) in [
        ("(mod (X) (f (c X (x))))", "02", "5"),
        ("(mod (X) (r (c (x) X)))", "02", "5"),
        (
            "(mod (X) (list (f (q . (1 . 2))) (r (q . (1 . 2))) X))",
            "ff04ffff0101ffff04ffff0102ffff04ff02ff80808080",
            "(q 2 5)",
        ),
        (
            "(mod (X) (defun-inline F _noargs (+ X 1)) (F))",
            "ff10ff02ffff010180",
            "6",
        ),
        (
            "(mod (X) (defun-inline F (Y) (+ X Y)) (defun-inline G (X) (F 1)) (G (+ X 2)))",
            "ff10ff02ffff010180",
            "6",
        ),
        (
            "(mod (X) (list (sha256 1) (sha256 0x0102 0x03) (sha256) X))",
            "ff04ffff01a04bf5122f344554c53bde2ebb8cd2b7e3d1600ad631c385a5d7cce23c7785459affff04ffff01a0039058c6f2c0cb492c533b0a4d14ef77cc0f78abccced5287d84a1a2011cfb81ffff04ffff01a0e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855ffff04ff02ff8080808080",
            "(0x4bf5122f344554c53bde2ebb8cd2b7e3d1600ad631c385a5d7cce23c7785459a 0x039058c6f2c0cb492c533b0a4d14ef77cc0f78abccced5287d84a1a2011cfb81 0xe3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855 5)",
        ),
    ] {
        let compiler = Compiler::new(Cow::Borrowed(source.as_bytes()), COMPAT_CHIA, 0, &[]);
        let program = compiler.compile().unwrap();
        assert_eq!(
            hex::encode(program.serialized().unwrap().as_ref()),
            expected,
            "{source}"
        );
        assert_eq!(
            program
                .run(INFINITE_COST, 0, &assemble_text("(5)").unwrap())
                .unwrap()
                .1,
            assemble_text(result).unwrap(),
            "{source}",
        );
    }
    // DG retains eager evaluation; the classic rewrite must not leak into this path.
    for source in ["(mod (X) (f (c X (x))))", "(mod (X) (r (c (x) X)))"] {
        let compiler = Compiler::new(Cow::Borrowed(source.as_bytes()), 0, 0, &[]);
        assert!(
            compiler
                .compile()
                .unwrap()
                .run(INFINITE_COST, 0, &assemble_text("(5)").unwrap())
                .is_err()
        );
    }
    for flags in [0, COMPAT_CHIA] {
        let compiler = Compiler::new(Cow::Borrowed(b"(mod () (sha256 (q . (1))))"), flags, 0, &[]);
        assert!(
            compiler
                .compile()
                .unwrap()
                .run(INFINITE_COST, 0, &assemble_text("()").unwrap())
                .is_err()
        );
    }
}

#[test]
fn test_compat_chia_flags() {
    use dg_xch_core::clvm::compile::{
        COMPAT_CHIA, Compiler, INLINE_CONSTS, INLINE_DEFUNS, NESTED_ASSIGN, OPT_COST,
        OPT_REFERENCE, OPT_SIZE,
    };
    use std::borrow::Cow;

    for (flags, level) in [
        (COMPAT_CHIA | INLINE_CONSTS, OPT_REFERENCE),
        (COMPAT_CHIA | INLINE_DEFUNS, OPT_REFERENCE),
        (COMPAT_CHIA | NESTED_ASSIGN, OPT_REFERENCE),
        (COMPAT_CHIA, OPT_SIZE),
        (COMPAT_CHIA, OPT_COST),
    ] {
        let compiler = Compiler::new(Cow::Borrowed(b"(mod (X) (+ X 1))"), flags, level, &[]);
        assert!(
            compiler
                .compile()
                .unwrap_err()
                .to_string()
                .contains("no optimization flags")
        );
    }
    for version in [25, 26] {
        let source = format!("(mod (X) (include *standard-cl-{version}*) (+ X 1))");
        let compiler = Compiler::new(
            Cow::Borrowed(source.as_bytes()),
            COMPAT_CHIA,
            OPT_REFERENCE,
            &[],
        );
        assert!(compiler.compile().is_ok());
        let compiler = Compiler::new(Cow::Borrowed(source.as_bytes()), 0, OPT_REFERENCE, &[]);
        assert!(
            compiler
                .compile()
                .unwrap_err()
                .to_string()
                .contains("require COMPAT_CHIA")
        );
    }
    for flags in [0, COMPAT_CHIA] {
        let compiler = Compiler::new(
            Cow::Borrowed(b"(mod (X) (+ X 1))"),
            flags,
            OPT_REFERENCE,
            &[],
        );
        assert_eq!(
            hex::encode(compiler.compile().unwrap().serialized().unwrap().as_ref()),
            "ff10ff02ffff010180"
        );
    }
}

#[test]
fn test_dg_default_optimization() {
    use dg_xch_core::clvm::assemble::assemble_text;
    use dg_xch_core::clvm::compile::{Compiler, OPT_DEFAULT, OPT_REFERENCE, OPT_SIZE};
    use dg_xch_core::clvm::utils::INFINITE_COST;
    use std::borrow::Cow;

    let source = b"(mod (X) (if X (list X) ()))";
    let chia_source = b"(mod (X) (include *standard-cl-26*) (if X (list X) ()))";
    let dg = Compiler::new(Cow::Borrowed(source), 0, OPT_DEFAULT, &[]);
    let size = Compiler::new(Cow::Borrowed(source), 0, OPT_SIZE, &[]);
    let chia = Compiler::new(Cow::Borrowed(chia_source), COMPAT_CHIA, OPT_REFERENCE, &[]);
    let dg = dg.compile().unwrap();
    let size = size.compile().unwrap();
    let chia = chia.compile().unwrap();
    assert_eq!(dg.serialized().unwrap(), size.serialized().unwrap());
    assert!(dg.serialized().unwrap().as_ref().len() < chia.serialized().unwrap().as_ref().len());
    for (env, expected) in [("(5)", "(5)"), ("(())", "()")] {
        let env = assemble_text(env).unwrap();
        let expected = assemble_text(expected).unwrap();
        for program in [&dg, &size, &chia] {
            assert_eq!(program.run(INFINITE_COST, 0, &env).unwrap().1, expected);
        }
    }
}

#[test]
fn test_large_argument_paths() {
    use dg_xch_core::clvm::assemble::assemble_text;
    use dg_xch_core::clvm::compile::Compiler;
    use dg_xch_core::clvm::utils::INFINITE_COST;
    use std::borrow::Cow;

    for count in [9, 70, 260] {
        let names = (0..count)
            .map(|i| format!("A{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        let values = (0..count)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(" ");
        let last = count - 1;
        for (source, args) in [
            (
                format!("(mod ({names}) (list A0 A{last}))"),
                format!("({values})"),
            ),
            (
                format!("(mod () (defun select ({names}) (list A0 A{last})) (select {values}))"),
                "()".to_string(),
            ),
        ] {
            let compiler = Compiler::new(Cow::Borrowed(source.as_bytes()), 0, 0, &[]);
            let prog = compiler.compile().unwrap();
            let (_, result) = prog
                .run(INFINITE_COST, 0, &assemble_text(&args).unwrap())
                .unwrap();
            assert_eq!(
                assemble_text(&format!("(0 {last})")).unwrap(),
                result,
                "{count} arguments"
            );
        }
    }
}

#[test]
fn test_large_constant_paths() {
    use dg_xch_core::clvm::assemble::assemble_text;
    use dg_xch_core::clvm::compile::Compiler;
    use dg_xch_core::clvm::utils::INFINITE_COST;
    use std::borrow::Cow;

    for count in [9, 70, 260] {
        let constants = (0..count)
            .map(|i| format!("(defconstant C{i} {i})"))
            .collect::<Vec<_>>()
            .join(" ");
        let last = count - 1;
        let source =
            format!("(mod (X) {constants} (defun select (Y) (list C0 C{last} Y)) (select X))");
        let compiler = Compiler::new(Cow::Borrowed(source.as_bytes()), 0, 0, &[]);
        let prog = compiler.compile().unwrap();
        let (_, result) = prog
            .run(INFINITE_COST, 0, &assemble_text("(5)").unwrap())
            .unwrap();
        assert_eq!(
            assemble_text(&format!("(0 {last} 5)")).unwrap(),
            result,
            "{count} constants"
        );
    }
}

#[test]
fn test_function_paths() {
    use dg_xch_core::clvm::assemble::assemble_text;
    use dg_xch_core::clvm::compile::Compiler;
    use dg_xch_core::clvm::utils::INFINITE_COST;
    use std::borrow::Cow;

    for count in [1, 2, 3, 70, 260] {
        for constants in [
            "",
            "(defconstant C 10)",
            "(include *standard-cl-26*)",
            "(include *standard-cl-26*) (defconstant C 10)",
        ] {
            let functions = (0..count)
                .map(|i| format!("(defun F{i} (Y) (+ Y {i}))"))
                .collect::<Vec<_>>()
                .join(" ");
            let last = count - 1;
            let middle = count / 2;
            let source = format!(
                "(mod (X) {constants} {functions} (assign Y X (list (F0 Y) (F{middle} Y) (F{last} Y))))"
            );
            let compiler = Compiler::new(Cow::Borrowed(source.as_bytes()), COMPAT_CHIA, 0, &[]);
            let prog = compiler.compile().unwrap();
            let (_, result) = prog
                .run(INFINITE_COST, 0, &assemble_text("(5)").unwrap())
                .unwrap();
            assert_eq!(
                assemble_text(&format!("(5 {} {})", middle + 5, last + 5)).unwrap(),
                result,
                "{count} functions, constants: {constants}"
            );
        }
    }
}

#[test]
fn test_function_tree_reference_bytes() {
    use dg_xch_core::clvm::compile::Compiler;
    use std::borrow::Cow;

    let source = b"(mod (X) (include *standard-cl-26*)
        (defun one (Y) (+ Y 1))
        (defun two (Y) (+ Y 2))
        (defun three (Y) (+ Y 3))
        (list (one X) (two X) (three X)))";
    let compiler = Compiler::new(Cow::Borrowed(source), COMPAT_CHIA, 0, &[]);
    let prog = compiler.compile().unwrap();
    // Output from chialisp 0.4.5.
    assert_eq!(
        format!("{}", prog.serialized().unwrap()),
        "0xff02ffff01ff04ffff02ff04ffff04ff02ffff04ff05ff80808080ffff04ffff02ff0affff04ff02ffff04ff05ff80808080ffff04ffff02ff0effff04ff02ffff04ff05ff80808080ff80808080ffff04ffff01ffff10ff05ffff010180ffff10ff05ffff010280ff10ff05ffff010380ff018080"
    );
}

#[test]
fn test_modern_reference_bytes() {
    use dg_xch_core::clvm::compile::Compiler;
    use std::borrow::Cow;

    // Outputs from chialisp 0.4.5.
    let cases = [
        (
            "(mod (X) (include *standard-cl-26*) (defconstant C 10) (+ X C))",
            "0xff10ff02ffff010a80",
        ),
        (
            "(mod (X) (include *standard-cl-26*) (assign (A (B . C) . D) X (list A B C D)))",
            "0xff04ff04ffff04ff12ffff04ff1affff04ff0eff8080808080",
        ),
        (
            "(mod (X) (include *standard-cl-26*) (defun unused (Y) (+ Y 2)) (defun first (Y) (second Y)) (defun second (Y) (+ Y 1)) (first X))",
            "0xff02ffff01ff02ff04ffff04ff02ffff04ff05ff80808080ffff04ffff01ffff02ff06ffff04ff02ffff04ff05ff80808080ff10ff05ffff010180ff018080",
        ),
        (
            "(mod (X) (include *standard-cl-26*) (defun unused (Y) (+ Y 2)) (defun-inline wrap (Y) (used Y)) (defun used (Y) (+ Y 1)) (wrap X))",
            "0xff02ffff01ff02ff02ffff04ff02ffff04ff05ff80808080ffff04ffff01ff10ff05ffff010180ff018080",
        ),
        (
            "(mod (P H A) (include *standard-cl-26*) (coinid P H A))",
            "0xff30ff02ff05ff0b80",
        ),
        (
            "(mod (X) (include *standard-cl-26*) (q . (X (assign A 1 A) . 7)))",
            "0xff01ff58ffff8661737369676eff41ff01ff418007",
        ),
        (
            "(mod (X) (include *standard-cl-26*) (quote (X . 7)))",
            "0xff01ff5807",
        ),
        ("(mod (X) (include *standard-cl-26*) (q . ()))", "0x80"),
        (
            "(mod (X) (include *standard-cl-26*) (if X () 0))",
            "0xff02ffff03ff02ffff01ff0180ffff01ff018080ff0180",
        ),
        (
            "(mod (X) (include *standard-cl-26*) (if (not X) (+ X 1) (- X 1)))",
            "0xff02ffff03ff02ffff01ff11ff02ffff010180ffff01ff10ff02ffff01018080ff0180",
        ),
        (
            "(mod (X Y) (include *standard-cl-26*) (assign A (+ X 1) B (+ A 2) C (+ Y 3) D (+ B C) (list A B C D)))",
            "0xff02ffff01ff02ff04ffff04ff02ffff04ff03ffff04ffff10ff0bffff010380ffff04ffff10ff05ffff010180ff808080808080ffff04ffff01ffff02ff06ffff04ff02ffff04ff03ffff04ffff10ff17ffff010280ff8080808080ff04ff2dffff04ff0bffff04ff15ffff04ffff10ff0bff1580ff8080808080ff018080",
        ),
        (
            "(mod (X) (include *standard-cl-26*) (defun-inline F (Y) (assign A (+ Y 1) (list A Y))) (defun G (Y) (+ Y 2)) (list (F X) (G X)))",
            "0xff02ffff01ff04ffff04ffff10ff05ffff010180ffff04ffff05ffff04ff05ff808080ff808080ffff04ffff02ff02ffff04ff02ffff04ff05ff80808080ff808080ffff04ffff01ff10ff05ffff010280ff018080",
        ),
        (
            "(mod (X) (include *standard-cl-26*) (q 1 2))",
            "0xff01ff01ff0280",
        ),
        (
            "(mod (X) (include *standard-cl-26*) (q . (f . 1)))",
            "0xff01ff6601",
        ),
        (
            "(mod (X) (include *standard-cl-26*) (list \"\" X))",
            "0xff04ff80ffff04ff02ff808080",
        ),
        (
            "(mod (X N) (include *standard-cl-26*) (defun walk (A N) (if (> N 0) (assign R (walk (r A) (- N 1)) (list (f A) (r A) (f R))) (list (f A) (r A)))) (walk X N))",
            "0xff02ffff01ff02ff04ffff04ff02ffff04ff05ffff04ff0bff8080808080ffff04ffff01ffff02ff06ffff04ff02ffff04ff03ffff04ff0dffff04ff09ff808080808080ff02ffff03ffff15ff15ff8080ffff01ff04ff17ffff04ff0bffff04ffff05ffff02ff04ffff04ff02ffff04ff0bffff04ffff11ff15ffff010180ff808080808080ff80808080ffff01ff04ff17ffff04ff0bff80808080ff0180ff018080",
        ),
        (
            "(mod (X) (include *standard-cl-26*) (defun F (Y) (if Y (list (f Y) (+ (f Y) 1)) 0)) (F X))",
            "0xff02ffff01ff02ff02ffff04ff02ffff04ff05ff80808080ffff04ffff01ff02ffff03ff05ffff01ff04ff09ffff04ffff10ff09ffff010180ff808080ffff01ff018080ff0180ff018080",
        ),
        (
            "(mod (X) (include *standard-cl-26*) (defun F (Y) (list (sha256 Y) (sha256 Y))) (F X))",
            "0xff02ffff01ff02ff02ffff04ff02ffff04ff05ff80808080ffff04ffff01ff04ffff0bff0580ffff04ffff0bff0580ff808080ff018080",
        ),
        (
            "(mod (X) (include *standard-cl-26*) (list \"text with spaces; (literal)\" X))",
            "0xff04ffff019b746578742077697468207370616365733b20286c69746572616c29ffff04ff02ff808080",
        ),
    ];
    // Compatibility targets clean runs. Prior compilations and their order must
    // never affect generated symbols or the resulting bytecode.
    for &(source, expected) in cases.iter().chain(cases.iter().rev()).chain(cases.iter()) {
        let compiler = Compiler::new(Cow::Borrowed(source.as_bytes()), COMPAT_CHIA, 0, &[]);
        let prog = compiler.compile().unwrap();
        assert_eq!(
            format!("{}", prog.serialized().unwrap()),
            expected,
            "{source}"
        );
    }
}

#[test]
fn test_shared_expressions_and_optimization_levels() {
    use dg_xch_core::clvm::assemble::assemble_text;
    use dg_xch_core::clvm::compile::{Compiler, NESTED_ASSIGN, OPT_COST, OPT_REFERENCE, OPT_SIZE};
    use dg_xch_core::clvm::utils::INFINITE_COST;
    use std::borrow::Cow;

    // Compare values in every mode, including lazy branches and shadowed names.
    for (source, env, expected) in [
        (
            "(mod (X N) (include *standard-cl-26*) (defun walk (A N) (if (> N 0) (assign R (walk (r A) (- N 1)) (list (f A) (f R) (f A))) (list (f A)))) (walk X N))",
            "((10 20 30) 1)",
            "(>s 20 10)",
        ),
        (
            "(mod (X N) (include *standard-cl-26*) (defun walk (A N) (if (> N 0) (assign R (walk (r A) (- N 1)) (list (f A) (r A) (f R))) (list (f A) (r A)))) (walk X N))",
            "((10 20 30) 1)",
            "(>s (divmod 30) 20)",
        ),
        (
            "(mod (X) (include *standard-cl-26*) (defun F (Y) (if Y (list (f Y) (+ (f Y) 1)) 0)) (F X))",
            "(())",
            "()",
        ),
        (
            "(mod (X) (include *standard-cl-26*) (defun F (Y) (list (sha256 Y) (sha256 Y))) (F X))",
            "(5)",
            "(0xe77b9a9ae9e30b0dbdb6f510a264ef9de781501d7b6b92ae89eb059c5ab743db 0xe77b9a9ae9e30b0dbdb6f510a264ef9de781501d7b6b92ae89eb059c5ab743db)",
        ),
        (
            "(mod (X) (include *standard-cl-26*) (defun F (Y) (list (f Y) (assign Y (q . (30)) (list (f Y) (f Y))))) (F X))",
            "((10 20 30))",
            "(>s (pubkey_for_exp 30))",
        ),
        (
            "(mod (X) (include *standard-cl-26*) (list \"text with spaces; (literal)\" X))",
            "(5)",
            "(\"text with spaces; (literal)\" 5)",
        ),
    ] {
        let mut reference_size = 0;
        for level in [OPT_REFERENCE, OPT_SIZE, OPT_COST] {
            let compiler = Compiler::new(Cow::Borrowed(source.as_bytes()), COMPAT_CHIA, level, &[]);
            let program = compiler.compile().unwrap();
            let bytes = program.serialized().unwrap();
            if level == OPT_REFERENCE {
                reference_size = bytes.as_ref().len();
            } else if level == OPT_SIZE {
                assert!(bytes.as_ref().len() <= reference_size, "{source}");
            } else {
                let nested = Compiler::new(
                    Cow::Borrowed(source.as_bytes()),
                    NESTED_ASSIGN | COMPAT_CHIA,
                    OPT_REFERENCE,
                    &[],
                );
                assert_eq!(bytes, nested.compile().unwrap().serialized().unwrap());
            }
            let (_, result) = program
                .run(INFINITE_COST, 0, &assemble_text(env).unwrap())
                .unwrap();
            assert_eq!(
                result,
                assemble_text(expected).unwrap(),
                "level {level}: {source}"
            );
        }
    }
    let compiler = Compiler::new(Cow::Borrowed(b"(mod () (list 1))"), COMPAT_CHIA, 3, &[]);
    assert!(compiler.compile().is_err());
}

#[test]
fn test_quoted_values() {
    use dg_xch_core::clvm::assemble::assemble_text;
    use dg_xch_core::clvm::compile::{Compiler, NESTED_ASSIGN};
    use dg_xch_core::clvm::utils::INFINITE_COST;
    use std::borrow::Cow;

    for flags in [0, NESTED_ASSIGN] {
        for (body, expected) in [
            ("(q . X)", "X"),
            ("(q . (X (assign A 1 A) . 7))", "(X (assign A 1 A) . 7)"),
            ("(quote (X . 7))", "(X . 7)"),
            ("(q 1 2)", "(1 2)"),
            ("(q . ())", "()"),
        ] {
            let source = format!("(mod (X) (include *standard-cl-26*) {body})");
            let compiler = Compiler::new(
                Cow::Borrowed(source.as_bytes()),
                flags | COMPAT_CHIA,
                0,
                &[],
            );
            let program = compiler.compile().unwrap();
            let (_, result) = program
                .run(INFINITE_COST, 0, &assemble_text("(5)").unwrap())
                .unwrap();
            assert_eq!(assemble_text(expected).unwrap(), result, "{body}");
        }
    }
    for body in [
        "(list \"unterminated)",
        "(q .)",
        "(q . 1 2)",
        "(q . (1 . 2 3))",
        "(quote)",
        "(quote 1 2)",
    ] {
        let source = format!("(mod () (include *standard-cl-26*) {body})");
        let compiler = Compiler::new(Cow::Borrowed(source.as_bytes()), COMPAT_CHIA, 0, &[]);
        assert!(compiler.compile().is_err(), "{body}");
    }
}

#[test]
fn test_assign_reference_bytes() {
    use dg_xch_core::clvm::assemble::assemble_text;
    use dg_xch_core::clvm::compile::{Compiler, NESTED_ASSIGN};
    use dg_xch_core::clvm::utils::INFINITE_COST;
    use std::borrow::Cow;

    // Outputs from chialisp 0.4.5.
    for (source, expected) in [
        (
            "(mod (X) (include *standard-cl-26*) (assign Y (+ X 1) (+ Y Y)))",
            "0xff10ffff10ff02ffff010180ffff10ff02ffff01018080",
        ),
        (
            "(mod (X) (include *standard-cl-26*) (assign Y (sha256 X) (list Y Y)))",
            "0xff04ffff0bff0280ffff04ffff0bff0280ff808080",
        ),
        (
            "(mod (X) (include *standard-cl-26*) (assign A (+ X 1) B (+ A 2) (list A B)))",
            "0xff02ffff01ff02ff02ffff04ff02ffff04ff03ffff04ffff10ff05ffff010180ff8080808080ffff04ffff01ff04ff0bffff04ffff10ff0bffff010280ff808080ff018080",
        ),
        (
            "(mod (X) (include *standard-cl-26*) (defun foo (Y) (sha256 Y)) (assign A (foo X) B (foo (+ X 1)) (list A A B B)))",
            "0xff02ffff01ff02ff06ffff04ff02ffff04ff03ffff04ffff02ff04ffff04ff02ffff04ffff10ff05ffff010180ff80808080ffff04ffff02ff04ffff04ff02ffff04ff05ff80808080ff808080808080ffff04ffff01ffff0bff0580ff04ff17ffff04ff17ffff04ff0bffff04ff0bff8080808080ff018080",
        ),
        (
            "(mod (X) (include *standard-cl-26*) (defun foo (Y) (assign Z (+ Y 1) (+ Z Z))) (foo X))",
            "0xff02ffff01ff02ff02ffff04ff02ffff04ff05ff80808080ffff04ffff01ff10ffff10ff05ffff010180ffff10ff05ffff01018080ff018080",
        ),
        (
            "(mod (X) (include *standard-cl-26*) (assign (A B) (list X X) (+ A B)))",
            "0xff10ffff05ffff04ff02ffff04ff02ff80808080ffff05ffff06ffff04ff02ffff04ff02ff808080808080",
        ),
        (
            "(mod (X) (include *standard-cl-26*) (defun-inline foo (Y . REST) (assign A (+ Y 1) (list A REST))) (foo X 2 3))",
            "0xff04ffff10ff02ffff010180ffff04ffff06ffff04ff02ffff04ffff0102ffff04ffff0103ff8080808080ff808080",
        ),
        (
            "(mod (X) (include *standard-cl-26*) (assign A X (list A X)))",
            "0xff04ff02ffff04ff02ff808080",
        ),
    ] {
        let compiler = Compiler::new(Cow::Borrowed(source.as_bytes()), COMPAT_CHIA, 0, &[]);
        let prog = compiler.compile().unwrap();
        assert_eq!(
            format!("{}", prog.serialized().unwrap()),
            expected,
            "{source}"
        );
        let nested = Compiler::new(
            Cow::Borrowed(source.as_bytes()),
            NESTED_ASSIGN | COMPAT_CHIA,
            0,
            &[],
        );
        let nested = nested.compile().unwrap();
        for args in ["(0)", "(5)", "(-3)"] {
            let args = assemble_text(args).unwrap();
            assert_eq!(
                prog.run(INFINITE_COST, 0, &args).unwrap().1,
                nested.run(INFINITE_COST, 0, &args).unwrap().1,
                "{source}, args {args}"
            );
        }
    }
}

#[test]
fn test_assign() {
    use dg_xch_core::clvm::assemble::assemble_text;
    use dg_xch_core::clvm::compile::{Compiler, INLINE_DEFUNS, NESTED_ASSIGN};
    use dg_xch_core::clvm::utils::INFINITE_COST;
    use std::borrow::Cow;

    for (sigil, flags) in [
        ("", 0),
        ("", INLINE_DEFUNS),
        ("(include *standard-cl-26*)", 0),
        ("(include *standard-cl-26*)", INLINE_DEFUNS),
        ("(include *standard-cl-26*)", NESTED_ASSIGN),
    ] {
        for (declarations, body, expected) in [
            (
                "",
                "(assign B (+ A 1) A (+ VALUE 2) (list A B VALUE))",
                "(7 8 5)",
            ),
            (
                "",
                "(list (assign VALUE 9 (assign Y (+ VALUE 1) (+ VALUE Y))) VALUE)",
                "(19 5)",
            ),
            (
                "",
                "(assign A (assign C (+ B 1) C) B (+ VALUE 1) (list A B))",
                "(7 6)",
            ),
            ("", "(assign unused (x) (+ VALUE 1))", "6"),
            ("", "(assign (+ VALUE 1))", "6"),
            (
                "(defun helper (X) (+ X 2))",
                "(assign A (helper VALUE) B (helper A) (list A B VALUE))",
                "(7 9 5)",
            ),
            (
                "(defun-inline helper (X) (assign A (+ X 1) (+ A X)))",
                "(helper (+ VALUE 2))",
                "15",
            ),
            (
                "(defun helper (X) (if X (assign Y (- X 1) (+ 1 (helper Y))) 0))",
                "(helper VALUE)",
                "5",
            ),
            (
                "(defconstant INC 2) (defun helper (X) (if X (assign Y (- X 1) (+ INC (helper Y))) 0))",
                "(helper VALUE)",
                "10",
            ),
        ] {
            let source = format!("(mod (VALUE) {sigil} {declarations} {body})");
            let compiler = Compiler::new(
                Cow::Borrowed(source.as_bytes()),
                flags | if sigil.is_empty() { 0 } else { COMPAT_CHIA },
                0,
                &[],
            );
            let prog = compiler.compile().unwrap();
            let (_, result) = prog
                .run(INFINITE_COST, 0, &assemble_text("(5)").unwrap())
                .unwrap();
            assert_eq!(
                assemble_text(expected).unwrap(),
                result,
                "{source}, flags {flags}"
            );
        }
    }
}

#[test]
fn test_assign_destructuring() {
    use dg_xch_core::clvm::assemble::assemble_text;
    use dg_xch_core::clvm::compile::{Compiler, NESTED_ASSIGN};
    use dg_xch_core::clvm::utils::INFINITE_COST;
    use std::borrow::Cow;

    for (sigil, flags) in [
        ("", 0),
        ("(include *standard-cl-26*)", 0),
        ("(include *standard-cl-26*)", NESTED_ASSIGN),
    ] {
        for (body, args, expected) in [
            ("(assign (A B) VALUE (list A B))", "((2 3))", "(2 3)"),
            (
                "(assign ((A . B) C . TAIL) VALUE (list A B C TAIL))",
                "(((2 . 3) 4 5 6))",
                "(2 3 4 (5 6))",
            ),
            (
                "(assign SUM (+ A B) (A B) VALUE (list SUM A B))",
                "((2 3))",
                "(5 2 3)",
            ),
            (
                "(assign (A B) VALUE (list (assign (A C) (list 9 8) (+ A C)) A B))",
                "((2 3))",
                "(17 2 3)",
            ),
            ("(assign () (x) (list))", "(0)", "()"),
        ] {
            let source = format!("(mod (VALUE) {sigil} {body})");
            let compiler = Compiler::new(
                Cow::Borrowed(source.as_bytes()),
                flags | COMPAT_CHIA,
                0,
                &[],
            );
            let prog = compiler.compile().unwrap();
            let (_, result) = prog
                .run(INFINITE_COST, 0, &assemble_text(args).unwrap())
                .unwrap();
            assert_eq!(assemble_text(expected).unwrap(), result, "{body}");
        }
        let names = (0..70)
            .map(|i| format!("A{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        let values = (0..70).map(|i| i.to_string()).collect::<Vec<_>>().join(" ");
        let source = format!("(mod (VALUE) {sigil} (assign ({names}) VALUE (list A0 A69)))");
        let compiler = Compiler::new(
            Cow::Borrowed(source.as_bytes()),
            flags | COMPAT_CHIA,
            0,
            &[],
        );
        let prog = compiler.compile().unwrap();
        let (_, result) = prog
            .run(
                INFINITE_COST,
                0,
                &assemble_text(&format!("(({values}))")).unwrap(),
            )
            .unwrap();
        assert_eq!(assemble_text("(0 69)").unwrap(), result);
    }
}

#[test]
fn test_invalid_assign() {
    use dg_xch_core::clvm::compile::{Compiler, NESTED_ASSIGN};
    use std::borrow::Cow;

    for (sigil, flags) in [
        ("", 0),
        ("(include *standard-cl-26*)", 0),
        ("(include *standard-cl-26*)", NESTED_ASSIGN),
    ] {
        for body in [
            "(assign)",
            "(assign A 1)",
            "(assign A 1 A 2 A)",
            "(assign A B B A A)",
            "(assign A (+ A 1) A)",
            "(assign (A .) VALUE A)",
            "(assign (. A) VALUE A)",
            "(assign (A . B C) VALUE A)",
            "(assign (A A) VALUE A)",
            "(assign (A B) VALUE B 2 A)",
            "(assign (A B) (list C 1) C A B)",
            "(assign 1 VALUE 1)",
        ] {
            let source = format!("(mod (VALUE) {sigil} {body})");
            let compiler = Compiler::new(
                Cow::Borrowed(source.as_bytes()),
                flags | COMPAT_CHIA,
                0,
                &[],
            );
            assert!(compiler.compile().is_err(), "{body}");
        }
    }
}

#[test]
fn test_assign_evaluates_once() {
    use dg_xch_core::clvm::compile::{Compiler, OPT_COST};
    use dg_xch_core::clvm::program::Program;
    use dg_xch_core::clvm::sexp::SExp;
    use dg_xch_core::clvm::utils::INFINITE_COST;
    use std::borrow::Cow;

    let mut increases = vec![];
    for body in [
        "(assign HASH (sha256 VALUE) (list HASH HASH))",
        "(assign (HASH . REST) (c (sha256 VALUE) ()) (list HASH HASH))",
        "(list (sha256 VALUE) (sha256 VALUE))",
    ] {
        let source = format!("(mod (VALUE) {body})");
        let compiler = Compiler::new(Cow::Borrowed(source.as_bytes()), 0, OPT_COST, &[]);
        let prog = compiler.compile().unwrap();
        let mut costs = vec![];
        for size in [0, 4096] {
            let args = Program::to(&[SExp::from(vec![0u8; size])]);
            costs.push(prog.run(INFINITE_COST, 0, &args).unwrap().0);
        }
        increases.push(costs[1] - costs[0]);
    }
    assert!(increases[0] > 0);
    assert_eq!(increases[0], increases[1]);
    assert_eq!(increases[0] * 2, increases[2]);
}

#[test]
fn test_nested_assign_cost() {
    use dg_xch_core::clvm::compile::{Compiler, NESTED_ASSIGN};
    use dg_xch_core::clvm::program::Program;
    use dg_xch_core::clvm::sexp::SExp;
    use dg_xch_core::clvm::utils::INFINITE_COST;
    use std::borrow::Cow;

    let source = b"(mod (VALUE) (include *standard-cl-26*)
        (assign HASH (sha256 VALUE) (list HASH HASH)))";
    let mut increases = vec![];
    for flags in [0, NESTED_ASSIGN] {
        let compiler = Compiler::new(Cow::Borrowed(source), flags | COMPAT_CHIA, 0, &[]);
        let prog = compiler.compile().unwrap();
        let mut costs = vec![];
        for size in [0, 4096] {
            let args = Program::to(&[SExp::from(vec![0u8; size])]);
            costs.push(prog.run(INFINITE_COST, 0, &args).unwrap().0);
        }
        increases.push(costs[1] - costs[0]);
    }
    assert!(increases[1] > 0);
    assert_eq!(increases[0], increases[1] * 2);
}

#[test]
fn test_inline_function_calls() {
    use dg_xch_core::clvm::assemble::assemble_text;
    use dg_xch_core::clvm::compile::{Compiler, INLINE_DEFUNS};
    use dg_xch_core::clvm::utils::INFINITE_COST;
    use std::borrow::Cow;

    for flags in [0, INLINE_DEFUNS] {
        for (source, expected) in [
            (
                "(mod (VALUE)
                (defun double (num) (* num 2))
                (defun-inline wrap (num) (double (+ num 1)))
                (defun-inline outer (num) (wrap (+ num 1)))
                (list (outer VALUE) (wrap (wrap VALUE))))",
                "(14 26)",
            ),
            (
                "(mod (VALUE)
                (defun count-down (num) (if num (step (- num 1)) 0))
                (defun-inline step (num) (+ 1 (count-down num)))
                (step VALUE))",
                "6",
            ),
        ] {
            let compiler = Compiler::new(Cow::Borrowed(source.as_bytes()), flags, 0, &[]);
            let prog = compiler.compile().unwrap();
            let (_, result) = prog
                .run(INFINITE_COST, 0, &assemble_text("(5)").unwrap())
                .unwrap();
            assert_eq!(
                assemble_text(expected).unwrap(),
                result,
                "{source}, flags {flags}"
            );
            assert!(compiler.inline_stack.lock().is_empty());
        }
    }
}

#[test]
fn test_recursive_inline_functions() {
    use dg_xch_core::clvm::compile::Compiler;
    use std::borrow::Cow;
    use std::io::ErrorKind;

    for source in [
        "(mod (num) (defun-inline recurse (n) (recurse n)) (recurse num))",
        "(mod (num) (defun-inline first (n) (second n)) (defun-inline second (n) (first n)) (first num))",
    ] {
        let compiler = Compiler::new(Cow::Borrowed(source.as_bytes()), 0, 0, &[]);
        let error = compiler.compile().unwrap_err();
        assert_eq!(ErrorKind::InvalidInput, error.kind());
        assert!(error.to_string().starts_with("Recursive inline function:"));
        assert!(compiler.inline_stack.lock().is_empty());
    }
}

#[test]
fn test_embed_file() {
    use dg_xch_core::clvm::assemble::assemble_text;
    use dg_xch_core::clvm::compile::Compiler;
    use std::borrow::Cow;
    use std::fs;

    let dir = std::env::temp_dir().join(format!("dg-embed-{}", uuid::Uuid::new_v4()));
    let output = dir.join("output");
    fs::create_dir_all(&output).unwrap();
    fs::write(
        dir.join("binding.clib"),
        "((embed-file DATA bin \"value.bin\"))",
    )
    .unwrap();
    let include_dirs = [dir.to_str().unwrap(), output.to_str().unwrap()];
    for data in [&b""[..], &b"\x00\x00\xff\x80 \n;("[..], &b"123"[..]] {
        fs::write(output.join("value.bin"), data).unwrap();
        let compiler = Compiler::new(
            Cow::Borrowed(b"(mod (input) (include binding.clib) (sha256 input DATA))"),
            0,
            0,
            &include_dirs,
        );
        let prog = compiler.compile().unwrap();
        let value = if data.is_empty() {
            "()".to_string()
        } else {
            format!("0x{}", hex::encode(data))
        };
        let expected =
            assemble_text(&format!("(a (q . (sha256 5 2)) (c (q . {value}) 1))")).unwrap();
        assert_eq!(
            expected.serialized().unwrap().to_bytes(),
            prog.serialized().unwrap().to_bytes()
        );
    }
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn test_embedded_function_tree_reference_bytes() {
    use dg_xch_core::clvm::assemble::assemble_text;
    use dg_xch_core::clvm::compile::{Compiler, NESTED_ASSIGN};
    use dg_xch_core::clvm::utils::INFINITE_COST;
    use std::borrow::Cow;
    use std::fs;

    let dir = std::env::temp_dir().join(format!("dg-embed-table-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("first.bin"), [0, 1, 255]).unwrap();
    fs::write(dir.join("second.bin"), [128, 0, 2]).unwrap();
    let include_dirs = [dir.to_str().unwrap()];
    // Outputs from chialisp 0.4.5.
    for (source, expected) in [
        (
            r#"(mod (X) (include *standard-cl-26*) (embed-file FIRST bin "first.bin") (embed-file SECOND bin "second.bin") (list X FIRST SECOND))"#,
            "0xff02ffff01ff04ff05ffff04ff04ffff04ff06ff80808080ffff04ffff01ff830001ff83800002ff018080",
        ),
        (
            r#"(mod (X) (include *standard-cl-26*) (defun add (Y) (+ Y 1)) (embed-file FIRST bin "first.bin") (defun hash (Y) (sha256 Y SECOND)) (embed-file SECOND bin "second.bin") (list (add X) FIRST (hash X) SECOND))"#,
            "0xff02ffff01ff04ffff02ff08ffff04ff02ffff04ff05ff80808080ffff04ff0cffff04ffff02ff0affff04ff02ffff04ff05ff80808080ffff04ff0eff8080808080ffff04ffff01ffffff10ff05ffff010180830001ffffff0bff05ff0e8083800002ff018080",
        ),
    ] {
        let compiler = Compiler::new(
            Cow::Borrowed(source.as_bytes()),
            COMPAT_CHIA,
            0,
            &include_dirs,
        );
        let prog = compiler.compile().unwrap();
        assert_eq!(
            format!("{}", prog.serialized().unwrap()),
            expected,
            "{source}"
        );
        let nested = Compiler::new(
            Cow::Borrowed(source.as_bytes()),
            NESTED_ASSIGN | COMPAT_CHIA,
            0,
            &include_dirs,
        );
        let nested = nested.compile().unwrap();
        let args = assemble_text("(5)").unwrap();
        assert_eq!(
            prog.run(INFINITE_COST, 0, &args).unwrap().1,
            nested.run(INFINITE_COST, 0, &args).unwrap().1
        );
    }
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn test_invalid_embed_file() {
    use dg_xch_core::clvm::compile::Compiler;
    use std::borrow::Cow;
    use std::io::ErrorKind;

    for (declaration, kind) in [
        ("(embed-file DATA bin missing.bin)", ErrorKind::NotFound),
        ("(embed-file DATA hex missing.bin)", ErrorKind::NotFound),
        ("(embed-file DATA bin)", ErrorKind::InvalidInput),
        (
            "(embed-file DATA bin missing.bin extra)",
            ErrorKind::InvalidInput,
        ),
        ("(embed-file () bin missing.bin)", ErrorKind::InvalidInput),
    ] {
        let source = format!("(mod () {declaration} (list))");
        let compiler = Compiler::new(Cow::Borrowed(source.as_bytes()), 0, 0, &[]);
        assert_eq!(
            kind,
            compiler.compile().unwrap_err().kind(),
            "{declaration}"
        );
    }
}

#[test]
fn test_rest_arguments() {
    use dg_xch_core::clvm::assemble::assemble_text;
    use dg_xch_core::clvm::compile::{Compiler, INLINE_DEFUNS};
    use dg_xch_core::clvm::utils::INFINITE_COST;
    use std::borrow::Cow;

    for declaration in ["defun", "defun-inline"] {
        for flags in [0, INLINE_DEFUNS] {
            for (params, body, call, expected) in [
                ("first . rest", "(c first rest)", "VALUE", "(7)"),
                ("first . rest", "(c first rest)", "VALUE 2 3", "(7 2 3)"),
                (
                    "first second . rest",
                    "(list first second rest)",
                    "VALUE 2 3 4",
                    "(7 2 (3 4))",
                ),
            ] {
                let source = format!(
                    "(mod (VALUE) ({declaration} collect ({params}) {body}) (collect {call}))"
                );
                let compiler = Compiler::new(Cow::Borrowed(source.as_bytes()), flags, 0, &[]);
                let prog = compiler.compile().unwrap();
                let (_, result) = prog
                    .run(INFINITE_COST, 0, &assemble_text("(7)").unwrap())
                    .unwrap();
                assert_eq!(
                    assemble_text(expected).unwrap(),
                    result,
                    "{source}, flags {flags}"
                );
            }
        }
    }
}

#[test]
fn test_invalid_rest_arguments() {
    use dg_xch_core::clvm::compile::Compiler;
    use std::borrow::Cow;

    for params in ["first .", "first . rest extra", "first . . rest", ". rest"] {
        let source = format!("(mod () (defun collect ({params}) (list)) (collect 1))");
        let compiler = Compiler::new(Cow::Borrowed(source.as_bytes()), 0, 0, &[]);
        assert!(compiler.compile().is_err(), "{params}");
    }
    for declaration in ["defun", "defun-inline"] {
        let source =
            format!("(mod () ({declaration} collect (first second . rest) (list)) (collect 1))");
        let compiler = Compiler::new(Cow::Borrowed(source.as_bytes()), 0, 0, &[]);
        assert!(compiler.compile().is_err(), "{declaration}");
    }
}

#[test]
fn test_include() {
    use dg_xch_core::clvm::assemble::assemble_text;
    use dg_xch_core::clvm::compile::{Compiler, INLINE_CONSTS};
    use dg_xch_core::clvm::utils::INFINITE_COST;
    use std::borrow::Cow;
    use std::fs;

    let dir = std::env::temp_dir().join(format!("dg-include-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(dir.join("fallback")).unwrap();
    fs::write(
        dir.join("values.clib"),
        "; constants\n((defconstant VALUE 7))",
    )
    .unwrap();
    fs::write(dir.join("fallback/values.clib"), "((defconstant VALUE 99))").unwrap();
    fs::write(
        dir.join("fallback/functions.clib"),
        "(\n(include values.clib)\n; function\n(defun add-value (num) (+ num VALUE))\n)",
    )
    .unwrap();
    let fallback = dir.join("fallback");
    let include_dirs = [dir.to_str().unwrap(), fallback.to_str().unwrap()];
    for name in ["functions.clib", "\"functions.clib\""] {
        let source = format!("(mod (num) (include {name}) (add-value num))");
        let compiler = Compiler::new(
            Cow::Borrowed(source.as_bytes()),
            INLINE_CONSTS,
            0,
            &include_dirs,
        );
        let prog = compiler.compile().unwrap();
        let direct = Compiler::new(
            Cow::Borrowed(b"(mod (num) (defconstant VALUE 7) (defun add-value (num) (+ num VALUE)) (add-value num))"),
            INLINE_CONSTS, 0, &[],
        );
        assert_eq!(
            direct.compile().unwrap().serialized().unwrap().to_bytes(),
            prog.serialized().unwrap().to_bytes()
        );
        let (_, result) = prog
            .run(INFINITE_COST, 0, &assemble_text("(5)").unwrap())
            .unwrap();
        assert_eq!(assemble_text("12").unwrap(), result);
    }
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn test_invalid_include() {
    use dg_xch_core::clvm::compile::Compiler;
    use std::borrow::Cow;
    use std::fs;
    use std::io::ErrorKind;

    let dir = std::env::temp_dir().join(format!("dg-include-errors-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&dir).unwrap();
    let include_dirs = [dir.to_str().unwrap()];
    for (content, kind) in [
        ("((defconstant VALUE 7)", ErrorKind::UnexpectedEof),
        ("(())", ErrorKind::InvalidInput),
        ("() trailing", ErrorKind::InvalidInput),
        ("((include invalid.clib))", ErrorKind::InvalidInput),
    ] {
        fs::write(dir.join("invalid.clib"), content).unwrap();
        let compiler = Compiler::new(
            Cow::Borrowed(b"(mod () (include invalid.clib) (list))"),
            0,
            0,
            &include_dirs,
        );
        assert_eq!(kind, compiler.compile().unwrap_err().kind(), "{content}");
    }
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn test_compiler_sigil() {
    use dg_xch_core::clvm::assemble::assemble_text;
    use dg_xch_core::clvm::compile::Compiler;
    use std::borrow::Cow;

    for version in [25, 26] {
        for level in 0..=2 {
            let source =
                format!("(mod (num) (include *standard-cl-{version}*) (+ num *chialisp-version*))");
            let compiler = Compiler::new(Cow::Borrowed(source.as_bytes()), COMPAT_CHIA, level, &[]);
            let prog = compiler.compile().unwrap();
            assert_eq!(
                assemble_text(&format!("(+ 2 (q . {version}))"))
                    .unwrap()
                    .serialized()
                    .unwrap(),
                prog.serialized().unwrap(),
            );
        }
    }
}

#[test]
fn test_cl25_lookup_paths() {
    use dg_xch_core::clvm::assemble::assemble_text;
    use dg_xch_core::clvm::compile::Compiler;
    use dg_xch_core::clvm::program::SerializedProgram;
    use dg_xch_core::clvm::utils::INFINITE_COST;
    use std::borrow::Cow;

    let names = (0..70)
        .map(|index| format!("a{index}"))
        .collect::<Vec<_>>()
        .join(" ");
    let values = (0..70)
        .map(|index| format!("({index} {index})"))
        .collect::<Vec<_>>()
        .join(" ");
    let env = assemble_text(&format!("({values})")).unwrap();
    // chialisp 0.4.5 goldens: truncate CL25 lookups, but not selections
    // composed afterward or expressions substituted into inline functions.
    for (version, declaration, body, expected) in [
        (
            25,
            "",
            "(list a61 a62 a63 a64 a69)",
            "ff04ff885fffffffffffffffffff04ff8900bfffffffffffffffffff04ff887fffffffffffffffffff04ff8900ffffffffffffffffffff04ff8900ffffffffffffffffff808080808080",
        ),
        (
            25,
            "",
            "(list (f a62) (r a62))",
            "ff04ff89013fffffffffffffffffff04ff8901bfffffffffffffffff808080",
        ),
        (
            25,
            "(defun-inline identity (X) X)",
            "(list (identity (f a62)) (identity a69))",
            "ff04ff89013fffffffffffffffffff04ff8900ffffffffffffffffff808080",
        ),
        (
            25,
            "(defun identity (X) X)",
            "(identity a69)",
            "ff02ffff01ff02ff02ffff04ff02ffff04ff8900ffffffffffffffffff80808080ffff04ffff0105ff018080",
        ),
        (
            26,
            "",
            "(list a61 a62 a63 a64 a69)",
            "ff04ff885fffffffffffffffffff04ff8900bfffffffffffffffffff04ff89017fffffffffffffffffff04ff8902ffffffffffffffffffff04ff895fffffffffffffffffff808080808080",
        ),
        (
            26,
            "",
            "(list (f a62) (r a62))",
            "ff04ff89013fffffffffffffffffff04ff8901bfffffffffffffffff808080",
        ),
        (
            26,
            "(defun-inline identity (X) X)",
            "(list (identity (f a62)) (identity a69))",
            "ff04ff89013fffffffffffffffffff04ff895fffffffffffffffffff808080",
        ),
        (
            26,
            "(defun identity (X) X)",
            "(identity a69)",
            "ff02ffff01ff02ff02ffff04ff02ffff04ff8a00bfffffffffffffffffff80808080ffff04ffff0105ff018080",
        ),
    ] {
        let source =
            format!("(mod ({names}) (include *standard-cl-{version}*) {declaration} {body})");
        let expected = SerializedProgram::from_hex(expected).unwrap();
        let (_, expected_value) = expected
            .to_program()
            .unwrap()
            .run(INFINITE_COST, 0, &env)
            .unwrap();
        for level in 0..=2 {
            let compiler = Compiler::new(Cow::Borrowed(source.as_bytes()), COMPAT_CHIA, level, &[]);
            let program = compiler.compile().unwrap();
            if level == 0 {
                assert_eq!(expected, program.serialized().unwrap(), "{source}");
            }
            let (_, result) = program.run(INFINITE_COST, 0, &env).unwrap();
            assert_eq!(expected_value, result, "CL{version}, level {level}: {body}");
        }
    }
}

#[test]
fn test_unsupported_compiler_sigil() {
    use dg_xch_core::clvm::compile::Compiler;
    use std::borrow::Cow;
    use std::io::ErrorKind;

    for sigil in ["*standard-cl-24*", "*strict-cl-21*", "*standard-cl-27*"] {
        let source = format!("(mod () (include {sigil}) (list))");
        let compiler = Compiler::new(Cow::Borrowed(source.as_bytes()), COMPAT_CHIA, 0, &[]);
        let error = compiler.compile().unwrap_err();
        assert_eq!(ErrorKind::Unsupported, error.kind());
        assert_eq!(
            format!("Unsupported compiler sigil: {sigil}"),
            error.to_string()
        );
    }
    let compiler = Compiler::new(
        Cow::Borrowed(b"(mod () (include *standard-cl-26* extra) (list))"),
        COMPAT_CHIA,
        0,
        &[],
    );
    assert_eq!(
        ErrorKind::InvalidInput,
        compiler.compile().unwrap_err().kind()
    );
    let compiler = Compiler::new(
        Cow::Borrowed(b"(mod () (include missing.clib) (list))"),
        COMPAT_CHIA,
        0,
        &[],
    );
    assert_eq!(ErrorKind::NotFound, compiler.compile().unwrap_err().kind());
}

#[test]
fn test_path_optimization() {
    use dg_xch_core::clvm::assemble::assemble_text;
    use dg_xch_core::clvm::compile::Compiler;
    use std::borrow::Cow;

    for (body, expected) in [
        ("(f TREE)", "4"),
        ("(r TREE)", "6"),
        ("(f (r TREE))", "10"),
        ("(r (f TREE))", "12"),
        ("(r (r (r (r (r (r (r TREE)))))))", "510"),
        ("(f 5)", "(f (q . 5))"),
        ("(f ())", "(f ())"),
        ("(f TREE TREE)", "(f 2 2)"),
    ] {
        let source = format!("(mod (TREE) {body})");
        let compiler = Compiler::new(Cow::Borrowed(source.as_bytes()), 0, 0, &[]);
        let prog = compiler.compile().unwrap();
        assert_eq!(
            assemble_text(expected)
                .unwrap()
                .serialized()
                .unwrap()
                .to_bytes(),
            prog.serialized().unwrap().to_bytes(),
            "{body}",
        );
    }
}

#[test]
fn test_list() {
    use dg_xch_core::clvm::assemble::assemble_text;
    use dg_xch_core::clvm::compile::Compiler;
    use dg_xch_core::clvm::utils::INFINITE_COST;
    use std::borrow::Cow;

    let compiler = Compiler::new(
        Cow::Borrowed(b"(mod (num) (list num (list 7) (list)))"),
        0,
        0,
        &[],
    );
    let prog = compiler.compile().unwrap();
    let (_, result) = prog
        .run(INFINITE_COST, 0, &assemble_text("(25)").unwrap())
        .unwrap();
    assert_eq!(assemble_text("(25 (7) ())").unwrap(), result);
}

#[test]
fn test_if() {
    use dg_xch_core::clvm::assemble::assemble_text;
    use dg_xch_core::clvm::compile::Compiler;
    use dg_xch_core::clvm::utils::INFINITE_COST;
    use std::borrow::Cow;

    for (source, args) in [
        ("(mod (num) (if num (list 7) (x)))", "(1)"),
        ("(mod (num) (if num (x) (list 7)))", "(0)"),
    ] {
        let compiler = Compiler::new(Cow::Borrowed(source.as_bytes()), 0, 0, &[]);
        let prog = compiler.compile().unwrap();
        let (_, result) = prog
            .run(INFINITE_COST, 0, &assemble_text(args).unwrap())
            .unwrap();
        assert_eq!(assemble_text("(7)").unwrap(), result);
    }
    for source in ["(mod () (if 1 2))", "(mod () (if 1 2 3 4))"] {
        let compiler = Compiler::new(Cow::Borrowed(source.as_bytes()), 0, 0, &[]);
        assert!(compiler.compile().is_err());
    }
}

#[test]
fn test_recursive_function_arguments() {
    use dg_xch_core::clvm::assemble::assemble_text;
    use dg_xch_core::clvm::compile::Compiler;
    use dg_xch_core::clvm::utils::INFINITE_COST;
    use std::borrow::Cow;

    const EXAMPLE_CLSP: &str = "
    (mod (TREE)
        (defun-inline wrap (VALUE) (list VALUE))
        (defun-inline leaf (VALUE) (if VALUE (f (list VALUE)) 0))
        (defun sum-tree (TREE)
            (if (l TREE)
                (+ (sum-tree (f TREE)) (sum-tree (r TREE)))
                (leaf TREE)
            )
        )
        (wrap (sum-tree TREE))
    )";
    let compiler = Compiler::new(Cow::Borrowed(EXAMPLE_CLSP.as_bytes()), 0, 0, &[]);
    let prog = compiler.compile().unwrap();
    let args = assemble_text("(((1 . 2) . (3 . 4)))").unwrap();
    let (_, result) = prog.run(INFINITE_COST, 0, &args).unwrap();
    assert_eq!(assemble_text("(10)").unwrap(), result);
}

#[test]
fn test_defun() {
    use dg_xch_core::clvm::compile::Compiler;
    use std::borrow::Cow;
    const EXAMPLE_CLSP: &str = "
    (mod (num)
        (defconstant NUL_NUM 2)
        (defun square (number)
            ;; Returns the number squared.
            (* number number)
        )
        (square num)
    )";
    let compiler = Compiler::new(Cow::Borrowed(EXAMPLE_CLSP.as_bytes()), 0, 0, &[]);
    let prog = compiler.compile().unwrap();
    assert_eq!(
        "(a (q 2 2 (c 2 (c 5 ()))) (c (q 18 5 5) 1))",
        format!("{prog}")
    )
}

#[test]
fn test_nested_defun() {
    use dg_xch_core::clvm::compile::Compiler;
    use std::borrow::Cow;
    const EXAMPLE_CLSP: &str = "
    (mod (num)
        (defconstant NUL_NUM 2)
        (defun square (number)
            ;; Returns the number squared.
            (* number number)
        )
        (defun double (number)
            (* NUL_NUM number)
        )
        (square (double num))
    )";
    let compiler = Compiler::new(Cow::Borrowed(EXAMPLE_CLSP.as_bytes()), 0, 0, &[]);
    let prog = compiler.compile().unwrap();
    assert_eq!(
        "(a (q 2 4 (c 2 (c (a 6 (c 2 (c 5 ()))) ()))) (c (q (* 5 5) 18 (q . 2) 5) 1))",
        format!("{prog}")
    )
}

#[test]
fn test_defun_inline() {
    use dg_xch_core::clvm::compile::Compiler;
    use std::borrow::Cow;
    const EXAMPLE_CLSP: &str = "
    (mod (num)
        (defun-inline double (number)
            ;; Returns twice the number.
            (* number 2)
        )
        (double num)
    )";
    let compiler = Compiler::new(Cow::Borrowed(EXAMPLE_CLSP.as_bytes()), 0, 0, &[]);
    let prog = compiler.compile().unwrap();
    assert_eq!("(* 2 (q . 2))", format!("{prog}"))
}

#[test]
fn test_multi_constant() {
    use dg_xch_core::clvm::assemble::assemble_text;
    use dg_xch_core::clvm::compile::Compiler;
    use dg_xch_core::clvm::program::Program;
    use dg_xch_core::clvm::sexp::SExp;
    use dg_xch_core::clvm::utils::INFINITE_COST;
    use std::borrow::Cow;
    const EXAMPLE_CLSP: &str = "
    (mod (num)
        (defconstant NUL_NUM 22)
        (defconstant NUL_NUM2 23)
        (defconstant NUL_NUM3 24)
        (defun mul (number)
            (* NUL_NUM3 (* NUL_NUM2 (* NUL_NUM number)))
        )
        (mul num)
    )";
    let compiler = Compiler::new(Cow::Borrowed(EXAMPLE_CLSP.as_bytes()), 0, 0, &[]);
    let prog = compiler.compile().unwrap();
    let chia_prog =
        assemble_text("(a (q 2 14 (c 2 (c 5 ()))) (c (q (ash . 23) 24 18 10 (* 12 (* 8 5))) 1))")
            .unwrap();
    let results = prog
        .run(INFINITE_COST, 0, &Program::to(&[SExp::from(11)]))
        .unwrap();
    println!(
        "DG Results: Cost({}) Value({})",
        results.0,
        results.1.as_int().unwrap()
    );
    let results = chia_prog
        .run(INFINITE_COST, 0, &Program::to(&[SExp::from(11)]))
        .unwrap();
    println!(
        "Chia Results: Cost({}) Value({})",
        results.0,
        results.1.as_int().unwrap()
    );
}

#[test]
fn test_2_constants() {
    use dg_xch_core::clvm::compile::Compiler;
    use std::borrow::Cow;
    const EXAMPLE_CLSP: &str = "
    (mod (num)
      (defconstant NUL_NUM 22)
      (defconstant NUL_NUM2 23)
      (defun mul (number)
          (* NUL_NUM2 (* NUL_NUM number))
      )
      (mul num)
    )";
    let compiler = Compiler::new(Cow::Borrowed(EXAMPLE_CLSP.as_bytes()), 0, 0, &[]);
    let prog = compiler.compile().unwrap();
    assert_eq!(
        "(a (q 2 2 (c 2 (c 5 ()))) (c (q 18 (q . 23) (* (q . 22) 5)) 1))",
        format!("{}", prog)
    );
}

#[test]
fn test_constant_inline() {
    use dg_xch_core::clvm::compile::{Compiler, INLINE_CONSTS};
    use dg_xch_core::clvm::program::Program;
    use dg_xch_core::clvm::sexp::SExp;
    use dg_xch_core::clvm::utils::INFINITE_COST;
    use std::borrow::Cow;
    const EXAMPLE_CLSP: &str = "
    (mod (num)
        (defconstant NUL_NUM 25)
        (defun mul (number)
            (* NUL_NUM number)
        )
        (mul num)
    )";
    let compiler = Compiler::new(
        Cow::Borrowed(EXAMPLE_CLSP.as_bytes()),
        INLINE_CONSTS,
        0,
        &[],
    );
    let prog = compiler.compile().unwrap();
    let results = prog
        .run(INFINITE_COST, 0, &Program::to(&[SExp::from(11)]))
        .unwrap();
    assert_eq!(Program::to(275), results.1)
}

#[test]
fn test_re_assembly() {
    use dg_xch_core::clvm::assemble::assemble_text;
    use dg_xch_core::clvm::compile::{Compiler, INLINE_CONSTS};
    use dg_xch_core::clvm::program::Program;
    use dg_xch_core::clvm::sexp::SExp;
    use dg_xch_core::clvm::utils::INFINITE_COST;
    use std::borrow::Cow;
    const EXAMPLE_CLSP: &str = "
    (mod (num)
        (defconstant NUL_NUM 22)
        (defconstant NUL_NUM2 23)
        (defconstant NUL_NUM3 24)
        (defun mul (number)
            (* NUL_NUM3 (* NUL_NUM2 (* NUL_NUM number)))
        )
        (mul num)
    )";
    println!("Compiling Program: {EXAMPLE_CLSP}");
    let inline_compiler = Compiler::new(
        Cow::Borrowed(EXAMPLE_CLSP.as_bytes()),
        INLINE_CONSTS,
        0,
        &[],
    );
    let prog = inline_compiler.compile().unwrap();
    let inlined_str = format!("{prog}");
    println!("Inlined Constants  CLVM: {inlined_str}");
    let serial = assemble_text(&inlined_str).unwrap();
    assert_eq!(prog, serial);
    let results = serial
        .run(INFINITE_COST, 0, &Program::to(&[SExp::from(11)]))
        .unwrap();
    println!(
        "Inlined Constants Results: Cost({}) Value({})",
        results.0,
        results.1.as_int().unwrap()
    );
    assert_eq!(Program::to(133584), results.1);
    let compiler = Compiler::new(Cow::Borrowed(EXAMPLE_CLSP.as_bytes()), 0, 0, &[]);
    let prog = compiler.compile().unwrap();
    let inlined_str = format!("{prog}");
    println!("Argument Constants CLVM: {inlined_str}");
    let serial = assemble_text(&inlined_str).unwrap();
    assert_eq!(prog, serial);
    let results = serial
        .run(INFINITE_COST, 0, &Program::to(&[SExp::from(11)]))
        .unwrap();
    println!(
        "Argument Constants Results: Cost({}) Value({})",
        results.0,
        results.1.as_int().unwrap()
    );
    assert_eq!(Program::to(133584), results.1);
}

#[test]
fn test_runtime_add() {
    use dg_xch_core::clvm::compile::Compiler;
    use dg_xch_core::clvm::program::Program;
    use dg_xch_core::clvm::sexp::SExp;
    use std::borrow::Cow;
    const EXAMPLE_CLSP: &str = "
    (mod (num num2)
        (+ num num2)
    )";
    let compiler = Compiler::new(Cow::Borrowed(EXAMPLE_CLSP.as_bytes()), 0, 0, &[]);
    let prog = compiler.compile().unwrap();
    assert_eq!("(+ 2 5)", format!("{prog}"));
    let args = Program::to(&[SExp::from(10), SExp::from(13)]);
    let mut runtime = ClvmRuntime::new(u64::MAX, MEMPOOL_MODE);
    let (_, output) = runtime.run(prog.sexp(), args.sexp()).unwrap();
    assert_eq!("23", format!("{output}"))
}

#[test]
fn test_runtime_sub() {
    use dg_xch_core::clvm::compile::Compiler;
    use dg_xch_core::clvm::program::Program;
    use dg_xch_core::clvm::sexp::SExp;
    use std::borrow::Cow;
    const EXAMPLE_CLSP: &str = "
    (mod (num num2)
        (- num num2)
    )";
    let compiler = Compiler::new(Cow::Borrowed(EXAMPLE_CLSP.as_bytes()), 0, 0, &[]);
    let prog = compiler.compile().unwrap();
    assert_eq!("(- 2 5)", format!("{prog}"));
    let args = Program::to(&[SExp::from(10), SExp::from(13)]);
    let mut runtime = ClvmRuntime::new(u64::MAX, MEMPOOL_MODE);
    let (_, output) = runtime.run(prog.sexp(), args.sexp()).unwrap();
    assert_eq!("-3", format!("{output}"))
}

#[test]
fn test_runtime_mul() {
    use dg_xch_core::clvm::compile::Compiler;
    use dg_xch_core::clvm::program::Program;
    use dg_xch_core::clvm::sexp::SExp;
    use std::borrow::Cow;
    const EXAMPLE_CLSP: &str = "
    (mod (num num2)
        (* num num2)
    )";
    let compiler = Compiler::new(Cow::Borrowed(EXAMPLE_CLSP.as_bytes()), 0, 0, &[]);
    let prog = compiler.compile().unwrap();
    assert_eq!("(* 2 5)", format!("{prog}"));
    let args = Program::to(&[SExp::from(10), SExp::from(13)]);
    let mut runtime = ClvmRuntime::new(u64::MAX, MEMPOOL_MODE);
    let (_, output) = runtime.run(prog.sexp(), args.sexp()).unwrap();
    assert_eq!("130", format!("{output}"))
}

#[test]
fn test_runtime_div() {
    use dg_xch_core::clvm::compile::Compiler;
    use dg_xch_core::clvm::program::Program;
    use dg_xch_core::clvm::sexp::SExp;
    use std::borrow::Cow;
    const EXAMPLE_CLSP: &str = "
    (mod (num num2)
        (/ num num2)
    )";
    let compiler = Compiler::new(Cow::Borrowed(EXAMPLE_CLSP.as_bytes()), 0, 0, &[]);
    let prog = compiler.compile().unwrap();
    assert_eq!("(/ 2 5)", format!("{prog}"));
    let args = Program::to(&[SExp::from(260), SExp::from(13)]);
    let mut runtime = ClvmRuntime::new(u64::MAX, MEMPOOL_MODE);
    let (_, output) = runtime.run(prog.sexp(), args.sexp()).unwrap();
    assert_eq!("20", format!("{output}"))
}

#[test]
fn multiply_running_product_limb_cost_matches_clvmr() {
    use dg_xch_core::clvm::program::{Program, SerializedProgram};
    use dg_xch_core::clvm::utils::INFINITE_COST;
    for (hex, want) in [
        // (* (q . 0x7fffff) (q . 2) (q . 3))
        ("ff12ffff01837fffffffff0102ffff010380", 2011u64),
        // (* (q . 0x7fffff) (q . 2) (q . 3) (q . 256))
        ("ff12ffff01837fffffffff0102ffff0103ffff0182010080", 2962u64),
    ] {
        let serial = SerializedProgram::from_hex(hex).unwrap();
        let prog = serial.to_program().unwrap();
        let (cost, _) = prog.run(INFINITE_COST, 0, &Program::to(0)).unwrap();
        assert_eq!(cost, want, "multiply cost diverges from clvmr for {hex}");
    }
}

#[test]
#[cfg(feature = "bls")]
fn bls_ops_cost_and_value_match_clvmr() {
    use dg_xch_core::clvm::program::{Program, SerializedProgram};
    use dg_xch_core::clvm::utils::INFINITE_COST;
    use dg_xch_core::consensus::block_generator::BlockGeneratorFlags;
    use dg_xch_core::consensus::constants::MAINNET;

    const G1_GEN: &str = "97f1d3a73197d7942695638c4fa9ac0fc3688c4f9774b905a14e3a3f171bac586c55e83ff97a1aeffb3af00adb22c6bb";
    const G2_GEN: &str = "93e02b6052719f607dacd3a088274f65596bd0d09920b61ab5da61bbdc7f5049334cf11213945d57e5ac7d055d042b7e024aa2b2f08f0a91260805272dc51051c6e47ad4fa403b02b4510b647ae3d1770bac0326a805bbefd48056c8c121bdb8";
    const NEG_G2_GEN: &str = "b3e02b6052719f607dacd3a088274f65596bd0d09920b61ab5da61bbdc7f5049334cf11213945d57e5ac7d055d042b7e024aa2b2f08f0a91260805272dc51051c6e47ad4fa403b02b4510b647ae3d1770bac0326a805bbefd48056c8c121bdb8";
    // Fixed-seed AUG-scheme vector: sk = SecretKey::from_seed([7; 32]), sig = sign(sk, "abc").
    const PK: &str = "a010d140e7c43146b5bb59695e6c444abbb62e964a535d0034351a90d1192bff0130de95f9bbc58af254c4dab4e65d3a";
    const SIG: &str = "b6263afd8aa87b1c4ad4f3da9657ed804a82a1483b6b500e6b4771e315d5f0aa6b188373b8e5b11f74184e15c0f266040bb84b6a4077e0399c8870685456eee33d89863325b07dcdb89000f609133c20fe4c8c041cf3e2242bb692cb560da59f";
    let inf_g2 = format!("c0{}", "00".repeat(95));

    // The mainnet flag regime the wedged block ran under (post SF8/SF9, pre hard fork 2).
    let flags = BlockGeneratorFlags::for_height(&MAINNET, 9_179_161).clvm_flags;

    let cases: Vec<(String, &str, u64, String)> = vec![
        (
            "g1_negate".into(),
            "51",
            1417,
            format!("b7{}", &G1_GEN[2..]),
        ),
        (
            "g2_negate".into(),
            "55",
            2185,
            NEG_G2_GEN.to_string(),
        ),
        (
            "g1_multiply".into(),
            "50",
            706_031,
            "a572cbea904d67468808c8eb50a9450c9721db309128012543902d0ac358a62ae28f75bb8f1c7c42c39a8c5529bf0f4e".into(),
        ),
        (
            "g2_multiply".into(),
            "54",
            2_101_006,
            "aa4edef9c1ed7f729f520e47730a124fd70662a904ba1074728114d1031e1572c6c886f6b57ec72a6178288c47c335771638533957d540a9d2370f17cc7ed5863bc0b995b8825e0ee1ea1e1e4d00dbae81f14b0bf3611b78c952aacab827a053".into(),
        ),
        (
            "g1_subtract".into(),
            "49",
            2_789_575,
            format!("c0{}", "00".repeat(47)),
        ),
        (
            "g2_add".into(),
            "52",
            3_981_001,
            "aa4edef9c1ed7f729f520e47730a124fd70662a904ba1074728114d1031e1572c6c886f6b57ec72a6178288c47c335771638533957d540a9d2370f17cc7ed5863bc0b995b8825e0ee1ea1e1e4d00dbae81f14b0bf3611b78c952aacab827a053".into(),
        ),
    ];

    let quote_g1 = |p: &str| format!("ffff01b0{p}");
    let quote_g2 = |p: &str| format!("ffff01c060{p}");
    let run = |hex: &str| -> Result<(u64, Vec<u8>), String> {
        let serial = SerializedProgram::from_hex(hex).unwrap();
        let prog = serial.to_program().unwrap();
        match prog.run(INFINITE_COST, flags, &Program::to(0)) {
            Ok((cost, output)) => Ok((cost, output.as_vec().expect("atom output"))),
            Err(e) => Err(format!("{e:?}")),
        }
    };

    // (g1_negate (q . G1)) — and the same one-arg shape for g2_negate.
    let one_arg: Vec<(usize, String)> = vec![(0, quote_g1(G1_GEN)), (1, quote_g2(G2_GEN))];
    for (case_idx, (name, op, want_cost, want_out)) in cases.iter().enumerate() {
        let args = match case_idx {
            0 => one_arg[0].1.clone(),
            1 => one_arg[1].1.clone(),
            // (op (q . point) (q . 2))
            2 => format!("{}ffff0102", quote_g1(G1_GEN)),
            3 => format!("{}ffff0102", quote_g2(G2_GEN)),
            // (op (q . point) (q . point))
            4 => format!("{}{}", quote_g1(G1_GEN), quote_g1(G1_GEN)),
            5 => format!("{}{}", quote_g2(G2_GEN), quote_g2(G2_GEN)),
            _ => unreachable!(),
        };
        let opcode = op.parse::<u8>().unwrap();
        let hex = format!("ff{opcode:02x}{args}80");
        let (cost, out) = run(&hex).unwrap_or_else(|e| panic!("{name} failed: {e}"));
        assert_eq!(cost, *want_cost, "{name} cost diverges from clvmr");
        assert_eq!(
            hex::encode(out),
            *want_out,
            "{name} value diverges from clvmr"
        );
    }

    // (g1_map (q . "abc")) / (g2_map (q . "abc")) — default DST.
    let (cost, out) = run("ff38ffff018361626380").unwrap();
    assert_eq!(cost, 195_685, "map_to_g1 cost diverges from clvmr");
    assert_eq!(
        hex::encode(out),
        "a4b925a7f78b97ad6a8203e9b1e319f0fcde5bea79e58fac5ec79a2867d11bd97ded3fed5e346bc0afd8e23f0069055d"
    );
    let (cost, out) = run("ff39ffff018361626380").unwrap();
    assert_eq!(cost, 816_165, "map_to_g2 cost diverges from clvmr");
    assert_eq!(
        hex::encode(out),
        "8c57634a695c6d4933239fcdefcd5d92e85c59a07b3721cf1a865981a1ba9e439839d4ee0fa6195e0fa0381bfd667ce10f57e6a4a5fa46df6cf2319b6e4396364173868d519cbab87ea0b32eb9bf9d76612f13254bb0d904ede697820c34782d"
    );

    // (bls_pairing_identity (q . G1) (q . -G2) (q . G1) (q . G2)) — e(P,-Q)·e(P,Q) = 1: nil.
    let ok_hex = format!(
        "ff3a{}{}{}{}80",
        quote_g1(G1_GEN),
        quote_g2(NEG_G2_GEN),
        quote_g1(G1_GEN),
        quote_g2(G2_GEN)
    );
    let (cost, out) = run(&ok_hex).unwrap();
    assert_eq!(cost, 5_400_081, "pairing_identity cost diverges from clvmr");
    assert!(out.is_empty(), "pairing_identity must return nil");
    let fail_hex = format!("ff3a{}{}80", quote_g1(G1_GEN), quote_g2(G2_GEN));
    assert!(run(&fail_hex).is_err(), "non-identity pairing must raise");

    // (bls_verify (q . sig) (q . pk) (q . "abc")) — AUG-scheme verify: nil on success.
    let verify_hex =
        format!("ff3b{}{}ffff0183616263 80", quote_g2(SIG), quote_g1(PK)).replace(' ', "");
    let (cost, out) = run(&verify_hex).unwrap();
    assert_eq!(cost, 4_200_245, "bls_verify cost diverges from clvmr");
    assert!(out.is_empty(), "bls_verify must return nil");
    // Empty pair set: verifies iff the signature is the G2 identity.
    let empty_hex = format!("ff3bffff01c060{inf_g2}80");
    let (cost, out) = run(&empty_hex).unwrap();
    assert_eq!(cost, 3_000_021, "bls_verify empty cost diverges from clvmr");
    assert!(out.is_empty());
    let bad_hex =
        format!("ff3b{}{}ffff0183616264 80", quote_g2(SIG), quote_g1(PK)).replace(' ', "");
    assert!(run(&bad_hex).is_err(), "bad bls_verify must raise");
}

#[test]
fn test_compound_constants_and_nested_module_arguments() {
    use dg_xch_core::clvm::assemble::assemble_text;
    use dg_xch_core::clvm::compile::Compiler;
    use std::borrow::Cow;
    for (body, env, expected) in [
        (
            "(mod ((A B) . C) SIGIL (list A B C))",
            "((7 8) 9 10)",
            "(7 8 (9 10))",
        ),
        (
            "(mod () SIGIL (defconstant DATA (1 2)) (list DATA))",
            "()",
            "((1 2))",
        ),
        (
            "(mod () SIGIL (defconstant DATA (+ 1 2)) DATA)",
            "()",
            "(43 1 2)",
        ),
    ] {
        for version in [0, 21, 23, 25, 26] {
            let sigil = if version == 0 {
                String::new()
            } else {
                format!("(include *standard-cl-{version}*)")
            };
            let source = body.replace("SIGIL", &sigil);
            let compiler = Compiler::new(
                Cow::Borrowed(source.as_bytes()),
                if version == 0 { 0 } else { COMPAT_CHIA },
                0,
                &[],
            );
            let program = compiler.compile().unwrap();
            let (_, result) = program
                .run(1_000_000, 0, &assemble_text(env).unwrap())
                .unwrap();
            assert_eq!(result, assemble_text(expected).unwrap(), "{source}");
        }
    }
}

#[test]
fn test_extended_language_reference_bytes() {
    use dg_xch_core::clvm::compile::Compiler;
    use std::borrow::Cow;
    let cases: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/compiler_language.json")).unwrap();
    for case in cases.as_array().unwrap() {
        let source = case["source"].as_str().unwrap();
        let compiler = Compiler::new(Cow::Borrowed(source.as_bytes()), COMPAT_CHIA, 0, &[]);
        let program = compiler
            .compile()
            .unwrap_or_else(|e| panic!("{source}: {e}"));
        assert_eq!(
            hex::encode(program.serialized().unwrap().as_ref()),
            case["hex"].as_str().unwrap(),
            "{source}"
        );
    }
}

#[test]
fn test_embedded_expressions_reference_bytes() {
    use dg_xch_core::clvm::compile::Compiler;
    use std::borrow::Cow;
    let dir = std::env::temp_dir().join(format!("dg-embed-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("value.sexp"), "(1 2 (4 . 5))").unwrap();
    std::fs::write(dir.join("child.clsp"), "(mod (X) (+ X 1))").unwrap();
    std::fs::write(dir.join("value.hex"), "ff01ff02ffff040580").unwrap();
    let cases: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/compiler_embed.json")).unwrap();
    let includes = [dir.to_str().unwrap()];
    for case in cases.as_array().unwrap() {
        let source = case["source"].as_str().unwrap();
        let compiler = Compiler::new(Cow::Borrowed(source.as_bytes()), COMPAT_CHIA, 0, &includes);
        let program = compiler.compile().unwrap();
        assert_eq!(
            hex::encode(program.serialized().unwrap().as_ref()),
            case["hex"].as_str().unwrap(),
            "{source}"
        );
    }
    std::fs::write(
        dir.join("child.clsp"),
        "(mod () (compile-file AGAIN child.clsp) AGAIN)",
    )
    .unwrap();
    let compiler = Compiler::new(
        Cow::Borrowed(b"(mod () (include *standard-cl-26*) (compile-file CHILD child.clsp) CHILD)"),
        COMPAT_CHIA,
        0,
        &includes,
    );
    assert!(
        compiler
            .compile()
            .unwrap_err()
            .to_string()
            .contains("nesting limit")
    );
    std::fs::remove_dir_all(dir).unwrap();
}
