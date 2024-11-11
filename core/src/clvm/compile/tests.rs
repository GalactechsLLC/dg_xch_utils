use std::borrow::Cow;

#[test]
fn test_mod() {
    use crate::clvm::compile::{Compiler};
    const EXAMPLE_CLSP: &str = "
    (mod (num)
        (* num 25)
    )";
    let mut compiler = Compiler::new(Cow::Borrowed(EXAMPLE_CLSP.as_bytes()), 0, 0, &[]);
    let prog = compiler.compile().unwrap();
    assert_eq!("(* 2 (q . 25))", format!("{prog}"))
}
#[test]
fn test_defun() {
    use crate::clvm::compile::{Compiler};
        const EXAMPLE_CLSP: &str = "
    (mod (num)
        (defconstant NUL_NUM 2)
        (defun square (number)
            ;; Returns the number squared.
            (* number number)
        )
        (square num)
    )";
    let mut compiler = Compiler::new(Cow::Borrowed(EXAMPLE_CLSP.as_bytes()), 0, 0, &[]);
    let prog = compiler.compile().unwrap();
    assert_eq!("(a (q 2 6 (c 2 (c 5 ()))) (c (q 2 18 5 5) 1))", format!("{prog}"))
}

#[test]
fn test_nested_defun() {
    use crate::clvm::compile::{Compiler};
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
    let mut compiler = Compiler::new(Cow::Borrowed(EXAMPLE_CLSP.as_bytes()), 0, 0, &[]);
    let prog = compiler.compile().unwrap();
    assert_eq!("(a (q 2 14 (c 2 (c (a 10 (c 2 (c 5 ()))) ()))) (c (q 2 (* 4 5) 18 5 5) 1))", format!("{prog}"))
}

#[test]
fn test_defun_inline() {
    use crate::clvm::compile::{Compiler};
    const EXAMPLE_CLSP: &str = "
    (mod (num)
        (defun-inline double (number)
            ;; Returns twice the number.
            (* number 2)
        )
        (double num)
    )";
    let mut compiler = Compiler::new(Cow::Borrowed(EXAMPLE_CLSP.as_bytes()), 0, 0, &[]);
    let prog = compiler.compile().unwrap();
    assert_eq!("(* 2 (q . 2))", format!("{prog}"))
}

#[test]
fn test_multi_constant() {
    use crate::clvm::assemble::assemble_text;
    use crate::clvm::program::Program;
    use crate::clvm::utils::INFINITE_COST;
    use crate::clvm::compile::{Compiler};
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
    let mut compiler = Compiler::new(Cow::Borrowed(EXAMPLE_CLSP.as_bytes()), 0, 0, &[]);
    let prog = compiler.compile().unwrap();
    let chia_prog = assemble_text("(a (q 2 14 (c 2 (c 5 ()))) (c (q (ash . 23) 24 18 10 (* 12 (* 8 5))) 1))").unwrap().to_program();
    let results = prog.run(INFINITE_COST, 0, &Program::to(vec![11])).unwrap();
    println!("DG Results: Cost({}) Value({})", results.0, results.1.as_int().unwrap());
    let results = chia_prog.run(INFINITE_COST, 0, &Program::to(vec![11])).unwrap();
    println!("Chia Results: Cost({}) Value({})", results.0, results.1.as_int().unwrap());
}

#[test]
fn test_2_constants() {
    use crate::clvm::compile::{Compiler};
    const EXAMPLE_CLSP: &str = "
    (mod (num)
      (defconstant NUL_NUM 22)
      (defconstant NUL_NUM2 23)
      (defun mul (number)
          (* NUL_NUM2 (* NUL_NUM number))
      )
      (mul num)
    )";
    let mut compiler = Compiler::new(Cow::Borrowed(EXAMPLE_CLSP.as_bytes()), 0, 0, &[]);
    let prog = compiler.compile().unwrap();
    assert_eq!("(a (q 2 14 (c 2 (c 5 ()))) (c (q 22 23 18 10 (* 4 5)) 1))", format!("{}", prog));
}


#[test]
fn test_constant_inline() {
    use crate::clvm::compile::{Compiler, INLINE_CONSTS};
    use crate::clvm::program::{Program};
    use crate::clvm::utils::INFINITE_COST;
    const EXAMPLE_CLSP: &str = "
    (mod (num)
        (defconstant NUL_NUM 25)
        (defun mul (number)
            (* NUL_NUM number)
        )
        (mul num)
    )";
    let mut compiler = Compiler::new(Cow::Borrowed(EXAMPLE_CLSP.as_bytes()), INLINE_CONSTS, 0, &[]);
    let prog = compiler.compile().unwrap();
    let results = prog.run(INFINITE_COST, 0, &Program::to(vec![11])).unwrap();
    assert_eq!(Program::to(275), results.1)
}

#[test]
fn test_re_assembly() {
    use crate::clvm::assemble::assemble_text;
    use crate::clvm::compile::{Compiler, INLINE_CONSTS};
    use crate::clvm::program::{Program};
    use crate::clvm::utils::INFINITE_COST;
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
    let mut inline_compiler = Compiler::new(Cow::Borrowed(EXAMPLE_CLSP.as_bytes()), INLINE_CONSTS, 0, &[]);
    let prog = inline_compiler.compile().unwrap();
    let inlined_str = format!("{prog}");
    println!("Inlined Constants  CLVM: {inlined_str}");
    let serial = assemble_text(&inlined_str).unwrap().to_program();
    assert_eq!(prog, serial);
    let results = serial.run(INFINITE_COST, 0, &Program::to(vec![11])).unwrap();
    println!("Inlined Constants Results: Cost({}) Value({})", results.0, results.1.as_int().unwrap());
    assert_eq!(Program::to(133584), results.1);
    let mut compiler = Compiler::new(Cow::Borrowed(EXAMPLE_CLSP.as_bytes()), 0, 0, &[]);
    let prog = compiler.compile().unwrap();
    let inlined_str = format!("{prog}");
    println!("Argument Constants CLVM: {inlined_str}");
    let serial = assemble_text(&inlined_str).unwrap().to_program();
    assert_eq!(prog, serial);
    let results = serial.run(INFINITE_COST, 0, &Program::to(vec![11])).unwrap();
    println!("Argument Constants Results: Cost({}) Value({})", results.0, results.1.as_int().unwrap());
    assert_eq!(Program::to(133584), results.1);
}