mod common;

use async_trait::async_trait;
use dg_xch_core::blockchain::block_record::BlockRecord;
use dg_xch_core::blockchain::coin_record::CoinRecord;
use dg_xch_core::blockchain::full_block::FullBlock;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::clvm::program::SerializedProgram;
use dg_xch_core::consensus::constants::MAINNET;
use dg_xch_core::traits::SizedBytes;
use dg_xch_node::engine::BlockDelta;
use dg_xch_node::{AddBlockOutcome, Engine, NativePrimitives};
use dg_xch_stores::{
    BatchHandle, BlockStatus, BlockStore, CoinStore, MmapStore, Savepoint, StoreError,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

fn synth_hash(tag: u8, height: u32) -> Bytes32 {
    let mut h = [tag; 32];
    h[28..32].copy_from_slice(&height.to_be_bytes());
    Bytes32::from(h)
}

// A deterministic delta over a real mainnet coin set. `coin_src` selects the fixture whose additions/removals
// this synthetic block carries; heights/weights/links are chosen to build two competing branches.
fn delta(
    template: &BlockRecord,
    tag: u8,
    height: u32,
    weight: u128,
    prev_hash: Bytes32,
    coin_src: Option<u32>,
) -> BlockDelta {
    let ts = 1_700_000_000u64 + u64::from(height);
    let mut record = template.clone();
    record.header_hash = synth_hash(tag, height);
    record.prev_hash = prev_hash;
    record.height = height;
    record.weight = weight;
    record.total_iters = weight;
    record.timestamp = Some(ts);
    record.sub_epoch_summary_included = None;

    let (additions, removals) = match coin_src {
        Some(h) => {
            let (adds, rems) = common::load_adds_rems(h);
            let additions: Vec<CoinRecord> = adds
                .into_iter()
                .map(|a| CoinRecord {
                    coin: a.coin,
                    confirmed_block_index: height,
                    spent_block_index: 0,
                    coinbase: a.coinbase,
                    timestamp: ts,
                    spent: false,
                })
                .collect();
            let removals: Vec<Bytes32> = rems.iter().map(|c| c.coin.name()).collect();
            (additions, removals)
        }
        None => (Vec::new(), Vec::new()),
    };
    BlockDelta {
        header_hash: synth_hash(tag, height),
        prev_hash,
        height,
        weight,
        timestamp: ts,
        record,
        additions,
        removals,
        hints: Vec::new(),
    }
}

async fn all_coin_records(
    store: &impl CoinStore,
    names: &[Bytes32],
) -> Vec<(Bytes32, Option<CoinRecord>)> {
    let mut out = Vec::new();
    for n in names {
        out.push((*n, store.get_coin_record(n).await.unwrap()));
    }
    out.sort_by_key(|(n, _)| n.bytes());
    out
}

// Fed a heavier branch, the engine reorgs; the coin store byte-equals a full replay of the heavier
// chain. Branches are constructed deterministically from real mainnet coin sets (deep reorgs
// are rare on mainnet, so the scenario is built).
#[tokio::test]
async fn heavier_branch_reorg_coin_store_equals_replay() {
    let records = common::load_records();
    let template = &records[0];

    // Common base B0@100 (no coins). Branch A: A1@101(5000000), A2@102(5000004), tip weight 1200.
    // Branch B (heavier): B1@101(5000007), B2@102(5000012), tip weight 1350.
    let b0 = delta(template, 0, 100, 1000, Bytes32::from([0u8; 32]), None);
    let a1 = delta(template, 0xa1, 101, 1100, b0.header_hash, Some(5_000_000));
    let a2 = delta(template, 0xa2, 102, 1200, a1.header_hash, Some(5_000_004));
    let b1 = delta(template, 0xb1, 101, 1150, b0.header_hash, Some(5_000_007));
    let b2 = delta(template, 0xb2, 102, 1350, b1.header_hash, Some(5_000_012));

    let store = common::new_store().await;
    let mut engine = Engine::new(store, NativePrimitives, MAINNET);

    assert_eq!(
        engine.add_delta(b0.clone()).await.unwrap(),
        AddBlockOutcome::NewPeak { height: 100 }
    );
    assert_eq!(
        engine.add_delta(a1.clone()).await.unwrap(),
        AddBlockOutcome::Extended { height: 101 }
    );
    assert_eq!(
        engine.add_delta(a2.clone()).await.unwrap(),
        AddBlockOutcome::Extended { height: 102 }
    );
    // B1 is lighter than the current peak (A2) — it parks as an orphan branch.
    assert_eq!(
        engine.add_delta(b1.clone()).await.unwrap(),
        AddBlockOutcome::Orphan { height: 101 }
    );
    // B2 makes branch B heavier (1350 > 1200) → reorg, forking at height 100.
    assert_eq!(
        engine.add_delta(b2.clone()).await.unwrap(),
        AddBlockOutcome::Reorg {
            fork_height: 100,
            links: 2
        }
    );

    // Peak is now the branch-B tip.
    let peak = engine.store().get_peak().await.unwrap().unwrap();
    assert_eq!(peak, (b2.header_hash, 102));

    // Byte-equality proof: replay [B0, B1, B2] into a fresh store; every coin the heavier chain touches must
    // match, and every branch-A-only coin must be absent in both.
    let replay_store = common::new_store().await;
    for d in [&b0, &b1, &b2] {
        replay_store
            .apply_block(d.height, d.timestamp, &d.additions, &d.removals)
            .await
            .unwrap();
    }

    let b_names: Vec<Bytes32> = b1
        .additions
        .iter()
        .chain(b2.additions.iter())
        .map(|c| c.coin.name())
        .collect();
    assert!(!b_names.is_empty(), "branch B created coins");
    let reorged = all_coin_records(engine.store(), &b_names).await;
    let replay = all_coin_records(&replay_store, &b_names).await;
    assert_eq!(
        reorged, replay,
        "reorged coin store byte-equals a full replay of the heavier chain"
    );

    // Branch-A-only additions are gone after the reorg (rolled back, never in the replay).
    let a_names: Vec<Bytes32> = a1
        .additions
        .iter()
        .chain(a2.additions.iter())
        .map(|c| c.coin.name())
        .filter(|n| !b_names.contains(n))
        .collect();
    let a_after = engine.store().get_coin_records(&a_names).await.unwrap();
    assert!(
        a_after.is_empty(),
        "branch-A-only coins are reverted by the reorg"
    );
}

// Reorg wallet delta: a landed reorg must surface the rolled-back records and the re-applied
// branch, so the server can push the post-rollback coin states to wallet subscribers.
#[tokio::test]
async fn reorg_report_carries_rollback_states_and_the_reapplied_branch() {
    let records = common::load_records();
    let template = &records[0];

    // Base B0@100 additionally creates coin X (below the fork); branch A's first block SPENDS X
    // and creates its own coins; branch B never touches X.
    let x = CoinRecord {
        coin: dg_xch_core::blockchain::coin::Coin {
            parent_coin_info: Bytes32::from([0xC0; 32]),
            puzzle_hash: Bytes32::from([0xC1; 32]),
            amount: 1_000,
        },
        confirmed_block_index: 100,
        spent_block_index: 0,
        coinbase: false,
        timestamp: 1_700_000_000,
        spent: false,
    };
    let mut b0 = delta(template, 0, 100, 1000, Bytes32::from([0u8; 32]), None);
    b0.additions.push(x);
    let mut a1 = delta(template, 0xa1, 101, 1100, b0.header_hash, Some(5_000_000));
    a1.removals.push(x.coin.name());
    let a2 = delta(template, 0xa2, 102, 1200, a1.header_hash, Some(5_000_004));
    let b1 = delta(template, 0xb1, 101, 1150, b0.header_hash, Some(5_000_007));
    let b2 = delta(template, 0xb2, 102, 1350, b1.header_hash, Some(5_000_012));

    let store = common::new_store().await;
    let mut engine = Engine::new(store, NativePrimitives, MAINNET);
    for d in [&b0, &a1, &a2, &b1] {
        engine.add_delta(d.clone()).await.unwrap();
    }
    assert!(
        engine.pop_reorg_report().is_none(),
        "no report before a reorg lands"
    );
    assert_eq!(
        engine.add_delta(b2.clone()).await.unwrap(),
        AddBlockOutcome::Reorg {
            fork_height: 100,
            links: 2
        }
    );

    let report = engine
        .pop_reorg_report()
        .expect("exactly one report per landed reorg");
    assert!(
        engine.pop_reorg_report().is_none(),
        "the report queue drains FIFO, one per reorg"
    );
    assert_eq!(report.fork_height, 100);
    assert_eq!(
        report
            .reapplied
            .iter()
            .map(|d| (d.height, d.header_hash))
            .collect::<Vec<_>>(),
        vec![(101, b1.header_hash), (102, b2.header_hash)],
        "the winning branch fork+1..=tip in height order"
    );

    // Coin X (spent on the abandoned branch, created below the fork) reverts to UNSPENT.
    let x_state = report
        .rolled_back
        .iter()
        .find(|r| r.coin == x.coin)
        .expect("the branch-A spend of X is rolled back");
    assert_eq!(x_state.spent_block_index, 0, "unspent again");
    assert!(!x_state.spent);
    assert_eq!(x_state.confirmed_block_index, 100, "creation stands");

    // Every branch-A-only addition reverts to not-on-chain (confirmed_block_index = 0,
    // timestamp = 0).
    let b_names: std::collections::HashSet<Bytes32> = b1
        .additions
        .iter()
        .chain(b2.additions.iter())
        .map(|c| c.coin.name())
        .collect();
    let mut a_only = 0;
    for cr in a1.additions.iter().chain(a2.additions.iter()) {
        let name = cr.coin.name();
        if b_names.contains(&name) {
            continue;
        }
        let rolled = report
            .rolled_back
            .iter()
            .find(|r| r.coin.name() == name)
            .expect("branch-A addition present in the rollback report");
        assert_eq!(rolled.confirmed_block_index, 0, "no longer on chain");
        assert_eq!(rolled.timestamp, 0);
        a_only += 1;
    }
    assert!(a_only > 0, "the fixture branches must diverge in coins");
}

// ---------------------------------------------------------------------------------------------
// Reorg atomicity under a mid-reorg crash. The ENTIRE reorg — rollback, per-block coin
// re-applies, the main-chain pointer flips, and the peak — runs inside one store transaction, so
// a crash anywhere means the reorg never happened. These tests inject a store fault at the two
// interior seams (before the first branch re-apply; before the peak flip) and assert the store is
// left EXACTLY as it was — never "coins reverted above the fork while the peak still points at the
// old branch". The FaultStore uses a real backend underneath with one call
// armed to fail.
// ---------------------------------------------------------------------------------------------

/// A real store with two armable fault points: the first `apply_block`/`apply_block_in` (the
/// branch re-apply seam right after the fork rollback) and the first `set_peak`/`set_peak_in`
/// (the pointer-flip seam after the branch is re-applied). Everything else delegates.
struct FaultStore<S> {
    inner: S,
    fail_apply: Arc<AtomicBool>,
    fail_set_peak: Arc<AtomicBool>,
}

impl<S> FaultStore<S> {
    fn new(inner: S) -> (Self, Arc<AtomicBool>, Arc<AtomicBool>) {
        let fail_apply = Arc::new(AtomicBool::new(false));
        let fail_set_peak = Arc::new(AtomicBool::new(false));
        (
            Self {
                inner,
                fail_apply: fail_apply.clone(),
                fail_set_peak: fail_set_peak.clone(),
            },
            fail_apply,
            fail_set_peak,
        )
    }

    fn injected(site: &str) -> StoreError {
        StoreError::Corrupt(format!("injected {site} fault"))
    }
}

#[async_trait]
impl<S: CoinStore + Send + Sync> CoinStore for FaultStore<S> {
    async fn get_coin_record(&self, coin_name: &Bytes32) -> Result<Option<CoinRecord>, StoreError> {
        self.inner.get_coin_record(coin_name).await
    }
    // The coin-index tier (enabled for the test build by the dev-dependency feature): pure
    // delegation — the fault seams above are the only injected behavior.
    async fn get_unspent_by_puzzle_hash(
        &self,
        ph: &Bytes32,
    ) -> Result<Vec<CoinRecord>, StoreError> {
        self.inner.get_unspent_by_puzzle_hash(ph).await
    }
    async fn get_coins_by_parent(&self, parent: &Bytes32) -> Result<Vec<CoinRecord>, StoreError> {
        self.inner.get_coins_by_parent(parent).await
    }
    async fn get_coins_added_at_height(&self, height: u32) -> Result<Vec<CoinRecord>, StoreError> {
        self.inner.get_coins_added_at_height(height).await
    }
    async fn get_coins_removed_at_height(
        &self,
        height: u32,
    ) -> Result<Vec<CoinRecord>, StoreError> {
        self.inner.get_coins_removed_at_height(height).await
    }
    async fn get_coin_states_by_puzzle_hashes(
        &self,
        puzzle_hashes: &[Bytes32],
        min_height: u32,
        include_spent: bool,
        max_items: usize,
    ) -> Result<Vec<dg_xch_core::protocols::wallet::CoinState>, StoreError> {
        self.inner
            .get_coin_states_by_puzzle_hashes(puzzle_hashes, min_height, include_spent, max_items)
            .await
    }
    async fn batch_coin_states_by_puzzle_hashes(
        &self,
        puzzle_hashes: &[Bytes32],
        min_height: u32,
        filters: &dg_xch_core::protocols::wallet::CoinStateFilters,
        max_items: usize,
    ) -> Result<(Vec<dg_xch_core::protocols::wallet::CoinState>, Option<u32>), StoreError> {
        self.inner
            .batch_coin_states_by_puzzle_hashes(puzzle_hashes, min_height, filters, max_items)
            .await
    }
    async fn get_coin_records(&self, names: &[Bytes32]) -> Result<Vec<CoinRecord>, StoreError> {
        self.inner.get_coin_records(names).await
    }
    async fn apply_block(
        &self,
        height: u32,
        timestamp: u64,
        additions: &[CoinRecord],
        removals: &[Bytes32],
    ) -> Result<(), StoreError> {
        if self.fail_apply.load(Ordering::Relaxed) {
            return Err(Self::injected("apply_block"));
        }
        self.inner
            .apply_block(height, timestamp, additions, removals)
            .await
    }
    async fn apply_block_in(
        &self,
        batch: &mut BatchHandle,
        height: u32,
        timestamp: u64,
        additions: &[CoinRecord],
        removals: &[Bytes32],
    ) -> Result<(), StoreError> {
        if self.fail_apply.load(Ordering::Relaxed) {
            return Err(Self::injected("apply_block_in"));
        }
        self.inner
            .apply_block_in(batch, height, timestamp, additions, removals)
            .await
    }
    async fn rollback_to(&self, fork_height: u32) -> Result<u64, StoreError> {
        self.inner.rollback_to(fork_height).await
    }
    async fn rollback_to_in(
        &self,
        batch: &mut BatchHandle,
        fork_height: u32,
    ) -> Result<u64, StoreError> {
        self.inner.rollback_to_in(batch, fork_height).await
    }
    async fn apply_hints_in(
        &self,
        batch: &mut BatchHandle,
        pairs: &[(Bytes32, Bytes32)],
    ) -> Result<(), StoreError> {
        self.inner.apply_hints_in(batch, pairs).await
    }
    async fn apply_hints(&self, pairs: &[(Bytes32, Bytes32)]) -> Result<(), StoreError> {
        self.inner.apply_hints(pairs).await
    }
    // hint tier: pure delegation (the fault seams live on the write path, not the hint scan).
    #[cfg(feature = "hint")]
    async fn get_coins_for_hint(
        &self,
        hint: &Bytes32,
        max_items: usize,
    ) -> Result<Vec<Bytes32>, StoreError> {
        self.inner.get_coins_for_hint(hint, max_items).await
    }
}

#[async_trait]
impl<S: BlockStore + Send + Sync> BlockStore for FaultStore<S> {
    async fn get_block_record(&self, hh: &Bytes32) -> Result<Option<BlockRecord>, StoreError> {
        self.inner.get_block_record(hh).await
    }
    async fn get_block_record_by_height(&self, h: u32) -> Result<Option<BlockRecord>, StoreError> {
        self.inner.get_block_record_by_height(h).await
    }
    async fn get_peak(&self) -> Result<Option<(Bytes32, u32)>, StoreError> {
        self.inner.get_peak().await
    }
    async fn min_record_height(&self) -> Result<Option<u32>, StoreError> {
        self.inner.min_record_height().await
    }
    async fn get_block(&self, hh: &Bytes32) -> Result<Option<FullBlock>, StoreError> {
        self.inner.get_block(hh).await
    }
    async fn get_generator_at_height(
        &self,
        h: u32,
    ) -> Result<Option<SerializedProgram>, StoreError> {
        self.inner.get_generator_at_height(h).await
    }
    async fn add_block_records(&self, records: &[BlockRecord]) -> Result<(), StoreError> {
        self.inner.add_block_records(records).await
    }
    async fn add_block_records_in(
        &self,
        batch: &mut BatchHandle,
        records: &[BlockRecord],
    ) -> Result<(), StoreError> {
        self.inner.add_block_records_in(batch, records).await
    }
    async fn begin(&self) -> Result<BatchHandle, StoreError> {
        self.inner.begin().await
    }
    async fn append_many(
        &self,
        batch: &mut BatchHandle,
        blocks: &[FullBlock],
    ) -> Result<(), StoreError> {
        self.inner.append_many(batch, blocks).await
    }
    async fn commit(&self, batch: BatchHandle) -> Result<(), StoreError> {
        self.inner.commit(batch).await
    }
    async fn get_unassociated(&self, limit: usize) -> Result<Vec<u32>, StoreError> {
        self.inner.get_unassociated(limit).await
    }
    async fn set_peak(&self, new_peak: &Bytes32) -> Result<u64, StoreError> {
        if self.fail_set_peak.load(Ordering::Relaxed) {
            return Err(Self::injected("set_peak"));
        }
        self.inner.set_peak(new_peak).await
    }
    async fn set_peak_in(
        &self,
        batch: &mut BatchHandle,
        new_peak: &Bytes32,
    ) -> Result<u64, StoreError> {
        if self.fail_set_peak.load(Ordering::Relaxed) {
            return Err(Self::injected("set_peak_in"));
        }
        self.inner.set_peak_in(batch, new_peak).await
    }
    async fn get_status(&self, hh: &Bytes32) -> Result<BlockStatus, StoreError> {
        self.inner.get_status(hh).await
    }
    async fn set_status(&self, hh: &Bytes32, s: BlockStatus) -> Result<(), StoreError> {
        self.inner.set_status(hh, s).await
    }
    async fn set_status_in(
        &self,
        batch: &mut BatchHandle,
        hh: &Bytes32,
        s: BlockStatus,
    ) -> Result<(), StoreError> {
        self.inner.set_status_in(batch, hh, s).await
    }
    async fn savepoint(&self) -> Result<Savepoint, StoreError> {
        self.inner.savepoint().await
    }
    async fn rollback(&self, sp: Savepoint) -> Result<u64, StoreError> {
        self.inner.rollback(sp).await
    }
    async fn get_sub_epoch_segments(
        &self,
        ses_hash: &Bytes32,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        self.inner.get_sub_epoch_segments(ses_hash).await
    }
    async fn persist_sub_epoch_segments(
        &self,
        ses_hash: &Bytes32,
        bytes: &[u8],
    ) -> Result<(), StoreError> {
        self.inner.persist_sub_epoch_segments(ses_hash, bytes).await
    }
    async fn build_indexes(&self) -> Result<(), StoreError> {
        self.inner.build_indexes().await
    }
}

// The five-delta two-branch scenario shared by the crash tests (same construction as
// heavier_branch_reorg_coin_store_equals_replay): base B0@100, branch A = peak, branch B heavier.
struct TwoBranches {
    b0: BlockDelta,
    a1: BlockDelta,
    a2: BlockDelta,
    b1: BlockDelta,
    b2: BlockDelta,
}

fn two_branches() -> TwoBranches {
    let records = common::load_records();
    let template = &records[0];
    let b0 = delta(template, 0, 100, 1000, Bytes32::from([0u8; 32]), None);
    let a1 = delta(template, 0xa1, 101, 1100, b0.header_hash, Some(5_000_000));
    let a2 = delta(template, 0xa2, 102, 1200, a1.header_hash, Some(5_000_004));
    let b1 = delta(template, 0xb1, 101, 1150, b0.header_hash, Some(5_000_007));
    let b2 = delta(template, 0xb2, 102, 1350, b1.header_hash, Some(5_000_012));
    TwoBranches { b0, a1, a2, b1, b2 }
}

// Drive the engine to the pre-reorg state: peak = A2@102, branch B parked as orphans through B1.
async fn seed_pre_reorg<S: CoinStore + BlockStore + Sync>(
    engine: &mut Engine<S, NativePrimitives>,
    tb: &TwoBranches,
) {
    assert_eq!(
        engine.add_delta(tb.b0.clone()).await.unwrap(),
        AddBlockOutcome::NewPeak { height: 100 }
    );
    assert_eq!(
        engine.add_delta(tb.a1.clone()).await.unwrap(),
        AddBlockOutcome::Extended { height: 101 }
    );
    assert_eq!(
        engine.add_delta(tb.a2.clone()).await.unwrap(),
        AddBlockOutcome::Extended { height: 102 }
    );
    assert_eq!(
        engine.add_delta(tb.b1.clone()).await.unwrap(),
        AddBlockOutcome::Orphan { height: 101 }
    );
}

// Every coin name either branch touches — the domain over which the store must byte-equal a
// replay of whichever chain is confirmed.
fn touched_names(tb: &TwoBranches) -> Vec<Bytes32> {
    let mut names: Vec<Bytes32> = tb
        .a1
        .additions
        .iter()
        .chain(tb.a2.additions.iter())
        .chain(tb.b1.additions.iter())
        .chain(tb.b2.additions.iter())
        .map(|c| c.coin.name())
        .chain(
            tb.a1
                .removals
                .iter()
                .chain(tb.a2.removals.iter())
                .chain(tb.b1.removals.iter())
                .chain(tb.b2.removals.iter())
                .copied(),
        )
        .collect();
    names.sort_by_key(|n| n.bytes());
    names.dedup();
    names
}

// The invariant the crash must not break: the peak's coin set is exactly the sum of its chain's
// deltas. Concretely: peak == expected tip AND the store's coin records over every touched name
// byte-equal a fresh replay of the expected chain.
async fn assert_peak_chain_consistent<S: CoinStore + BlockStore>(
    store: &S,
    expected_peak: (&BlockDelta, u32),
    expected_chain: &[&BlockDelta],
    names: &[Bytes32],
    context: &str,
) {
    let peak = store.get_peak().await.unwrap().unwrap();
    assert_eq!(
        peak,
        (expected_peak.0.header_hash, expected_peak.1),
        "{context}: peak"
    );
    let replay_store = common::new_store().await;
    for d in expected_chain {
        replay_store
            .apply_block(d.height, d.timestamp, &d.additions, &d.removals)
            .await
            .unwrap();
    }
    let actual = all_coin_records(store, names).await;
    let expected = all_coin_records(&replay_store, names).await;
    assert_eq!(
        actual, expected,
        "{context}: the peak's coin set must be exactly the sum of its chain's deltas"
    );
    // The confirmed by-height chain must agree with the expected chain too (a torn reorg can
    // leave coins reverted while in_main_chain still says the old branch — or vice versa).
    for d in expected_chain {
        let by_height = store
            .get_block_record_by_height(d.height)
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("{context}: height {} confirmed", d.height));
        assert_eq!(
            by_height.header_hash, d.header_hash,
            "{context}: main chain at height {}",
            d.height
        );
    }
}

// T0-4 red/green (seam 1): the store faults on the FIRST branch re-apply — right after the fork
// rollback has (on the torn code) already committed. The failed reorg must leave the store exactly
// at the pre-reorg state (peak A2, coins = replay of [B0,A1,A2]); the retry must then complete.
#[tokio::test]
async fn crash_before_branch_apply_leaves_store_untouched_and_retry_succeeds() {
    let tb = two_branches();
    let (store, fail_apply, _fail_set_peak) = FaultStore::new(common::new_store().await);
    let mut engine = Engine::new(store, NativePrimitives, MAINNET);
    seed_pre_reorg(&mut engine, &tb).await;
    let names = touched_names(&tb);

    fail_apply.store(true, Ordering::Relaxed);
    engine
        .add_delta(tb.b2.clone())
        .await
        .expect_err("injected apply fault surfaces");
    assert_peak_chain_consistent(
        engine.store(),
        (&tb.a2, 102),
        &[&tb.b0, &tb.a1, &tb.a2],
        &names,
        "after crashed reorg (apply seam)",
    )
    .await;

    fail_apply.store(false, Ordering::Relaxed);
    assert_eq!(
        engine.add_delta(tb.b2.clone()).await.unwrap(),
        AddBlockOutcome::Reorg {
            fork_height: 100,
            links: 2
        }
    );
    assert_peak_chain_consistent(
        engine.store(),
        (&tb.b2, 102),
        &[&tb.b0, &tb.b1, &tb.b2],
        &names,
        "after retried reorg",
    )
    .await;
}

// T0-4 red/green (seam 2): the store faults on the peak flip — after the rollback AND the whole
// branch re-apply have (on the torn code) already committed. Same invariant: untouched, then a
// clean retry.
#[tokio::test]
async fn crash_before_peak_flip_leaves_store_untouched_and_retry_succeeds() {
    let tb = two_branches();
    let (store, _fail_apply, fail_set_peak) = FaultStore::new(common::new_store().await);
    let mut engine = Engine::new(store, NativePrimitives, MAINNET);
    seed_pre_reorg(&mut engine, &tb).await;
    let names = touched_names(&tb);

    fail_set_peak.store(true, Ordering::Relaxed);
    engine
        .add_delta(tb.b2.clone())
        .await
        .expect_err("injected set_peak fault surfaces");
    assert_peak_chain_consistent(
        engine.store(),
        (&tb.a2, 102),
        &[&tb.b0, &tb.a1, &tb.a2],
        &names,
        "after crashed reorg (peak seam)",
    )
    .await;

    fail_set_peak.store(false, Ordering::Relaxed);
    assert_eq!(
        engine.add_delta(tb.b2.clone()).await.unwrap(),
        AddBlockOutcome::Reorg {
            fork_height: 100,
            links: 2
        }
    );
    assert_peak_chain_consistent(
        engine.store(),
        (&tb.b2, 102),
        &[&tb.b0, &tb.b1, &tb.b2],
        &names,
        "after retried reorg",
    )
    .await;
}

// T0-4 on the mmap backend: same engine-level crash at the branch re-apply seam. The mmap store
// has no transactions — its reorg mutations must be DEFERRED to the batch durability point, so a
// mid-reorg failure leaves only unreferenced log frames (invisible to every read), never a swept
// coin table under an unmoved peak. After the retried reorg the state must also survive a
// close-and-reopen (the crash-consistency files are the store).
#[tokio::test]
async fn mmap_crash_mid_reorg_leaves_store_untouched_and_retry_survives_reopen() {
    let tb = two_branches();
    let dir = tempfile::tempdir().expect("tempdir");
    let mmap = MmapStore::open(dir.path()).await.expect("open mmap store");
    let (store, fail_apply, _fail_set_peak) = FaultStore::new(mmap);
    let mut engine = Engine::new(store, NativePrimitives, MAINNET);
    seed_pre_reorg(&mut engine, &tb).await;
    let names = touched_names(&tb);

    fail_apply.store(true, Ordering::Relaxed);
    engine
        .add_delta(tb.b2.clone())
        .await
        .expect_err("injected apply fault surfaces");
    assert_peak_chain_consistent(
        engine.store(),
        (&tb.a2, 102),
        &[&tb.b0, &tb.a1, &tb.a2],
        &names,
        "after crashed reorg (mmap)",
    )
    .await;

    fail_apply.store(false, Ordering::Relaxed);
    assert_eq!(
        engine.add_delta(tb.b2.clone()).await.unwrap(),
        AddBlockOutcome::Reorg {
            fork_height: 100,
            links: 2
        }
    );
    assert_peak_chain_consistent(
        engine.store(),
        (&tb.b2, 102),
        &[&tb.b0, &tb.b1, &tb.b2],
        &names,
        "after retried reorg (mmap)",
    )
    .await;

    // Reopen from disk: the completed reorg is durable and no recovery journal is left behind.
    drop(engine);
    let reopened = MmapStore::open(dir.path()).await.expect("reopen");
    assert_peak_chain_consistent(
        &reopened,
        (&tb.b2, 102),
        &[&tb.b0, &tb.b1, &tb.b2],
        &names,
        "after reopen (mmap)",
    )
    .await;
}
