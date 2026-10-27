use dg_xch_core::clvm::arena::Arena;
use dg_xch_core::clvm::assemble::assemble_text;
use dg_xch_core::clvm::debug_ops::op_print;
use dg_xch_core::clvm::dialect::ChiaDialect;
use dg_xch_core::clvm::runtime::ClvmRuntime;
use dg_xch_core::clvm::sexp::SExp;
use dg_xch_core::clvm::utils::{DISABLE_PRINTS, MEMPOOL_MODE};
use log::{LevelFilter, Log, Metadata, Record};
use std::sync::Mutex;

struct PrintLogger(Mutex<Vec<String>>);

impl Log for PrintLogger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.target() == "dg_xch_core::clvm::debug_ops"
    }

    fn log(&self, record: &Record<'_>) {
        if self.enabled(record.metadata()) {
            self.0.lock().unwrap().push(record.args().to_string());
        }
    }

    fn flush(&self) {}
}

static LOGGER: PrintLogger = PrintLogger(Mutex::new(Vec::new()));

#[test]
fn disable_prints_preserves_results_costs_and_cost_limits() {
    log::set_logger(&LOGGER).unwrap();
    log::set_max_level(LevelFilter::Info);
    for source in [
        "(all (q . \"$print$\"))",
        "(all (q . \"$print$\") (q . \"label\"))",
        "(all (q . \"$print$\") (q . \"label\") (q . 42) (q . ()))",
    ] {
        let program = assemble_text(source).unwrap();
        for flags in [0, MEMPOOL_MODE] {
            LOGGER.0.lock().unwrap().clear();
            let expected = ClvmRuntime::new(u64::MAX, flags)
                .run(program.sexp(), &SExp::default())
                .unwrap();
            if source.contains("label") {
                assert!(!LOGGER.0.lock().unwrap().is_empty());
            }
            LOGGER.0.lock().unwrap().clear();
            let actual = ClvmRuntime::new(u64::MAX, flags | DISABLE_PRINTS)
                .run(program.sexp(), &SExp::default())
                .unwrap();
            assert_eq!(actual, expected);
            assert!(LOGGER.0.lock().unwrap().is_empty());
            for limit in [expected.0 - 1, expected.0] {
                let enabled = ClvmRuntime::new(limit, flags).run(program.sexp(), &SExp::default());
                let disabled = ClvmRuntime::new(limit, flags | DISABLE_PRINTS)
                    .run(program.sexp(), &SExp::default());
                assert_eq!(enabled.is_ok(), limit == expected.0);
                assert_eq!(disabled.is_ok(), enabled.is_ok());
            }
        }
    }
    let mut arena = Arena::new();
    let value = arena.import(&SExp::from(42)).unwrap();
    for count in [0, 1, 3] {
        let args = vec![value; count];
        let expected = op_print(&arena, &args, u64::MAX, &ChiaDialect::new(0)).unwrap();
        LOGGER.0.lock().unwrap().clear();
        let actual = op_print(&arena, &args, u64::MAX, &ChiaDialect::new(DISABLE_PRINTS)).unwrap();
        assert_eq!(actual, expected);
        assert!(LOGGER.0.lock().unwrap().is_empty());
    }
}
