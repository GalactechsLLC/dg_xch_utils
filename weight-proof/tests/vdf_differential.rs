// VDF verification parity tests using captured vectors.

use dg_xch_vdf::proof::verify_vdf;
use std::path::PathBuf;

fn field<'a>(j: &'a str, key: &str) -> &'a str {
    let k = format!("\"{key}\"");
    let i = j.find(&k).unwrap_or_else(|| panic!("key {key} missing"));
    let after = &j[i + k.len()..];
    let q1 = after.find('"').unwrap();
    let rest = &after[q1 + 1..];
    &rest[..rest.find('"').unwrap()]
}
fn int_field(j: &str, key: &str) -> u64 {
    let k = format!("\"{key}\"");
    let i = j.find(&k).unwrap();
    let after = &j[i + k.len()..];
    after
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .parse()
        .unwrap()
}

/// Verify one JSON case's (challenge, x_s, proof, iters, disc_bits, witness_type) via the public dg_xch API.
fn verify_case(path: &std::path::Path) -> bool {
    let j = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
    let challenge = hex::decode(field(&j, "challenge")).unwrap();
    let x_s = hex::decode(field(&j, "x_s_input")).unwrap();
    let proof = hex::decode(field(&j, "proof")).unwrap();
    let iters = int_field(&j, "num_iterations");
    let db = int_field(&j, "discriminant_size_bits") as usize;
    let wt = int_field(&j, "recursion_witness_type");
    verify_vdf(&challenge, &x_s, &proof, db, iters, wt)
}

#[test]
fn all_thirteen_vectors_now_accept() {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut checked = 0usize;
    let mut rejected: Vec<String> = Vec::new();

    let original = base.join("vdf_differential_case.json");
    if verify_case(&original) {
        checked += 1;
    } else {
        rejected.push("vdf_differential_case.json".into());
    }

    let dir = base.join("vdf13");
    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    entries.sort();
    for p in &entries {
        if verify_case(p) {
            checked += 1;
        } else {
            rejected.push(p.file_name().unwrap().to_string_lossy().into_owned());
        }
    }

    eprintln!(
        "13-vector accept-parity: {checked} accepted, {} rejected",
        rejected.len()
    );
    assert!(
        rejected.is_empty(),
        "post-fix, these vectors are still REJECTED by dg_xch (chiavdf accepts them): {rejected:?}"
    );
    assert!(checked >= 13, "expected >=13 vectors, checked {checked}");
}
