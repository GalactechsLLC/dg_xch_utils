use dg_xch_core::clvm::assemble::assemble_text;
use dg_xch_core::clvm::program::Program;
use dg_xch_core::clvm::runtime::ClvmRuntime;
use dg_xch_core::clvm::sexp::{AtomBuf, SExp};
use dg_xch_core::clvm::utils::MEMPOOL_MODE;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

const GOLDEN_PATH: &str = "tests/fixtures/clvm_op_golden.json";

const VECTORS: &[(&str, &str, Option<&str>)] = &[
    // arithmetic
    ("add", "(+ (q . 10) (q . 13))", Some("23")),
    ("add_nil", "(+)", Some("0")),
    ("add_cancel", "(+ (q . -1) (q . 1))", Some("0")),
    (
        "add_big",
        "(+ (q . 0x00ffffffffffffffff) (q . 1))",
        Some("0x010000000000000000"),
    ),
    ("sub", "(- (q . 5) (q . 11))", None), // -6; negative display, golden-only
    ("sub_nil", "(-)", Some("0")),
    ("mul", "(* (q . -128) (q . 2))", None), // -256; golden-only
    ("mul_nil", "(*)", Some("1")),
    ("div", "(/ (q . 10) (q . 3))", Some("3")),
    ("div_exact", "(/ (q . 12) (q . 4))", Some("3")),
    // negative division semantics changed across chain history; pin whatever the current VM
    // does per flag set rather than hand-guess it.
    ("div_neg", "(/ (q . -10) (q . 3))", None),
    ("divmod", "(divmod (q . 10) (q . 3))", Some("(3 . 1)")),
    ("divmod_neg", "(divmod (q . -10) (q . 3))", None), // (-4 . 2); golden-only
    // comparison
    ("eq_true", "(= (q . 1) (q . 1))", Some("1")),
    ("eq_false", "(= (q . 1) (q . 2))", Some("()")),
    ("eq_empty", "(= (q . 0) (q . ()))", Some("1")),
    ("gr_true", "(> (q . 3) (q . 2))", Some("1")),
    ("gr_signed", "(> (q . -1) (q . 1))", Some("()")),
    ("grs", "(>s (q . \"b\") (q . \"a\"))", Some("1")),
    ("grs_prefix", "(>s (q . \"ab\") (q . \"a\"))", Some("1")),
    // atoms
    ("strlen", "(strlen (q . \"clvm\"))", Some("4")),
    ("strlen_nil", "(strlen (q . ()))", Some("0")),
    (
        "substr",
        "(substr (q . \"clvm\") (q . 1) (q . 3))",
        Some("\"lv\""),
    ),
    (
        "substr_all",
        "(substr (q . \"clvm\") (q . 0) (q . 4))",
        Some("\"clvm\""),
    ),
    (
        "substr_empty",
        "(substr (q . \"clvm\") (q . 2) (q . 2))",
        Some("()"),
    ),
    (
        "concat",
        "(concat (q . \"cl\") (q . \"vm\"))",
        Some("\"clvm\""),
    ),
    ("concat_nil", "(concat)", Some("()")),
    // control
    (
        "if_true",
        "(i (q . 1) (q . \"yes\") (q . \"no\"))",
        Some("\"yes\""),
    ),
    (
        "if_false",
        "(i (q . ()) (q . \"yes\") (q . \"no\"))",
        Some("\"no\""),
    ),
    ("cons", "(c (q . 1) (q . (2 3)))", Some("(1 2 3)")),
    ("first", "(f (q . (1 2)))", Some("1")),
    ("rest", "(r (q . (1 2)))", Some("(2)")),
    ("listp_pair", "(l (q . (1 2)))", Some("1")),
    ("listp_atom", "(l (q . 1))", Some("()")),
    ("apply_env", "(a (q . 1) (q . 42))", Some("42")),
    // shifts and bit logic
    ("ash_left", "(ash (q . 1) (q . 8))", Some("256")),
    ("ash_right", "(ash (q . 256) (q . -8))", Some("1")),
    ("ash_neg_sticky", "(ash (q . -1) (q . -1))", None), // -1; golden-only
    ("lsh_left", "(lsh (q . 1) (q . 8))", Some("256")),
    ("lsh_neg_bytes", "(lsh (q . -1) (q . 1))", Some("510")),
    ("logand", "(logand (q . 12) (q . 10))", Some("8")),
    ("logior", "(logior (q . 12) (q . 10))", Some("14")),
    ("logxor", "(logxor (q . 12) (q . 10))", Some("6")),
    ("lognot_zero", "(lognot (q . ()))", None), // -1; golden-only
    ("lognot_neg", "(lognot (q . -1))", Some("0")),
    // boolean
    ("not_nil", "(not (q . ()))", Some("1")),
    ("not_atom", "(not (q . 1))", Some("()")),
    ("any_mixed", "(any (q . ()) (q . 1))", Some("1")),
    ("any_none", "(any (q . ()) (q . ()))", Some("()")),
    ("all_mixed", "(all (q . ()) (q . 1))", Some("()")),
    ("all_true", "(all (q . 1) (q . 2))", Some("1")),
    // A recognized softfork executes its body and discards the result.
    (
        "softfork",
        "(softfork (q . 160) (q . 0) (q . (q . 42)) (q . ()))",
        Some("()"),
    ),
];

// Semantic failures: the VM must reject these, and the exact error text is the regression pin.
const ERROR_VECTORS: &[(&str, &str)] = &[
    ("raise", "(x (q . \"boom\"))"),
    ("div_zero", "(/ (q . 1) (q . 0))"),
    ("divmod_zero", "(divmod (q . 1) (q . 0))"),
    ("first_of_atom", "(f (q . 1))"),
    ("rest_of_atom", "(r (q . 1))"),
    ("strlen_of_pair", "(strlen (q . (1)))"),
    (
        "substr_backwards",
        "(substr (q . \"clvm\") (q . 3) (q . 1))",
    ),
    ("substr_past_end", "(substr (q . \"clvm\") (q . 0) (q . 5))"),
    ("ash_over_65535", "(ash (q . 1) (q . 65536))"),
    ("softfork_zero_cost", "(softfork (q . 0))"),
];

fn quoted(value: SExp<'static>) -> SExp<'static> {
    SExp::Pair(dg_xch_core::clvm::sexp::PairBuf::from((
        SExp::Atom(AtomBuf::new(vec![1])),
        value,
    )))
}

fn opcall(op: u8, args: Vec<SExp<'static>>) -> SExp<'static> {
    let mut items = vec![SExp::Atom(AtomBuf::new(vec![op]))];
    items.extend(args.into_iter().map(quoted));
    SExp::from(items)
}

// Ops without an assembler keyword, built structurally: coinid (48), modpow (60), mod (61).
fn structural_vectors() -> Vec<(&'static str, SExp<'static>, Option<String>)> {
    let parent = [1u8; 32];
    let ph = [2u8; 32];
    let mut hasher = Sha256::new();
    hasher.update(parent);
    hasher.update(ph);
    hasher.update([100u8]);
    let coin_id = hasher.finalize();
    let coin_id_src = format!("0x{}", hex::encode(coin_id));
    vec![
        (
            "coinid",
            opcall(
                48,
                vec![
                    SExp::Atom(AtomBuf::new(parent.to_vec())),
                    SExp::Atom(AtomBuf::new(ph.to_vec())),
                    SExp::from(100),
                ],
            ),
            Some(coin_id_src),
        ),
        (
            "modpow",
            opcall(60, vec![SExp::from(2), SExp::from(10), SExp::from(1000)]),
            Some("24".to_string()),
        ),
        (
            "mod_op",
            opcall(61, vec![SExp::from(10), SExp::from(3)]),
            Some("1".to_string()),
        ),
        (
            "mod_op_neg",
            opcall(61, vec![SExp::from(-10), SExp::from(3)]),
            None, // floored-mod remainder; golden-only
        ),
    ]
}

fn run(program: &SExp, flags: u32) -> Result<(u64, SExp<'static>), String> {
    let nil = SExp::Atom(AtomBuf::new(vec![]));
    let mut runtime = ClvmRuntime::new(u64::MAX, flags);
    runtime.run(program, &nil).map_err(|e| format!("{e:?}"))
}

fn display_of(sexp: &SExp) -> String {
    sexp.to_string()
}

fn expected_display(src: &str) -> String {
    let prog: Program = assemble_text(src).expect("hand oracle assembles");
    display_of(prog.sexp())
}

#[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
struct GoldenEntry {
    value: Option<String>,
    cost: Option<u64>,
    error: Option<String>,
}

type Golden = BTreeMap<String, GoldenEntry>;

fn collect(flags: u32, mode: &str) -> Golden {
    let mut out = Golden::new();
    for (name, src, oracle) in VECTORS {
        let prog: Program = assemble_text(src).unwrap_or_else(|e| panic!("{name}: {e:?}"));
        let (cost, output) = run(prog.sexp(), flags)
            .unwrap_or_else(|e| panic!("{mode}/{name}: expected success, got {e}"));
        let value = display_of(&output);
        if let Some(oracle_src) = oracle {
            assert_eq!(
                value,
                expected_display(oracle_src),
                "{mode}/{name}: output diverged from the hand-computed CLVM semantics"
            );
        }
        out.insert(
            format!("{mode}/{name}"),
            GoldenEntry {
                value: Some(value),
                cost: Some(cost),
                error: None,
            },
        );
    }
    for (name, program, oracle) in structural_vectors() {
        let (cost, output) = run(&program, flags)
            .unwrap_or_else(|e| panic!("{mode}/{name}: expected success, got {e}"));
        let value = display_of(&output);
        if let Some(oracle_src) = oracle {
            assert_eq!(
                value,
                expected_display(&oracle_src),
                "{mode}/{name}: output diverged from the hand-computed CLVM semantics"
            );
        }
        out.insert(
            format!("{mode}/{name}"),
            GoldenEntry {
                value: Some(value),
                cost: Some(cost),
                error: None,
            },
        );
    }
    for (name, src) in ERROR_VECTORS {
        let prog: Program = assemble_text(src).unwrap_or_else(|e| panic!("{name}: {e:?}"));
        let err = match run(prog.sexp(), flags) {
            Err(e) => e,
            Ok((_, v)) => panic!(
                "{mode}/{name}: the VM accepted an invalid program and returned {}",
                display_of(&v)
            ),
        };
        out.insert(
            format!("{mode}/{name}"),
            GoldenEntry {
                value: None,
                cost: None,
                error: Some(err),
            },
        );
    }
    out
}

#[test]
fn operator_vectors_match_golden_in_both_modes() {
    let mut all = collect(0, "base");
    all.extend(collect(MEMPOOL_MODE, "mempool"));

    if std::env::var("UPDATE_GOLDEN").is_ok() {
        std::fs::write(
            GOLDEN_PATH,
            serde_json::to_string_pretty(&all).expect("serializes"),
        )
        .expect("golden file writes");
        eprintln!("  wrote {} entries to {GOLDEN_PATH}", all.len());
        return;
    }

    let stored = std::fs::read_to_string(GOLDEN_PATH)
        .expect("golden file missing — harvest once with UPDATE_GOLDEN=1");
    let stored: Golden = serde_json::from_str(&stored).expect("golden file parses");
    for (name, got) in &all {
        let want = stored.get(name).unwrap_or_else(|| {
            panic!("{name}: no golden entry — a new vector needs a UPDATE_GOLDEN=1 harvest")
        });
        assert_eq!(
            got, want,
            "{name}: the VM diverged from its pinned behavior (value, cost, or error text)"
        );
    }
    assert_eq!(
        all.len(),
        stored.len(),
        "golden file has entries no vector produces — stale after a vector rename?"
    );
    eprintln!("  {} operator vectors hold in both modes", all.len());
}

#[test]
fn operator_results_are_deterministic() {
    // Same program, two fresh runtimes, byte-identical output and cost — catches any
    // iteration-order or uninitialized-state nondeterminism in the arena.
    for (name, src, _) in VECTORS {
        let prog: Program = assemble_text(src).unwrap_or_else(|e| panic!("{name}: {e:?}"));
        let a = run(prog.sexp(), 0).unwrap_or_else(|e| panic!("{name}: {e}"));
        let b = run(prog.sexp(), 0).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(a.0, b.0, "{name}: cost differs between identical runs");
        assert_eq!(
            display_of(&a.1),
            display_of(&b.1),
            "{name}: output differs between identical runs"
        );
    }
}
