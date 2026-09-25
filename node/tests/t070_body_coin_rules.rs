//! Coin-store body validation on the live add-block path — body rules 3, 5, 10, 11, 13-21.
//!
//! Body validation runs for EVERY block added, on both the single-block and batch paths.
//! There is no body-validation skip window.
//! dg_xch runs them on every transaction block, gated only on FULL COIN HISTORY (a
//! `--sync-from` anchored store has no coin set below its anchor, so the store-backed rules are
//! undefined there; the pure structural rules still run).
//!
//! Vehicle: real mainnet block 5,000,004 validated on top of the confirmed fixture records
//! 5,000,000..=5,000,003, with its 223 removals seeded as unspent coin rows at their real heights
//! and the reward-claim walk grounded by two sub-fixture records (4,999,999 non-tx + 4,999,998 tx,
//! carrying the claim puzzle hashes the block itself declares). The honest block must ACCEPT
//! under full enforcement; each negative case below breaks exactly one rule.

mod common;

use dg_xch_core::blockchain::block_record::BlockRecord;
use dg_xch_core::blockchain::coin::Coin;
use dg_xch_core::blockchain::coin_record::CoinRecord;
use dg_xch_core::blockchain::condition_with_args::ConditionWithArgs;
use dg_xch_core::blockchain::foliage_transaction_block::FoliageTransactionBlock;
use dg_xch_core::blockchain::full_block::FullBlock;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::blockchain::spend_bundle::SpendBundle;
use dg_xch_core::blockchain::transactions_info::TransactionsInfo;
use dg_xch_core::clvm::program::{Program, SerializedProgram};
use dg_xch_core::clvm::sexp::SExp;
use dg_xch_core::consensus::block_filter::chia_block_filter;
use dg_xch_core::consensus::block_generator::{
    BlockGeneratorFlags, BlockGeneratorInput, additions_for_conditions, additions_root,
    execute_block_generator_result, removals_for_conditions, removals_root,
    transactions_generator_refs_root, transactions_generator_root, transactions_info_hash,
};
use dg_xch_core::consensus::block_rewards::{calculate_base_farmer_reward, calculate_pool_reward};
use dg_xch_core::consensus::coinbase::{
    create_farmer_coin, create_pool_coin, farmer_parent_id, pool_parent_id,
};
use dg_xch_core::consensus::constants::MAINNET;
use dg_xch_core::errors::ChiaError;
use dg_xch_core::traits::SizedBytes;
use dg_xch_core::utils::hash_256;
use dg_xch_node::engine::BlockDelta;
use dg_xch_node::{AddBlockOutcome, Engine, NativePrimitives, NodeError};
use dg_xch_stores::{BlockStore, CoinStore, SqliteStore};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

fn consensus_err(err: NodeError) -> ChiaError {
    match err {
        NodeError::Consensus(c) => c,
        other => panic!("expected a consensus rejection, got {other:?}"),
    }
}

const H: u32 = 5_000_004;

fn delta_for(record: &BlockRecord, additions: Vec<CoinRecord>) -> BlockDelta {
    BlockDelta {
        header_hash: record.header_hash,
        prev_hash: record.prev_hash,
        height: record.height,
        weight: record.weight,
        timestamp: record.timestamp.unwrap_or(0),
        record: record.clone(),
        additions,
        removals: Vec::new(),
        hints: Vec::new(),
    }
}

/// The two records below the fixture range that ground block 5,000,004's reward-claim walk:
/// 4,999,999 (non-transaction — its pool+farmer rewards are claimed by 5,000,004) and 4,999,998
/// (transaction — the walk's stop). Puzzle hashes come from the block's OWN declared claims,
/// selected by the height-derived coinbase parent ids, so the expected set the walk computes
/// matches the honest block exactly (create_pool_coin/create_farmer_coin are byte-faithful).
fn sub_fixture_records(block: &FullBlock, records: &[BlockRecord]) -> (BlockRecord, BlockRecord) {
    let claims = &block
        .transactions_info
        .as_ref()
        .expect("tx block")
        .reward_claims_incorporated;
    let pool_parent = pool_parent_id(4_999_999, MAINNET.genesis_challenge);
    let farmer_parent = farmer_parent_id(4_999_999, MAINNET.genesis_challenge);
    let pool_ph = claims
        .iter()
        .find(|c| c.parent_coin_info == pool_parent)
        .expect("pool claim for 4,999,999")
        .puzzle_hash;
    let farmer_ph = claims
        .iter()
        .find(|c| c.parent_coin_info == farmer_parent)
        .expect("farmer claim for 4,999,999")
        .puzzle_hash;

    // records[1] = 5,000,001, a non-transaction record (timestamp None) — the 4,999,999 template.
    let mut r999 = records[1].clone();
    r999.header_hash = records[0].prev_hash;
    r999.height = 4_999_999;
    r999.prev_hash = common::synth_hash(0xee, 4_999_998);
    r999.pool_puzzle_hash = pool_ph;
    r999.farmer_puzzle_hash = farmer_ph;
    r999.sub_epoch_summary_included = None;
    assert!(!r999.is_transaction_block());

    // records[0] = 5,000,000, a transaction record (timestamp Some) — the 4,999,998 walk-stop.
    let mut r998 = records[0].clone();
    r998.header_hash = r999.prev_hash;
    r998.height = 4_999_998;
    r998.prev_hash = common::synth_hash(0xee, 4_999_997);
    r998.sub_epoch_summary_included = None;
    assert!(r998.is_transaction_block());
    (r999, r998)
}

/// Confirmed chain 5,000,000..=5,000,003 (real records; block 5,000,000's real coin additions),
/// walk-grounding records seeded, and block 5,000,004's removals inserted unspent at their real
/// confirmed heights — except `withhold` (never inserted) and `pre_spend` (inserted already spent
/// at its real spent height, below the fork point).
async fn seeded_store(withhold: Option<Bytes32>, pre_spend: Option<Bytes32>) -> SqliteStore {
    let records = common::load_records();
    let block4 = common::load_full_block(H);
    let store = common::new_store().await;

    let (r999, r998) = sub_fixture_records(&block4, &records);
    store.add_block_records(&[r998, r999]).await.unwrap();

    // Seed the removals BEFORE confirming the chain: grouped by their real (height, timestamp) so
    // birth heights are exact for the time-lock context. Coins created at 5,000,000 arrive with
    // that block's own delta below instead.
    let (_, rems) = common::load_adds_rems(H);
    let mut groups: HashMap<(u32, u64), Vec<CoinRecord>> = HashMap::new();
    for r in rems {
        if r.confirmed_block_index >= 5_000_000 {
            continue;
        }
        let name = r.coin.name();
        if withhold == Some(name) {
            continue;
        }
        let spent = pre_spend == Some(name);
        groups
            .entry((r.confirmed_block_index, r.timestamp))
            .or_default()
            .push(CoinRecord {
                spent_block_index: 0,
                spent: false,
                ..r
            });
        if spent {
            // Mark it spent at a height at or below the fork point (any ancestor height works).
            let last = groups
                .get_mut(&(r.confirmed_block_index, r.timestamp))
                .unwrap()
                .last_mut()
                .unwrap();
            last.spent = true;
            last.spent_block_index = 4_999_990;
        }
    }
    for ((height, ts), adds) in groups {
        let spent_names: Vec<Bytes32> = adds
            .iter()
            .filter(|c| c.spent)
            .map(|c| c.coin.name())
            .collect();
        store.apply_block(height, ts, &adds, &[]).await.unwrap();
        if !spent_names.is_empty() {
            // Flip the pre-spent coin: spent_index = 4,999,990 (an ancestor of the fork point).
            store
                .apply_block(4_999_990, ts, &[], &spent_names)
                .await
                .unwrap();
        }
    }
    store
}

async fn engine_at_5000003(store: SqliteStore) -> Engine<SqliteStore, NativePrimitives> {
    let records = common::load_records();
    let (adds0, _) = common::load_adds_rems(5_000_000);
    let mut engine = Engine::new(store, NativePrimitives, MAINNET);
    let outcome = engine
        .add_delta(delta_for(&records[0], adds0))
        .await
        .unwrap();
    assert_eq!(outcome, AddBlockOutcome::NewPeak { height: 5_000_000 });
    for r in &records[1..4] {
        let outcome = engine.add_delta(delta_for(r, Vec::new())).await.unwrap();
        assert_eq!(outcome, AddBlockOutcome::Extended { height: r.height });
    }
    engine
}

// The honest mainnet block, with its removals present and unspent and its reward-claim walk
// grounded, passes FULL body validation and extends the peak. No rule may fire on real data.
#[tokio::test]
async fn honest_mainnet_block_passes_full_coin_validation() {
    let store = seeded_store(None, None).await;
    let mut engine = engine_at_5000003(store).await.with_enforced_coin_rules();
    let block4 = common::load_full_block(H);
    let outcome = engine.add_block(&block4).await.expect("honest block");
    assert_eq!(outcome, AddBlockOutcome::Extended { height: H });

    // The block's removals are now marked spent at H.
    let (_, rems) = common::load_adds_rems(H);
    let name = rems[0].coin.name();
    let rec = engine
        .store()
        .get_coin_record(&name)
        .await
        .unwrap()
        .expect("removed coin present");
    assert!(rec.spent, "removal marked spent");
    assert_eq!(rec.spent_block_index, H);
}

// Check 26a rejects a transaction block whose timestamp is more than MAX_FUTURE_TIME2 (120 s)
// beyond wall-clock now (TIMESTAMP_TOO_FAR_IN_FUTURE). Without it a block claiming an arbitrary
// far-future timestamp is accepted. The same seeded engine that accepts the honest block4 above
// must REJECT it once its timestamp is pushed far into the future. The guard sits before the
// body/record work, so it wins over the foliage-hash checks the mutated timestamp would otherwise
// trip — the rejection is specifically the future-timestamp code.
#[tokio::test]
async fn future_dated_transaction_block_is_rejected() {
    let store = seeded_store(None, None).await;
    let mut engine = engine_at_5000003(store).await.with_enforced_coin_rules();
    let mut block4 = common::load_full_block(H);
    let far_future = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
        + 100_000;
    block4
        .foliage_transaction_block
        .as_mut()
        .expect("block 5,000,004 is a transaction block")
        .timestamp = far_future;
    let err = engine
        .add_block(&block4)
        .await
        .expect_err("a far-future-dated block must be rejected");
    assert!(
        err.to_string().contains("TIMESTAMP_TOO_FAR_IN_FUTURE"),
        "expected TIMESTAMP_TOO_FAR_IN_FUTURE, got {err:?}"
    );
}

// A timestamp only 60 s ahead is INSIDE the 120 s window, so the future-timestamp guard must NOT
// fire. The ftb timestamp is not separately bound on this path, so the assertion is only that
// whatever the outcome, it is not a future-timestamp rejection: the guard must not over-reject
// within the allowed skew.
#[tokio::test]
async fn timestamp_within_future_window_is_not_rejected_as_future() {
    let store = seeded_store(None, None).await;
    let mut engine = engine_at_5000003(store).await.with_enforced_coin_rules();
    let mut block4 = common::load_full_block(H);
    let near = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
        + 60;
    block4
        .foliage_transaction_block
        .as_mut()
        .expect("tx block")
        .timestamp = near;
    if let Err(err) = engine.add_block(&block4).await {
        assert!(
            !err.to_string().contains("TIMESTAMP_TOO_FAR_IN_FUTURE"),
            "a timestamp inside the 120s window must not be rejected as future, got {err:?}"
        );
    }
}

/// The first removal created strictly below the confirmed base — a coin whose row is entirely
/// under the test's control (never touched by block 5,000,000's own delta).
fn probe_removal() -> Bytes32 {
    let (_, rems) = common::load_adds_rems(H);
    rems.iter()
        .find(|r| r.confirmed_block_index < 5_000_000)
        .expect("a removal below the base")
        .coin
        .name()
}

// Rule 15, DOUBLE_SPEND: one of the block's removals is already spent at an ancestor height
// (at or below the fork point). The engine must consult the coin store on the live path.
#[tokio::test]
async fn spending_an_already_spent_coin_is_rejected() {
    let name = probe_removal();
    let store = seeded_store(None, Some(name)).await;
    let mut engine = engine_at_5000003(store).await.with_enforced_coin_rules();
    let block4 = common::load_full_block(H);
    let err = engine
        .add_block(&block4)
        .await
        .expect_err("double spend must reject");
    assert_eq!(consensus_err(err), ChiaError::DoubleSpend);
}

// Rule 15, UNKNOWN_UNSPENT: one of the block's removals does not exist anywhere (store, fork,
// or this block).
#[tokio::test]
async fn spending_a_nonexistent_coin_is_rejected() {
    let name = probe_removal();
    let store = seeded_store(Some(name), None).await;
    let mut engine = engine_at_5000003(store).await.with_enforced_coin_rules();
    let block4 = common::load_full_block(H);
    let err = engine
        .add_block(&block4)
        .await
        .expect_err("unknown coin must reject");
    assert_eq!(consensus_err(err), ChiaError::UnknownUnspent);
}

/// Rebind the foliage transaction block to a (tampered) transactions_info, so the body rules —
/// not the rule-3 hash binding — are what judge the tamper.
fn rebind_foliage(block: &mut FullBlock) {
    let ti_hash = transactions_info_hash(block.transactions_info.as_ref().unwrap()).unwrap();
    let ftb = block.foliage_transaction_block.as_mut().unwrap();
    ftb.transactions_info_hash = ti_hash;
    let ftb_hash = block
        .foliage_transaction_block
        .as_ref()
        .unwrap()
        .hash()
        .unwrap();
    block.foliage.foliage_transaction_block_hash = Some(ftb_hash);
}

// Rule 5, INVALID_REWARD_COINS: a tampered reward claim (one mojo added to a claim coin).
// `ti.reward_claims_incorporated` must never be trusted wholesale.
#[tokio::test]
async fn tampered_reward_claims_are_rejected() {
    let store = seeded_store(None, None).await;
    let mut engine = engine_at_5000003(store).await.with_enforced_coin_rules();
    let mut block4 = common::load_full_block(H);
    block4
        .transactions_info
        .as_mut()
        .unwrap()
        .reward_claims_incorporated[0]
        .amount += 1;
    rebind_foliage(&mut block4);
    let err = engine
        .add_block(&block4)
        .await
        .expect_err("tampered claims must reject");
    assert_eq!(consensus_err(err), ChiaError::InvalidRewardCoins);
}

// Rule 19, INVALID_BLOCK_FEE_AMOUNT: the declared fee differs from the computed
// removals-minus-additions, which must be recomputed on the live path.
#[tokio::test]
async fn tampered_fee_amount_is_rejected() {
    let store = seeded_store(None, None).await;
    let mut engine = engine_at_5000003(store).await.with_enforced_coin_rules();
    let mut block4 = common::load_full_block(H);
    block4.transactions_info.as_mut().unwrap().fees += 1;
    rebind_foliage(&mut block4);
    let err = engine
        .add_block(&block4)
        .await
        .expect_err("wrong fee must reject");
    assert_eq!(consensus_err(err), ChiaError::InvalidBlockFeeAmount);
}

// Rule 11, BAD_ADDITION_ROOT / BAD_REMOVAL_ROOT: the foliage merkle roots must commit to the
// actual coin delta, recomputed on the live path.
#[tokio::test]
async fn tampered_addition_and_removal_roots_are_rejected() {
    let store = seeded_store(None, None).await;
    let mut engine = engine_at_5000003(store).await.with_enforced_coin_rules();

    let mut bad_add = common::load_full_block(H);
    bad_add
        .foliage_transaction_block
        .as_mut()
        .unwrap()
        .additions_root = Bytes32::new([0x5a; 32]);
    let err = engine
        .add_block(&bad_add)
        .await
        .expect_err("bad additions root must reject");
    assert_eq!(consensus_err(err), ChiaError::BadAdditionRoot);

    let mut bad_rem = common::load_full_block(H);
    bad_rem
        .foliage_transaction_block
        .as_mut()
        .unwrap()
        .removals_root = Bytes32::new([0x5b; 32]);
    let err = engine
        .add_block(&bad_rem)
        .await
        .expect_err("bad removals root must reject");
    assert_eq!(consensus_err(err), ChiaError::BadRemovalRoot);
}

// RED — rule 12, INVALID_TRANSACTIONS_FILTER_HASH: the foliage BIP158 filter must commit
// to the actual additions/removals, recomputed on the live path.
#[tokio::test]
async fn tampered_filter_hash_is_rejected() {
    let store = seeded_store(None, None).await;
    let mut engine = engine_at_5000003(store).await.with_enforced_coin_rules();
    let mut block4 = common::load_full_block(H);
    block4
        .foliage_transaction_block
        .as_mut()
        .unwrap()
        .filter_hash = Bytes32::new([0x5d; 32]);
    let err = engine
        .add_block(&block4)
        .await
        .expect_err("bad transactions filter must reject");
    assert_eq!(consensus_err(err), ChiaError::InvalidTransactionsFilterHash);
}

// Rule 3, INVALID_TRANSACTIONS_INFO_HASH: the foliage transaction block must bind the
// transactions_info by hash, checked on the live path.
#[tokio::test]
async fn tampered_transactions_info_hash_is_rejected() {
    let store = seeded_store(None, None).await;
    let mut engine = engine_at_5000003(store).await.with_enforced_coin_rules();
    let mut block4 = common::load_full_block(H);
    block4
        .foliage_transaction_block
        .as_mut()
        .unwrap()
        .transactions_info_hash = Bytes32::new([0x5c; 32]);
    let err = engine
        .add_block(&block4)
        .await
        .expect_err("unbound transactions_info must reject");
    assert_eq!(consensus_err(err), ChiaError::InvalidTransactionsInfoHash);
}

// ---------------------------------------------------------------------------
// Synthetic post-hard-fork vehicle: a body-consistent transaction block whose simple generator
// (`(q . spends)`, the post-hard-fork shape) is built per test, over a chain of two synthetic
// transaction records confirmed via add_delta. Body validation runs BEFORE header validation in
// the engine, so body rejections are observable even though the synthetic headers carry no real
// proof of space; the `..._reaches_header_validation` control proves the honest-body variant
// clears every body rule.

const SYNTH_BASE: u32 = 6_000_100; // post-hard-fork (5,496,000), pre-soft-fork-9 (8,655,000)

fn tx_record(
    template: &BlockRecord,
    tag: u8,
    height: u32,
    weight: u128,
    prev: Bytes32,
    timestamp: u64,
    fees: u64,
) -> BlockRecord {
    let mut r = template.clone();
    r.header_hash = common::synth_hash(tag, height);
    r.prev_hash = prev;
    r.height = height;
    r.weight = weight;
    r.total_iters = weight;
    r.timestamp = Some(timestamp);
    r.fees = Some(fees);
    r.sub_epoch_summary_included = None;
    r
}

/// The exact reward claims a child transaction block must incorporate for `record`
/// (a transaction block directly on top of another transaction block).
fn claims_for(record: &BlockRecord) -> Vec<Coin> {
    vec![
        create_pool_coin(
            record.height,
            record.pool_puzzle_hash,
            calculate_pool_reward(record.height),
            MAINNET.genesis_challenge,
        ),
        create_farmer_coin(
            record.height,
            record.farmer_puzzle_hash,
            calculate_base_farmer_reward(record.height) + record.fees.unwrap_or(0),
            MAINNET.genesis_challenge,
        ),
    ]
}

fn quoted_generator(output: SExp<'static>) -> SerializedProgram {
    Program::to((1_u8, output)).serialized().unwrap()
}

fn puzzle_for_conditions(conditions: Vec<ConditionWithArgs>) -> Program<'static> {
    let condition_sexps = conditions
        .iter()
        .map(|condition| SExp::from(condition).to_owned())
        .collect::<Vec<_>>();
    Program::to((1_u8, SExp::from(condition_sexps)))
}

fn spend_output(parent: Bytes32, amount: u64, puzzle: &Program<'static>) -> SExp<'static> {
    SExp::from(vec![
        SExp::from(parent),
        puzzle.sexp().to_owned(),
        SExp::from(amount),
        SExp::default(),
    ])
}

/// A body-consistent synthetic transaction block: the generator is run through the same flag
/// ladder the engine uses, and ti/foliage are recomputed from the resulting conditions — only the
/// fields a test explicitly breaks afterwards diverge from a body-honest block.
fn synth_tx_block(
    prev_record: &BlockRecord,
    height: u32,
    weight: u128,
    spends: Vec<SExp<'static>>,
    claims: Vec<Coin>,
    fees: u64,
) -> FullBlock {
    let output = SExp::from(vec![SExp::from(spends)]);
    let generator = quoted_generator(output);
    let input = BlockGeneratorInput {
        transactions_generator: generator.clone(),
        generator_refs: Vec::new(),
        constants: MAINNET,
        height,
        flags: BlockGeneratorFlags::for_height(&MAINNET, height),
    };
    let conds = execute_block_generator_result(&input).expect("synthetic generator runs");
    let ti = TransactionsInfo {
        generator_root: transactions_generator_root(&generator),
        generator_refs_root: transactions_generator_refs_root(&[]).unwrap(),
        aggregated_signature: SpendBundle::empty().aggregated_signature,
        fees,
        cost: conds.cost,
        reward_claims_incorporated: claims,
    };
    // The BIP158 filter over every addition's puzzle hash + every removal id (rule 12).
    let all_additions = additions_for_conditions(&conds, &ti.reward_claims_incorporated);
    let all_removals = removals_for_conditions(&conds);
    let mut filter_items: Vec<Vec<u8>> = Vec::new();
    for coin in &all_additions {
        filter_items.push(coin.puzzle_hash.bytes().to_vec());
    }
    for name in &all_removals {
        filter_items.push(name.bytes().to_vec());
    }
    let ftb = FoliageTransactionBlock {
        prev_transaction_block_hash: prev_record.header_hash,
        timestamp: prev_record.timestamp.unwrap_or(0) + 10,
        filter_hash: Bytes32::new(hash_256(chia_block_filter(&filter_items))),
        additions_root: additions_root(&conds, &ti.reward_claims_incorporated).unwrap(),
        removals_root: removals_root(&conds),
        transactions_info_hash: transactions_info_hash(&ti).unwrap(),
    };
    let mut b = common::load_full_block(5_000_000);
    b.reward_chain_block.height = height;
    b.reward_chain_block.weight = weight;
    b.foliage.prev_block_hash = prev_record.header_hash;
    b.foliage.foliage_transaction_block_hash = Some(ftb.hash().unwrap());
    b.foliage_transaction_block = Some(ftb);
    b.transactions_info = Some(ti);
    b.transactions_generator = Some(generator);
    b.transactions_generator_ref_list = Vec::new();
    b
}

/// Two synthetic transaction records confirmed (m1 then m0), a coin `amount` mojo strong born at
/// m0, and a child block spending it with `conditions`; returns the add_block outcome.
async fn run_synth_spend(
    conditions: Vec<ConditionWithArgs>,
    amount: u64,
    fees: u64,
    constants: dg_xch_core::consensus::constants::ConsensusConstants,
) -> Result<AddBlockOutcome, NodeError> {
    let records = common::load_records();
    let template = &records[0];
    let m1 = tx_record(
        template,
        0xf1,
        SYNTH_BASE - 1,
        9_000,
        common::synth_hash(0xf1, SYNTH_BASE - 2),
        1_700_000_000,
        0,
    );
    let m0 = tx_record(
        template,
        0xf0,
        SYNTH_BASE,
        9_010,
        m1.header_hash,
        1_700_000_010,
        7,
    );
    let puzzle = puzzle_for_conditions(conditions);
    let coin = Coin {
        parent_coin_info: common::synth_hash(0xcc, 1),
        puzzle_hash: puzzle.tree_hash(),
        amount,
    };
    let store = common::new_store().await;
    let mut engine = Engine::new(store, NativePrimitives, constants).with_enforced_coin_rules();
    engine.add_delta(delta_for(&m1, Vec::new())).await.unwrap();
    let born = CoinRecord {
        coin,
        confirmed_block_index: m0.height,
        spent_block_index: 0,
        coinbase: false,
        timestamp: m0.timestamp.unwrap(),
        spent: false,
    };
    engine.add_delta(delta_for(&m0, vec![born])).await.unwrap();
    let block = synth_tx_block(
        &m0,
        SYNTH_BASE + 1,
        9_020,
        vec![spend_output(coin.parent_coin_info, coin.amount, &puzzle)],
        claims_for(&m0),
        fees,
    );
    engine.add_block(&block).await
}

// An honest-bodied synthetic spend clears EVERY body rule (reward claims, roots, fees, coin
// lookups, conditions) and is accepted — the control for the synthetic vehicle below.
#[tokio::test]
async fn honest_synthetic_spend_is_accepted() {
    let create = ConditionWithArgs::CreateCoin(Bytes32::new([0x77; 32]), 9_000, Vec::new());
    let outcome = run_synth_spend(vec![create], 10_000, 1_000, MAINNET)
        .await
        .expect("honest synthetic body");
    assert_eq!(
        outcome,
        AddBlockOutcome::Extended {
            height: SYNTH_BASE + 1
        }
    );
}

#[tokio::test]
async fn configured_reward_schedule_rejects_chia_reward_claims() {
    let mut constants = MAINNET;
    constants.rewards.initial_farmer += 1_000_000;
    let create = ConditionWithArgs::CreateCoin(Bytes32::new([0x77; 32]), 9_000, Vec::new());
    let error = run_synth_spend(vec![create], 10_000, 1_000, constants)
        .await
        .expect_err("claims from a different reward schedule must fail");
    assert_eq!(consensus_err(error), ChiaError::InvalidRewardCoins);
}

// Rule 16, MINTING_COIN: additions exceed removals. The engine must compare the two amounts.
#[tokio::test]
async fn minting_block_is_rejected() {
    let create = ConditionWithArgs::CreateCoin(Bytes32::new([0x77; 32]), 11_000, Vec::new());
    let err = run_synth_spend(vec![create], 10_000, 0, MAINNET)
        .await
        .expect_err("minting must reject");
    assert_eq!(consensus_err(err), ChiaError::MintingCoin);
}

// Rule 10, COIN_AMOUNT_EXCEEDS_MAXIMUM: a created coin above the consensus cap
// (exercised with a lowered cap; mainnet's is u64::MAX, unreachable by construction).
#[tokio::test]
async fn oversized_coin_amount_is_rejected() {
    let mut constants = MAINNET;
    constants.max_coin_amount = 2_000_000_000_000; // above the reward claims, below the creation
    let create =
        ConditionWithArgs::CreateCoin(Bytes32::new([0x77; 32]), 2_400_000_000_000, Vec::new());
    let err = run_synth_spend(vec![create], 2_500_000_000_000, 100_000_000_000, constants)
        .await
        .expect_err("oversized coin must reject");
    assert_eq!(consensus_err(err), ChiaError::CoinAmountExceedsMaximum);
}

// Rule-21 coin context (time-lock checks): ASSERT_MY_BIRTH_HEIGHT against the spent coin's
// actual birth height. An empty coin context silently skips the assert.
#[tokio::test]
async fn wrong_birth_height_assert_is_rejected() {
    let conditions = vec![
        ConditionWithArgs::AssertMyBirthHeight(1),
        ConditionWithArgs::CreateCoin(Bytes32::new([0x77; 32]), 9_000, Vec::new()),
    ];
    let err = run_synth_spend(conditions, 10_000, 1_000, MAINNET)
        .await
        .expect_err("wrong birth height must reject");
    assert_eq!(consensus_err(err), ChiaError::InvalidCondition);
}

// Rule 15 with ForkInfo semantics, DOUBLE_SPEND_IN_FORK: a reorg-candidate branch
// block spending a coin ALREADY SPENT EARLIER ON THE SAME BRANCH is rejected at arrival — the
// fork view must carry the branch's own removals, not just the main chain's, or the double spend
// is accepted into the branch and poisons any later reorg replay.
#[tokio::test]
async fn fork_branch_double_spend_is_rejected() {
    let records = common::load_records();
    let template = &records[0];
    let m1 = tx_record(
        template,
        0xf1,
        SYNTH_BASE - 1,
        9_000,
        common::synth_hash(0xf1, SYNTH_BASE - 2),
        1_700_000_000,
        0,
    );
    let m0 = tx_record(
        template,
        0xf0,
        SYNTH_BASE,
        9_010,
        m1.header_hash,
        1_700_000_010,
        7,
    );
    let puzzle = puzzle_for_conditions(vec![ConditionWithArgs::CreateCoin(
        Bytes32::new([0x77; 32]),
        9_000,
        Vec::new(),
    )]);
    let coin = Coin {
        parent_coin_info: common::synth_hash(0xcc, 1),
        puzzle_hash: puzzle.tree_hash(),
        amount: 10_000,
    };
    let store = common::new_store().await;
    let mut engine = Engine::new(store, NativePrimitives, MAINNET).with_enforced_coin_rules();
    engine.add_delta(delta_for(&m1, Vec::new())).await.unwrap();
    let born = CoinRecord {
        coin,
        confirmed_block_index: m0.height,
        spent_block_index: 0,
        coinbase: false,
        timestamp: m0.timestamp.unwrap(),
        spent: false,
    };
    engine.add_delta(delta_for(&m0, vec![born])).await.unwrap();

    // Main chain extends heavier, so the branch below parks as reorg candidates.
    let m2 = tx_record(
        template,
        0xf2,
        SYNTH_BASE + 1,
        9_100,
        m0.header_hash,
        1_700_000_020,
        0,
    );
    assert_eq!(
        engine.add_delta(delta_for(&m2, Vec::new())).await.unwrap(),
        AddBlockOutcome::Extended {
            height: SYNTH_BASE + 1
        }
    );

    // Branch block A1 (already-validated delta, the fork-view role) spends the coin.
    let a1 = tx_record(
        template,
        0xa1,
        SYNTH_BASE + 1,
        9_050,
        m0.header_hash,
        1_700_000_015,
        0,
    );
    let mut a1_delta = delta_for(&a1, Vec::new());
    a1_delta.removals = vec![coin.name()];
    assert_eq!(
        engine.add_delta(a1_delta).await.unwrap(),
        AddBlockOutcome::Orphan {
            height: SYNTH_BASE + 1
        }
    );

    // B2 extends the branch and spends the SAME coin again.
    let b2 = synth_tx_block(
        &a1,
        SYNTH_BASE + 2,
        9_060,
        vec![spend_output(coin.parent_coin_info, coin.amount, &puzzle)],
        claims_for(&a1),
        1_000,
    );
    let err = engine
        .add_block(&b2)
        .await
        .expect_err("fork double spend must reject");
    assert_eq!(consensus_err(err), ChiaError::DoubleSpendInFork);
}
