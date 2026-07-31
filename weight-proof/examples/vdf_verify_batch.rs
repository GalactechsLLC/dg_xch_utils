// Batch VDF verifier for JSONL input.
use std::io::{BufRead, Write};
fn hx(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}
fn field<'a>(l: &'a str, k: &str) -> &'a str {
    let m = format!("\"{k}\":");
    let s = l.find(&m).unwrap() + m.len();
    let rest = &l[s..];
    let rest = rest.trim_start().trim_start_matches('"');
    let e = rest.find(['"', ',', '}']).unwrap();
    &rest[..e]
}
fn main() {
    let out = std::io::stdout();
    let mut out = out.lock();
    for line in std::io::stdin().lock().lines() {
        let l = line.unwrap();
        if l.trim().is_empty() {
            continue;
        }
        let id = field(&l, "id");
        let c = hx(field(&l, "challenge"));
        let x = hx(field(&l, "x_s_input"));
        let p = hx(field(&l, "proof"));
        let db: usize = field(&l, "discriminant_size_bits").parse().unwrap();
        let it: u64 = field(&l, "num_iterations").parse().unwrap();
        let wt: u64 = field(&l, "recursion_witness_type").parse().unwrap();
        let v = dg_xch_vdf::verify_vdf(&c, &x, &p, db, it, wt);
        writeln!(out, "{{\"id\":\"{id}\",\"dg_xch\":{v}}}").unwrap();
    }
}
