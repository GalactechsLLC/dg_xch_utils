// Differential gates: the deduped tree hash must be indistinguishable from the
// naive per-spend `SExp::tree_hash` — on randomized Arc-shared trees (the shape the
// backref parser produces), on real corpus generators (env-gated), and through the
// public mempool bundle path.

use dg_xch_core::blockchain::coin::Coin;
use dg_xch_core::blockchain::coin_spend::CoinSpend;
use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes96};
use dg_xch_core::blockchain::spend_bundle::SpendBundle;
use dg_xch_core::clvm::program::SerializedProgram;
use dg_xch_core::clvm::sexp::{AtomBuf, PairBuf, SExp};
use dg_xch_core::clvm::tree_hash_cache::TreeHashCache;
use dg_xch_core::consensus::block_generator::conditions_from_spend_bundle;
use dg_xch_core::consensus::constants::MAINNET;
use dg_xch_core::traits::SizedBytes;
use std::sync::Arc;

// Deterministic xorshift64* — no dependencies, reproducible cases.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

// Build a random tree, drawing from (and feeding) a pool of shared subtrees the way
// `sexp_from_bytes_backrefs` does: a back-reference reuses child `Arc`s, so repeated
// subtrees are pointer-shared.
fn random_tree(
    rng: &mut Rng,
    pool: &mut Vec<Arc<SExp<'static>>>,
    depth: u32,
) -> Arc<SExp<'static>> {
    let roll = rng.below(100);
    if !pool.is_empty() && roll < 30 {
        // shared reuse — the backref case
        let idx = rng.below(pool.len() as u64) as usize;
        return pool[idx].clone();
    }
    let node: Arc<SExp<'static>> = if depth == 0 || roll < 65 {
        let len = rng.below(40) as usize;
        let bytes: Vec<u8> = (0..len).map(|_| (rng.next() & 0xff) as u8).collect();
        Arc::new(SExp::Atom(AtomBuf::new(bytes)))
    } else {
        let first = random_tree(rng, pool, depth - 1);
        let rest = random_tree(rng, pool, depth - 1);
        Arc::new(SExp::Pair(PairBuf::Owned((first, rest))))
    };
    if rng.below(100) < 40 {
        pool.push(node.clone());
    }
    node
}

#[test]
fn cached_tree_hash_matches_naive_on_randomized_shared_trees() {
    let mut rng = Rng(0x0001_84c0_ffee_d00d);
    let mut checked = 0u32;
    for case in 0..2000 {
        let mut pool = Vec::new();
        // A "block" of 1..=8 reveals drawing from one shared pool — the cache is
        // shared across all of them, exactly like the per-block reveal walk.
        let reveal_count = 1 + rng.below(8) as usize;
        let reveals: Vec<Arc<SExp<'static>>> = (0..reveal_count)
            .map(|_| random_tree(&mut rng, &mut pool, 6))
            .collect();
        let mut cache = TreeHashCache::default();
        for r in &reveals {
            cache.visit_tree(r);
        }
        for r in &reveals {
            let cached = cache.tree_hash(r);
            let naive = r.tree_hash();
            assert_eq!(cached, naive, "case {case}: cached != naive");
            // hashing the same node again must return the identical hash
            assert_eq!(cache.tree_hash(r), naive, "case {case}: rehash != naive");
            checked += 1;
        }
    }
    assert!(
        checked > 4000,
        "expected thousands of differential cases, got {checked}"
    );
}

#[test]
fn cached_tree_hash_matches_naive_on_deep_unshared_tree() {
    // No sharing at all: the cache must degrade to plain sha256tree.
    let mut node: SExp<'static> = SExp::Atom(AtomBuf::new(vec![7u8]));
    for i in 0..200u32 {
        node = SExp::Pair(PairBuf::Owned((
            Arc::new(SExp::Atom(AtomBuf::new(i.to_be_bytes().to_vec()))),
            Arc::new(node),
        )));
    }
    let mut cache = TreeHashCache::default();
    cache.visit_tree(&node);
    assert_eq!(cache.tree_hash(&node), node.tree_hash());
}

// Mempool path: repeated reveals hash once but semantics are unchanged, and a wrong
// puzzle hash is still rejected.
#[test]
fn spend_bundle_reveal_dedup_preserves_semantics() {
    // puzzle `1` (the identity program) returns its solution; solution `()` → no conditions.
    let reveal = SerializedProgram::from_bytes(&[0x01]);
    let reveal_hash: Bytes32 = SExp::Atom(AtomBuf::new(vec![1u8])).tree_hash();
    let solution = SerializedProgram::from_bytes(&[0x80]);
    let mk_spend = |parent_byte: u8, puzzle_hash: Bytes32| CoinSpend {
        coin: Coin {
            parent_coin_info: Bytes32::new([parent_byte; 32]),
            puzzle_hash,
            amount: 1,
        },
        puzzle_reveal: reveal.clone(),
        solution: solution.clone(),
    };
    let bundle = SpendBundle {
        authorization: Default::default(),
        coin_spends: vec![
            mk_spend(1, reveal_hash),
            mk_spend(2, reveal_hash),
            mk_spend(3, reveal_hash),
        ],
        aggregated_signature: Bytes96::default(),
    };
    let height = MAINNET.hard_fork_height + 10;
    let conds = conditions_from_spend_bundle(&bundle, height, &MAINNET)
        .expect("repeated-reveal bundle validates");
    assert_eq!(conds.spends.len(), 3);
    for s in &conds.spends {
        assert_eq!(s.puzzle_hash, reveal_hash);
    }

    // Same bundle, one wrong puzzle hash: still WrongPuzzleHash.
    let bad = SpendBundle {
        authorization: Default::default(),
        coin_spends: vec![
            mk_spend(1, reveal_hash),
            mk_spend(2, Bytes32::new([0xAB; 32])),
        ],
        aggregated_signature: Bytes96::default(),
    };
    let err = conditions_from_spend_bundle(&bad, height, &MAINNET);
    assert!(err.is_err(), "wrong puzzle hash must still be rejected");
}
