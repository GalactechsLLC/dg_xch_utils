// Mempool dedup + singleton fast-forward. Real CLVM fixtures: a
// `singleton_top_layer_v1_1` singleton (inner puzzle `1`) run through the ACTUAL conditions
// runner, so eligibility flows exactly as in production — the ELIGIBLE_FOR_DEDUP /
// ELIGIBLE_FOR_FF flags at the spend-bundle conditions run, `supports_fast_forward` + the
// store's unspent-lineage lookup at admission, FF-aware conflict/double-spend classification,
// FF rebase at new-peak and at block assembly, and identical-spend deduplication with cost
// savings.

use dg_xch_core::blockchain::coin::Coin;
use dg_xch_core::blockchain::coin_record::CoinRecord;
use dg_xch_core::blockchain::coin_spend::CoinSpend;
use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes96};
use dg_xch_core::blockchain::spend::{ELIGIBLE_FOR_DEDUP, ELIGIBLE_FOR_FF};
use dg_xch_core::blockchain::spend_bundle::SpendBundle;
use dg_xch_core::clvm::program::{Program, SerializedProgram};
use dg_xch_core::clvm::sexp::SExp;
use dg_xch_core::clvm::utils::is_clvm_canonical;
use dg_xch_core::consensus::block_generator::conditions_from_spend_bundle;
use dg_xch_core::consensus::constants::MAINNET;
use dg_xch_core::consensus::fast_forward::supports_fast_forward;
use dg_xch_core::traits::SizedBytes;
use dg_xch_node::mempool::{Mempool, MempoolError};
use dg_xch_puzzles::clvm_puzzles::puzzle_for_singleton_v1_1;
use dg_xch_stores::{CoinStore, SqliteStore};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

static COUNTER: AtomicU64 = AtomicU64::new(0);

const PEAK_HEIGHT: u32 = MAINNET.soft_fork9_height + 100;
const PEAK_TIME: u64 = 1_000;
const SINGLETON_AMOUNT: u64 = 1023; // odd — singletons require it

async fn store() -> SqliteStore {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("dg_xch_t062_{}_{n}.sqlite", std::process::id()));
    SqliteStore::open(&path).await.expect("open store")
}

fn h(tag: u8) -> Bytes32 {
    Bytes32::from([tag; 32])
}

fn record(c: Coin, height: u32) -> CoinRecord {
    CoinRecord {
        coin: c,
        confirmed_block_index: height,
        spent_block_index: 0,
        coinbase: false,
        timestamp: PEAK_TIME,
        spent: false,
    }
}

// The G2 identity — a VALID (aggregatable) signature; bundle names stay distinct because every
// fixture bundle carries distinct coin spends.
fn infinity_sig(_tag: u8) -> Bytes96 {
    let mut sig = [0u8; 96];
    sig[0] = 0xc0;
    sig.into()
}

// The singleton fixture: inner puzzle `1` (the solution IS the condition list) wrapped in
// singleton_top_layer_v1_1, plus the derived lineage G -> PA -> C (all same puzzle hash+amount).
struct SingletonFixture {
    puzzle: Program<'static>,
    puzzle_hash: Bytes32,
    inner_hash: Bytes32,
    grandparent: Coin,
    parent: Coin,
    coin: Coin,
}

fn singleton_fixture(launcher_tag: u8) -> SingletonFixture {
    let inner = Program::to(1_u64);
    let inner_hash = inner.tree_hash();
    let puzzle =
        puzzle_for_singleton_v1_1(h(launcher_tag), &inner).expect("singleton puzzle curries");
    let puzzle_hash = puzzle.tree_hash();
    let grandparent = Coin {
        parent_coin_info: h(launcher_tag ^ 0xAA),
        puzzle_hash,
        amount: SINGLETON_AMOUNT,
    };
    let parent = Coin {
        parent_coin_info: grandparent.name(),
        puzzle_hash,
        amount: SINGLETON_AMOUNT,
    };
    let coin = Coin {
        parent_coin_info: parent.name(),
        puzzle_hash,
        amount: SINGLETON_AMOUNT,
    };
    SingletonFixture {
        puzzle,
        puzzle_hash,
        inner_hash,
        grandparent,
        parent,
        coin,
    }
}

impl SingletonFixture {
    // Solution `( (G inner_hash amount) amount ((51 inner_hash amount)) )`: a lineage-proof spend
    // recreating the singleton with the SAME inner puzzle (the FF-eligible shape).
    fn spend(&self) -> CoinSpend {
        let lineage: SExp<'static> = vec![
            SExp::from(self.grandparent.name()),
            SExp::from(self.inner_hash),
            SExp::from(SINGLETON_AMOUNT),
        ]
        .into();
        let create: SExp<'static> = vec![
            SExp::from(51_u64),
            SExp::from(self.inner_hash),
            SExp::from(SINGLETON_AMOUNT),
        ]
        .into();
        let inner_solution: SExp<'static> = vec![create].into();
        let solution: SExp<'static> =
            vec![lineage, SExp::from(SINGLETON_AMOUNT), inner_solution].into();
        CoinSpend {
            coin: self.coin,
            puzzle_reveal: self.puzzle.serialized().expect("puzzle serializes"),
            solution: Program::to(solution)
                .serialized()
                .expect("solution serializes"),
        }
    }
}

fn fee_spend(parent_tag: u8, amount: u64) -> (Coin, CoinSpend) {
    let puzzle = Program::to(1_u64);
    let coin = Coin {
        parent_coin_info: h(parent_tag),
        puzzle_hash: puzzle.tree_hash(),
        amount,
    };
    let create: SExp<'static> = vec![
        SExp::from(51_u64),
        SExp::from(h(parent_tag ^ 0x0F)),
        SExp::from(amount / 2),
    ]
    .into();
    let conditions: SExp<'static> = vec![create].into();
    let spend = CoinSpend {
        coin,
        puzzle_reveal: puzzle.serialized().expect("puzzle serializes"),
        solution: Program::to(conditions)
            .serialized()
            .expect("solution serializes"),
    };
    (coin, spend)
}

// A dedup-eligible spend: puzzle `1`, outputs its FULL amount (no excess), no signatures.
fn dedup_spend(parent_tag: u8, amount: u64) -> (Coin, CoinSpend) {
    let puzzle = Program::to(1_u64);
    let coin = Coin {
        parent_coin_info: h(parent_tag),
        puzzle_hash: puzzle.tree_hash(),
        amount,
    };
    let create: SExp<'static> = vec![
        SExp::from(51_u64),
        SExp::from(h(parent_tag ^ 0x0F)),
        SExp::from(amount),
    ]
    .into();
    let conditions: SExp<'static> = vec![create].into();
    let spend = CoinSpend {
        coin,
        puzzle_reveal: puzzle.serialized().expect("puzzle serializes"),
        solution: Program::to(conditions)
            .serialized()
            .expect("solution serializes"),
    };
    (coin, spend)
}

fn bundle(spends: Vec<CoinSpend>, sig_tag: u8) -> SpendBundle {
    SpendBundle {
        authorization: Default::default(),
        coin_spends: spends,
        aggregated_signature: infinity_sig(sig_tag),
    }
}

// Seed the store with the singleton lineage where the mempool-target coin C is ALREADY SPENT and
// its child C2 is the latest unspent version. Returns C2.
async fn seed_spent_singleton(store: &SqliteStore, fx: &SingletonFixture) -> Coin {
    let c2 = Coin {
        parent_coin_info: fx.coin.name(),
        puzzle_hash: fx.puzzle_hash,
        amount: SINGLETON_AMOUNT,
    };
    store
        .apply_block(90, PEAK_TIME, &[record(fx.grandparent, 90)], &[])
        .await
        .unwrap();
    store
        .apply_block(
            95,
            PEAK_TIME,
            &[record(fx.parent, 95)],
            &[fx.grandparent.name()],
        )
        .await
        .unwrap();
    store
        .apply_block(100, PEAK_TIME, &[record(fx.coin, 100)], &[fx.parent.name()])
        .await
        .unwrap();
    store
        .apply_block(101, PEAK_TIME, &[record(c2, 101)], &[fx.coin.name()])
        .await
        .unwrap();
    c2
}

// ---- eligibility flows from the conditions runner ------------------------------------------------

#[tokio::test]
async fn conditions_runner_computes_eligibility_flags() {
    let fx = singleton_fixture(0x11);
    let (_, fee) = fee_spend(0x21, 2_000);
    let b = bundle(vec![fx.spend(), fee], 0x01);
    let conds = conditions_from_spend_bundle(&b, PEAK_HEIGHT, &MAINNET).expect("bundle runs");

    let singleton = conds
        .spends
        .iter()
        .find(|s| s.coin_id == fx.coin.name())
        .expect("singleton spend present");
    assert!(
        singleton.flags & ELIGIBLE_FOR_FF != 0,
        "odd-amount singleton recreating itself must be ELIGIBLE_FOR_FF (flags={:#x})",
        singleton.flags
    );
    assert!(
        singleton.flags & ELIGIBLE_FOR_DEDUP != 0,
        "no agg-sigs, no messages, no excess: ELIGIBLE_FOR_DEDUP"
    );
    assert!(supports_fast_forward(&fx.spend()), "structural FF check");

    let fee = conds
        .spends
        .iter()
        .find(|s| s.coin_id != fx.coin.name())
        .expect("fee spend present");
    assert_eq!(
        fee.flags & ELIGIBLE_FOR_FF,
        0,
        "even-amount spend is never FF-eligible"
    );
    assert_eq!(
        fee.flags & ELIGIBLE_FOR_DEDUP,
        0,
        "excess amount (fee) disqualifies dedup"
    );
    assert!(
        fee.condition_cost + fee.execution_cost > 0,
        "per-spend costs must be attributed"
    );
}

// ---- FF admission: a spent singleton coin is NOT a double spend ---------------------------------

#[tokio::test]
async fn ff_spend_of_spent_singleton_admits_instead_of_double_spend() {
    let store = store().await;
    let fx = singleton_fixture(0x12);
    let _c2 = seed_spent_singleton(&store, &fx).await;
    let (fee_coin, fee) = fee_spend(0x22, 2_000);
    store
        .apply_block(100, PEAK_TIME, &[record(fee_coin, 100)], &[])
        .await
        .unwrap();

    let mut mp = Mempool::new(&MAINNET);
    mp.set_peak(PEAK_HEIGHT, PEAK_TIME);
    let b = bundle(vec![fx.spend(), fee], 0x02);
    let conds = conditions_from_spend_bundle(&b, PEAK_HEIGHT, &MAINNET).expect("bundle runs");
    let name = mp
        .admit(&store, b, conds)
        .await
        .expect("FF spend of a spent singleton must admit (rebased later), not DOUBLE_SPEND");
    let item = mp.get(&name).expect("resident");
    let bcs = item
        .bundle_coin_spend(&fx.coin.name())
        .expect("singleton bcs");
    assert!(bcs.supports_fast_forward(), "lineage resolved at admission");
}

// ---- all-FF bundles are structurally invalid ----------------------------------------------------

#[tokio::test]
async fn all_ff_bundle_rejected() {
    let store = store().await;
    let fx = singleton_fixture(0x13);
    seed_spent_singleton(&store, &fx).await;

    let mut mp = Mempool::new(&MAINNET);
    mp.set_peak(PEAK_HEIGHT, PEAK_TIME);
    let b = bundle(vec![fx.spend()], 0x03);
    let conds = conditions_from_spend_bundle(&b, PEAK_HEIGHT, &MAINNET).expect("bundle runs");
    let err = mp
        .admit(&store, b, conds)
        .await
        .expect_err("a bundle whose spends are ALL fast-forward must reject");
    assert_eq!(err.ack().1, "INVALID_SPEND_BUNDLE", "got {err:?}");
}

// ---- dedup: identical spends coexist; different solutions conflict ------------------------------

#[tokio::test]
async fn identical_dedup_spends_coexist_and_assemble_once() {
    let store = store().await;
    let (d_coin, d_spend) = dedup_spend(0x31, 500);
    let (f1_coin, f1) = fee_spend(0x32, 2_000);
    let (f2_coin, f2) = fee_spend(0x33, 4_000);
    store
        .apply_block(
            100,
            PEAK_TIME,
            &[
                record(d_coin, 100),
                record(f1_coin, 100),
                record(f2_coin, 100),
            ],
            &[],
        )
        .await
        .unwrap();

    let mut mp = Mempool::new(&MAINNET);
    mp.set_peak(PEAK_HEIGHT, PEAK_TIME);

    let b1 = bundle(vec![d_spend.clone(), f1], 0x04);
    let conds1 = conditions_from_spend_bundle(&b1, PEAK_HEIGHT, &MAINNET).expect("b1 runs");
    let cost1 = conds1.cost;
    let name1 = mp.admit(&store, b1, conds1).await.expect("first admits");

    let b2 = bundle(vec![d_spend.clone(), f2], 0x05);
    let conds2 = conditions_from_spend_bundle(&b2, PEAK_HEIGHT, &MAINNET).expect("b2 runs");
    let cost2 = conds2.cost;
    let name2 = mp
        .admit(&store, b2, conds2)
        .await
        .expect("identical dedup spend of the same coin must COEXIST, not conflict");
    assert_ne!(name1, name2);
    assert_eq!(mp.len(), 2, "both items resident");

    // Assembly: the dedup'd spend appears ONCE; the block's cost realizes the saving.
    let tx = mp
        .create_block_generator(&MAINNET, PEAK_HEIGHT + 1, Duration::from_secs(2))
        .expect("assembles");
    let dedup_count = tx
        .removals
        .iter()
        .filter(|c| c.name() == d_coin.name())
        .count();
    assert_eq!(dedup_count, 1, "identical spend deduplicated in the block");
    assert!(
        tx.removals.iter().any(|c| c.name() == f1_coin.name())
            && tx.removals.iter().any(|c| c.name() == f2_coin.name()),
        "both items' fee spends included"
    );
    assert!(
        tx.cost < cost1 + cost2,
        "block cost {} must be below the un-deduplicated sum {}",
        tx.cost,
        cost1 + cost2
    );
}

#[tokio::test]
async fn dedup_spend_with_different_solution_conflicts() {
    let store = store().await;
    let (d_coin, d_spend) = dedup_spend(0x41, 500);
    let (f1_coin, f1) = fee_spend(0x42, 2_000);
    let (f2_coin, f2) = fee_spend(0x43, 4_000);
    store
        .apply_block(
            100,
            PEAK_TIME,
            &[
                record(d_coin, 100),
                record(f1_coin, 100),
                record(f2_coin, 100),
            ],
            &[],
        )
        .await
        .unwrap();

    let mut mp = Mempool::new(&MAINNET);
    mp.set_peak(PEAK_HEIGHT, PEAK_TIME);
    let b1 = bundle(vec![d_spend, f1], 0x06);
    let conds1 = conditions_from_spend_bundle(&b1, PEAK_HEIGHT, &MAINNET).expect("b1 runs");
    mp.admit(&store, b1, conds1).await.expect("first admits");

    // Same coin, different (still dedup-eligible) solution: pays to a different puzzle hash.
    let puzzle = Program::to(1_u64);
    let create: SExp<'static> =
        vec![SExp::from(51_u64), SExp::from(h(0x77)), SExp::from(500_u64)].into();
    let conditions: SExp<'static> = vec![create].into();
    let different = CoinSpend {
        coin: d_coin,
        puzzle_reveal: puzzle.serialized().unwrap(),
        solution: Program::to(conditions).serialized().unwrap(),
    };
    let b2 = bundle(vec![different, f2], 0x07);
    let conds2 = conditions_from_spend_bundle(&b2, PEAK_HEIGHT, &MAINNET).expect("b2 runs");
    let err = mp
        .admit(&store, b2, conds2)
        .await
        .expect_err("differing dedup solutions cannot merge");
    assert!(matches!(err, MempoolError::Conflict(_)), "got {err:?}");
}

// ---- canonical-solution enforcement on dedup spends ---------------------------------------------

#[tokio::test]
async fn non_canonical_dedup_solution_rejected() {
    // ((51 <ph> 500)) with the amount atom length-encoded non-canonically (0xC0 0x02 vs 0x82).
    let out_ph = h(0x59);
    let mut canonical: Vec<u8> = vec![0xff, 0xff, 0x33, 0xff, 0xa0];
    canonical.extend_from_slice(&out_ph.bytes());
    canonical.extend_from_slice(&[0xff, 0x82, 0x01, 0xf4, 0x80, 0x80]);
    let mut non_canonical: Vec<u8> = vec![0xff, 0xff, 0x33, 0xff, 0xa0];
    non_canonical.extend_from_slice(&out_ph.bytes());
    non_canonical.extend_from_slice(&[0xff, 0xc0, 0x02, 0x01, 0xf4, 0x80, 0x80]);
    assert!(is_clvm_canonical(&canonical), "control: canonical bytes");
    assert!(
        !is_clvm_canonical(&non_canonical),
        "over-long length prefix is not canonical"
    );

    let store = store().await;
    let puzzle = Program::to(1_u64);
    let d_coin = Coin {
        parent_coin_info: h(0x51),
        puzzle_hash: puzzle.tree_hash(),
        amount: 500,
    };
    let (f_coin, f) = fee_spend(0x52, 2_000);
    store
        .apply_block(
            100,
            PEAK_TIME,
            &[record(d_coin, 100), record(f_coin, 100)],
            &[],
        )
        .await
        .unwrap();
    let spend = CoinSpend {
        coin: d_coin,
        puzzle_reveal: puzzle.serialized().unwrap(),
        solution: SerializedProgram::from_bytes(&non_canonical),
    };
    let b = bundle(vec![spend, f], 0x08);
    let conds = conditions_from_spend_bundle(&b, PEAK_HEIGHT, &MAINNET)
        .expect("non-canonical solutions still parse and run");
    let mut mp = Mempool::new(&MAINNET);
    mp.set_peak(PEAK_HEIGHT, PEAK_TIME);
    let err = mp
        .admit(&store, b, conds)
        .await
        .expect_err("dedup-eligible spend with a non-canonical solution must reject");
    assert_eq!(err.ack().1, "INVALID_COIN_SOLUTION", "got {err:?}");
}

// ---- replacement may not strip dedup eligibility ------------------------------------------------

#[tokio::test]
async fn replacement_must_preserve_dedup_eligibility() {
    let store = store().await;
    let (d_coin, d_spend) = dedup_spend(0x61, 500);
    let (f1_coin, f1) = fee_spend(0x62, 100_000_000);
    store
        .apply_block(
            100,
            PEAK_TIME,
            &[record(d_coin, 100), record(f1_coin, 100)],
            &[],
        )
        .await
        .unwrap();
    let mut mp = Mempool::new(&MAINNET);
    mp.set_peak(PEAK_HEIGHT, PEAK_TIME);
    let b1 = bundle(vec![d_spend.clone(), f1.clone()], 0x09);
    let conds1 = conditions_from_spend_bundle(&b1, PEAK_HEIGHT, &MAINNET).expect("b1 runs");
    mp.admit(&store, b1, conds1).await.expect("first admits");

    // Higher-fee replacement spending the same coins, but spending D with EXCESS (not dedup):
    // output only 100 of D's 500 — flags drop ELIGIBLE_FOR_DEDUP.
    let puzzle = Program::to(1_u64);
    let create: SExp<'static> =
        vec![SExp::from(51_u64), SExp::from(h(0x63)), SExp::from(100_u64)].into();
    let conditions: SExp<'static> = vec![create].into();
    let stripped = CoinSpend {
        coin: d_coin,
        puzzle_reveal: puzzle.serialized().unwrap(),
        solution: Program::to(conditions).serialized().unwrap(),
    };
    // Fee spend outputs almost nothing: a huge fee bump that would satisfy every fee rule.
    let bump_create: SExp<'static> =
        vec![SExp::from(51_u64), SExp::from(h(0x64)), SExp::from(1_u64)].into();
    let bump_conditions: SExp<'static> = vec![bump_create].into();
    let f1_replacement = CoinSpend {
        coin: f1_coin,
        puzzle_reveal: puzzle.serialized().unwrap(),
        solution: Program::to(bump_conditions).serialized().unwrap(),
    };
    let b2 = bundle(vec![stripped, f1_replacement], 0x0a);
    let conds2 = conditions_from_spend_bundle(&b2, PEAK_HEIGHT, &MAINNET).expect("b2 runs");
    let err = mp
        .admit(&store, b2, conds2)
        .await
        .expect_err("a replacement stripping ELIGIBLE_FOR_DEDUP must be rejected");
    assert!(matches!(err, MempoolError::Conflict(_)), "got {err:?}");
}

// ---- block assembly rebases FF spends onto the latest singleton version -------------------------

#[tokio::test]
async fn assembly_rebases_ff_spend_onto_latest_version() {
    let store = store().await;
    let fx = singleton_fixture(0x71);
    let c2 = seed_spent_singleton(&store, &fx).await;
    let (fee_coin, fee) = fee_spend(0x72, 2_000);
    store
        .apply_block(100, PEAK_TIME, &[record(fee_coin, 100)], &[])
        .await
        .unwrap();

    let mut mp = Mempool::new(&MAINNET);
    mp.set_peak(PEAK_HEIGHT, PEAK_TIME);
    let b = bundle(vec![fx.spend(), fee], 0x0b);
    let conds = conditions_from_spend_bundle(&b, PEAK_HEIGHT, &MAINNET).expect("bundle runs");
    mp.admit(&store, b, conds).await.expect("admits");

    let tx = mp
        .create_block_generator(&MAINNET, PEAK_HEIGHT + 1, Duration::from_secs(2))
        .expect("assembles");
    assert!(
        tx.removals.iter().any(|c| c.name() == c2.name()),
        "the FF spend must be REBASED onto the latest unspent version"
    );
    assert!(
        !tx.removals.iter().any(|c| c.name() == fx.coin.name()),
        "the stale singleton coin must not appear in the block"
    );
}

// ---- new peak: FF items rebase (or die) via the spent-coin index --------------------------------

#[tokio::test]
async fn new_peak_rebases_ff_item_then_evicts_when_singleton_dies() {
    let store = store().await;
    let fx = singleton_fixture(0x81);
    let c2 = seed_spent_singleton(&store, &fx).await;
    let (fee_coin, fee) = fee_spend(0x82, 2_000);
    store
        .apply_block(100, PEAK_TIME, &[record(fee_coin, 100)], &[])
        .await
        .unwrap();

    let mut mp = Mempool::new(&MAINNET);
    mp.set_peak(PEAK_HEIGHT, PEAK_TIME);
    let b = bundle(vec![fx.spend(), fee], 0x0c);
    let conds = conditions_from_spend_bundle(&b, PEAK_HEIGHT, &MAINNET).expect("bundle runs");
    let name = mp.admit(&store, b, conds).await.expect("admits");

    // Block 102 spends C2, creating C3 — the item survives, rebased onto C3.
    let c3 = Coin {
        parent_coin_info: c2.name(),
        puzzle_hash: fx.puzzle_hash,
        amount: SINGLETON_AMOUNT,
    };
    store
        .apply_block(102, PEAK_TIME + 10, &[record(c3, 102)], &[c2.name()])
        .await
        .unwrap();
    let result = mp
        .new_peak(&store, PEAK_HEIGHT + 1, PEAK_TIME + 10, &[c2.name()])
        .await
        .expect("new peak");
    assert_eq!(result.dropped, 0, "FF item must be REBASED, not dropped");
    let item = mp.get(&name).expect("still resident");
    let bcs = item.bundle_coin_spend(&fx.coin.name()).expect("bcs");
    assert_eq!(
        bcs.latest_singleton_lineage.map(|l| l.coin_id),
        Some(c3.name()),
        "lineage advanced to the new version"
    );

    // Block 103 spends C3 with NO same-puzzle child (the singleton melts): the item dies.
    store
        .apply_block(103, PEAK_TIME + 20, &[], &[c3.name()])
        .await
        .unwrap();
    let result = mp
        .new_peak(&store, PEAK_HEIGHT + 2, PEAK_TIME + 20, &[c3.name()])
        .await
        .expect("new peak");
    assert_eq!(result.dropped, 1, "no unspent version left: evict");
    assert!(mp.get(&name).is_none());
}

// ---- O(delta) fast path + the reorg slow path ---------------------------------------

#[tokio::test]
async fn new_peak_touches_only_spent_coin_owners_and_reorg_uses_slow_path() {
    let store = store().await;
    let (x_coin, x_spend) = fee_spend(0x91, 2_000);
    store
        .apply_block(100, PEAK_TIME, &[record(x_coin, 100)], &[])
        .await
        .unwrap();
    let mut mp = Mempool::new(&MAINNET);
    mp.set_peak(PEAK_HEIGHT, PEAK_TIME);
    let b = bundle(vec![x_spend], 0x0d);
    let conds = conditions_from_spend_bundle(&b, PEAK_HEIGHT, &MAINNET).expect("runs");
    let name = mp.admit(&store, b, conds).await.expect("admits");

    // The store learns X was spent, but the peak's spent list does NOT mention it (out-of-band
    // change). The fast path touches only items indexed by the block's spent coins — the
    // resident item MUST survive: no O(pool) store re-scan per peak.
    store
        .apply_block(102, PEAK_TIME + 10, &[], &[x_coin.name()])
        .await
        .unwrap();
    let result = mp
        .new_peak(&store, PEAK_HEIGHT + 1, PEAK_TIME + 10, &[])
        .await
        .expect("new peak");
    assert_eq!(
        result.dropped, 0,
        "fast path must not re-query the store for unrelated items"
    );
    assert!(mp.get(&name).is_some());

    // The reorg slow path DOES the full store re-check and drops it (spent removal).
    let dropped = mp.revalidate_for_reorg(&store).await.expect("revalidate");
    assert_eq!(dropped, 1, "slow path drops the spent-removal item");
    assert!(mp.get(&name).is_none());
}

#[tokio::test]
async fn reorg_revalidation_drops_unknown_unspent_items() {
    let store = store().await;
    let (y_coin, y_spend) = fee_spend(0xA1, 2_000);
    store
        .apply_block(100, PEAK_TIME, &[record(y_coin, 100)], &[])
        .await
        .unwrap();
    let mut mp = Mempool::new(&MAINNET);
    mp.set_peak(PEAK_HEIGHT, PEAK_TIME);
    let b = bundle(vec![y_spend], 0x0e);
    let conds = conditions_from_spend_bundle(&b, PEAK_HEIGHT, &MAINNET).expect("runs");
    let name = mp.admit(&store, b, conds).await.expect("admits");

    // Roll the store back below Y's creation: the removal ceases to exist
    // (UNKNOWN_UNSPENT on the slow path).
    store.rollback_to(99).await.expect("rollback");
    let dropped = mp.revalidate_for_reorg(&store).await.expect("revalidate");
    assert_eq!(dropped, 1, "rolled-back removal: item dies UNKNOWN_UNSPENT");
    assert!(mp.get(&name).is_none());
}
