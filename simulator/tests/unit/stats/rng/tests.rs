use super::*;
use rand_chacha::rand_core::Rng;
use std::collections::HashSet;

fn head(run_seed: u64, domain: Domain, index: u64) -> [u8; 64] {
    let mut rng = derive_rng(run_seed, domain, index);
    let mut out = [0u8; 64];
    rng.fill_bytes(&mut out);
    out
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn a_triple_always_yields_the_same_bytes() {
    for domain in Domain::ALL {
        assert_eq!(head(42, domain, 3), head(42, domain, 3));
    }
}

#[test]
fn creation_order_does_not_affect_any_substream() {
    let forward: Vec<[u8; 64]> = Domain::ALL.iter().map(|d| head(7, *d, 0)).collect();
    let backward: Vec<[u8; 64]> = Domain::ALL
        .iter()
        .rev()
        .map(|d| head(7, *d, 0))
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    assert_eq!(forward, backward);
}

#[test]
fn seed_domain_and_index_each_separate_the_keystream() {
    let mut seen = HashSet::new();
    for run_seed in [0u64, 1, 2] {
        for domain in Domain::ALL {
            for index in [0u64, 1, 2, 1000] {
                assert!(
                    seen.insert(head(run_seed, domain, index)),
                    "substreams collided at ({run_seed}, {domain:?}, {index})"
                );
            }
        }
    }
}

#[test]
fn an_index_selects_a_stream_not_a_rehashed_seed() {
    let a = derive_rng(9, Domain::NetLink, 0);
    let b = derive_rng(9, Domain::NetLink, 17);
    assert_eq!(a.get_seed(), b.get_seed());
    assert_eq!(a.get_stream(), 0);
    assert_eq!(b.get_stream(), 17);
    assert_eq!(a.get_word_pos(), 0);
    assert_eq!(b.get_word_pos(), 0);
}

/// Frozen first 64 bytes per triple. A `rand_chacha` bump, a changed framing tag, or a renamed
/// domain all move these.
const GOLDEN: &[(u64, Domain, u64, &str)] = &[
    (
        0,
        Domain::PoSpace,
        0,
        "ad324438b1d3b47ca31118d21bd8b90b6bd9d517a0110cd1a504d12166144e06\
         c0175d0da5436d878cb5bb0434e0827db08d30ca044eeabaca8b6c5c90eb8cf4",
    ),
    (
        1,
        Domain::PoSpace,
        0,
        "f6bfb7bf1320f4d451eef770a043ddad3d90e693db4c2a1ef0c148fde4986dd3\
         2563b2d0d00b9913819a1407792d7111dcaa1f465a3e8930e8d17a99a86c00ed",
    ),
    (
        1,
        Domain::PoSpace,
        1,
        "80207a2a6e61560446cb8a9445c868d9e96645e87800bfb93878884a8fc14d5a\
         b08b1a6d5ca4d70462459625bfe01b601c404362ad2d6966e7b9cefdb41ffe8e",
    ),
    (
        1,
        Domain::Quality,
        0,
        "992e653393ce81b6a6a45c8d700d9a06905a262ed88123c280fee49e1ee5e3d2\
         add13433e1995093a237c2abea0ac06e73080293a3d1d5bff9f0c0cae596bec1",
    ),
    (
        0xDEAD_BEEF_CAFE_F00D,
        Domain::Timelord,
        7,
        "6426acf8d8454be7bb0012d27dc7919d84be25ec5bc28631f7ece6341fef9943\
         0556b1bfd0cb46235733886508dc8e8f8ff49e5922bb9497aaf1dc97e4bde573",
    ),
];

#[test]
fn value_stability_vector() {
    let mut drift = Vec::new();
    for (run_seed, domain, index, expected) in GOLDEN {
        let actual = hex(&head(*run_seed, *domain, *index));
        if actual != *expected {
            drift.push(format!(
                "  ({run_seed}, Domain::{domain:?}, {index}) expected {expected:?} got {actual:?}"
            ));
        }
    }
    assert!(
        drift.is_empty(),
        "the RNG substream vector moved:\n{}",
        drift.join("\n")
    );
}
