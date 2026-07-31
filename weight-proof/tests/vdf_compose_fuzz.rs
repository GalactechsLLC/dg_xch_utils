// Composition parity tests using independent BQF vectors.

use dg_xch_vdf::form::Form;
use num_bigint::BigInt;
use std::path::PathBuf;
use std::str::FromStr;

fn bi(s: &str) -> BigInt {
    BigInt::from_str(s).expect("bigint")
}
fn form_of(v: &[BigInt]) -> Form {
    Form {
        a: v[0].clone(),
        b: v[1].clone(),
        c: v[2].clone(),
    }
}
fn triple(form: &Form) -> (BigInt, BigInt, BigInt) {
    (form.a.clone(), form.b.clone(), form.c.clone())
}

#[test]
fn compose_matches_reference_over_battery() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bqf_vectors.txt");
    let data = std::fs::read_to_string(&path).expect("bqf vectors present");
    let mut sq = 0usize;
    let mut mul = 0usize;
    let mut failures: Vec<String> = Vec::new();

    for line in data.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (lhs, rhs) = line.split_once('|').expect("vector has '|'");
        let ins: Vec<BigInt> = lhs.split_whitespace().skip(1).map(bi).collect();
        let out: Vec<BigInt> = rhs.split_whitespace().map(bi).collect();
        let expected = (out[0].clone(), out[1].clone(), out[2].clone());

        if line.starts_with("SQ") {
            sq += 1;
            let got = form_of(&ins).square().expect("square ok");
            if triple(&got) != expected {
                failures.push(format!(
                    "SQ ({},{},{}) ref=({},{},{}) got=({},{},{})",
                    ins[0], ins[1], ins[2], out[0], out[1], out[2], got.a, got.b, got.c
                ));
            }
        } else if line.starts_with("MUL") {
            mul += 1;
            let got = form_of(&ins[0..3])
                .multiply(&form_of(&ins[3..6]))
                .expect("multiply ok");
            if triple(&got) != expected {
                failures.push(format!(
                    "MUL ({},{},{})x({},{},{}) ref=({},{},{}) got=({},{},{})",
                    ins[0],
                    ins[1],
                    ins[2],
                    ins[3],
                    ins[4],
                    ins[5],
                    out[0],
                    out[1],
                    out[2],
                    got.a,
                    got.b,
                    got.c
                ));
            }
        }
    }

    eprintln!(
        "compose fuzz: {sq} squares + {mul} multiplies checked; {} divergences",
        failures.len()
    );
    assert!(
        failures.is_empty(),
        "dg_xch compose diverges from reference (residual gcd>1 bug reproducers):\n{}",
        failures.join("\n")
    );
}
