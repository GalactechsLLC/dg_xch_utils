use crate::clvm::dialect::Dialect;
use crate::clvm::more_ops::BOOL_BASE_COST;
use log::info;
use std::io::Error;
use crate::clvm::sexp::{SExp};
use crate::constants::NULL_SEXP;

pub fn op_print<D: Dialect>(
    args: &SExp,
    _max_cost: u64,
    _dialect: &D,
) -> Result<(u64, SExp), Error> {
    match args.clone().proper_list(true) {
        None => {
            Ok((BOOL_BASE_COST, NULL_SEXP.clone()))
        }
        Some(mut args) => {
            args.reverse();
            if args.is_empty() {
                Ok((BOOL_BASE_COST, NULL_SEXP.clone()))
            } else {
                let mut buffer = String::new();
                match args.first() {
                    Some(arg) => {
                        buffer.extend(format!("{}:", arg).chars());
                        let mut cost = BOOL_BASE_COST * 2;
                        let iter = args.iter().skip(1);
                        for arg in iter {
                            cost += BOOL_BASE_COST;
                            buffer.extend(format!(" {},", arg).chars());
                        }
                        buffer.remove(buffer.len() - 1);
                        info!("CLVM DEBUG: {}", buffer);
                        Ok((cost, NULL_SEXP.clone()))
                    }
                    None => {
                        Ok((BOOL_BASE_COST, NULL_SEXP.clone()))
                    }
                }
            }
        }
    }
}

#[test]
fn test_print_ops() {
    use crate::clvm::assemble::assemble_text;
    use crate::clvm::program::Program;
    use crate::clvm::utils::INFINITE_COST;
    use simple_logger::SimpleLogger;
    // (mod (num)
    //   (defun print (l x) (i (all "$print$" l x) x x))
    //
    //   (defun inc (N)
    //     (+ N 1)
    //   )
    //
    //   (print (list "Running" (q . (+ num 1)) " With " num) (inc num))
    // )
    SimpleLogger::default().init().unwrap();
    let test_program = r#"(a (q 2 4 (c 2 (c 5 ()))) (c (q (a 6 (c 2 (c (c (q . "Running") (c (q 16 78 1) (c (q . " With ") (c 5 ())))) (c (+ 5 (q . 1)) ())))) 3 (all (q . "$print$") 5 11) 11 11) 1))"#;
    let assembled = assemble_text(test_program).unwrap();
    let results = assembled
        .to_program()
        .run(INFINITE_COST, 0, &Program::to(vec![50]))
        .unwrap();
    info!("Output {:?}", results);
}
