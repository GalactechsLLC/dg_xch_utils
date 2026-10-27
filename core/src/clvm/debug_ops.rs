use crate::clvm::arena::{Arena, NodePtr};
use crate::clvm::dialect::Dialect;
use crate::clvm::more_ops::BOOL_BASE_COST;
use crate::clvm::utils::DISABLE_PRINTS;
use log::info;
use std::io::Error;

/// Debug print for the `(all "$print$" ...)` convention: `args` are the operands AFTER the
/// print keyword, in order. Output format: `first: second, third`.
pub fn op_print<D: Dialect>(
    arena: &Arena,
    args: &[NodePtr],
    _max_cost: u64,
    dialect: &D,
) -> Result<(u64, NodePtr), Error> {
    if dialect.flags() & DISABLE_PRINTS != 0 || !log::log_enabled!(log::Level::Info) {
        return Ok((BOOL_BASE_COST * (args.len() as u64 + 1), NodePtr::NIL));
    }
    if args.is_empty() {
        Ok((BOOL_BASE_COST, NodePtr::NIL))
    } else {
        let mut buffer = String::new();
        match args.first() {
            Some(arg) => {
                buffer.extend(format!("{}:", arena.export(*arg)).chars());
                let mut cost = BOOL_BASE_COST * 2;
                let iter = args.iter().skip(1);
                for arg in iter {
                    cost += BOOL_BASE_COST;
                    buffer.extend(format!(" {},", arena.export(*arg)).chars());
                }
                buffer.remove(buffer.len() - 1);
                info!("CLVM DEBUG: {buffer}");
                Ok((cost, NodePtr::NIL))
            }
            None => Ok((BOOL_BASE_COST, NodePtr::NIL)),
        }
    }
}

#[test]
fn test_print_ops() {
    use crate::clvm::assemble::assemble_text;
    use crate::clvm::program::Program;
    use crate::clvm::sexp::SExp;
    use crate::clvm::utils::INFINITE_COST;
    use dg_logger::DruidGardenLogger;
    use log::Level;
    // (mod (num)
    //   (defun print (l x) (i (all "$print$" l x) x x))
    //
    //   (defun inc (N)
    //     (+ N 1)
    //   )
    //
    //   (print (list "Running" (q . (+ num 1)) " With " num) (inc num))
    // )
    let _logger = DruidGardenLogger::build()
        .use_colors(true)
        .current_level(Level::Info)
        .init()
        .map_err(|e| Error::other(format!("{e:?}")))
        .unwrap();
    let test_program = r#"(a (q 2 4 (c 2 (c 5 ()))) (c (q (a 6 (c 2 (c (c (q . "Running") (c (q 16 78 1) (c (q . " With ") (c 5 ())))) (c (+ 5 (q . 1)) ())))) 3 (all (q . "$print$") 5 11) 11 11) 1))"#;
    let assembled = assemble_text(test_program).unwrap();
    let args = Program::to(&[SExp::from(50)]);
    let results = assembled.run(INFINITE_COST, 0, &args).unwrap();
    info!("Output {results:?}");
}
