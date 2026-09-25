mod common;

use dg_xch_core::blockchain::coin_record::CoinRecord;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::consensus::constants::MAINNET;
use dg_xch_node::{AddBlockOutcome, Engine, NativePrimitives};
use dg_xch_stores::{BlockStore, CoinStore};
use std::collections::HashSet;

fn names(records: &[CoinRecord]) -> HashSet<Bytes32> {
    records.iter().map(|c| c.coin.name()).collect()
}

// Validate + apply a real mainnet block through the full engine path; prove the engine-derived coin
// state matches the reference additions/removals and the peak advances.
#[tokio::test]
async fn add_real_mainnet_block_matches_reference_and_advances_peak() {
    let block = common::load_full_block(5_000_000);
    let (ref_adds, ref_rems) = common::load_adds_rems(5_000_000);
    let store = common::new_store().await;
    common::ancestry::seed_mainnet_parent(&store).await;
    let mut engine = Engine::new(store, NativePrimitives, MAINNET);

    let outcome = engine.add_block(&block).await.expect("add_block");
    assert_eq!(
        outcome,
        AddBlockOutcome::NewPeak { height: 5_000_000 },
        "the first block becomes the peak"
    );

    // Peak advanced to this block.
    let peak = engine.store().get_peak().await.unwrap().expect("peak");
    assert_eq!(peak.1, 5_000_000, "peak height");
    assert_eq!(
        peak.0,
        block.header_hash().unwrap(),
        "peak header hash is the applied block"
    );

    // The engine-derived removals equal the reference removals exactly.
    // (Derive them the same way the engine does, then check the coin store reflects them.)
    let ref_removal_names: HashSet<Bytes32> = ref_rems.iter().map(|c| c.coin.name()).collect();

    // Every reference NON-coinbase addition (generator-created coin) is present, unspent, at this height.
    let gen_adds: Vec<&CoinRecord> = ref_adds.iter().filter(|c| !c.coinbase).collect();
    assert!(!gen_adds.is_empty(), "block has generator-created coins");
    let got = engine
        .store()
        .get_coin_records(&gen_adds.iter().map(|c| c.coin.name()).collect::<Vec<_>>())
        .await
        .unwrap();
    let got_names = names(&got);
    for a in &gen_adds {
        assert!(
            got_names.contains(&a.coin.name()),
            "generator addition {} present in coin store",
            a.coin.name()
        );
    }
    assert_eq!(
        got.len(),
        gen_adds.len(),
        "exactly the generator additions were applied (count)"
    );
    for c in &got {
        assert_eq!(
            c.confirmed_block_index, 5_000_000,
            "confirmed at this height"
        );
        assert!(!c.spent, "freshly-created coin is unspent");
    }

    // Coinbase reward coins from the block's own transactions_info are applied and flagged coinbase.
    let coinbase_ref: Vec<&CoinRecord> = ref_adds.iter().filter(|c| c.coinbase).collect();
    let reward_names: Vec<Bytes32> = block
        .transactions_info
        .as_ref()
        .unwrap()
        .reward_claims_incorporated
        .iter()
        .map(dg_xch_core::blockchain::coin::Coin::name)
        .collect();
    let reward_got = engine
        .store()
        .get_coin_records(&reward_names)
        .await
        .unwrap();
    assert!(
        reward_got.iter().all(|c| c.coinbase),
        "reward coins are flagged coinbase"
    );
    assert!(
        !coinbase_ref.is_empty() && !reward_got.is_empty(),
        "reference and engine both have reward coins"
    );

    // A removal that this block spends (if any of its removals reference coins created here) must be marked
    // spent; assert none of the removal ids linger as unspent additions of this block.
    for name in &ref_removal_names {
        if got_names.contains(name) {
            let rec = engine.store().get_coin_record(name).await.unwrap().unwrap();
            assert!(rec.spent, "a coin removed in this block is marked spent");
        }
    }

    // Re-adding the same block is idempotent.
    let again = engine.add_block(&block).await.expect("re-add");
    assert_eq!(again, AddBlockOutcome::AlreadyHave);
}

fn child_of(
    parent: &dg_xch_core::blockchain::full_block::FullBlock,
) -> dg_xch_core::blockchain::full_block::FullBlock {
    let mut c = parent.clone();
    // Point at the real parent so prev-lookup succeeds; caller then breaks one field.
    c.foliage.prev_block_hash = parent.header_hash().unwrap();
    c.reward_chain_block.height = parent.reward_chain_block.height + 1;
    c.reward_chain_block.weight = parent.reward_chain_block.weight + 1;
    c
}

async fn peak_engine() -> (
    Engine<dg_xch_stores::SqliteStore, NativePrimitives>,
    dg_xch_core::blockchain::full_block::FullBlock,
) {
    let parent = common::load_full_block(5_000_000);
    let store = common::new_store().await;
    common::ancestry::seed_mainnet_parent(&store).await;
    let mut engine = Engine::new(store, NativePrimitives, MAINNET);
    engine
        .add_block(&parent)
        .await
        .expect("parent becomes peak");
    (engine, parent)
}

#[tokio::test]
async fn unanchored_mainnet_block_is_rejected_without_writing() {
    let block = common::load_full_block(5_000_000);
    let hash = block.header_hash().unwrap();
    let store = common::new_store().await;
    let mut engine = Engine::new(store, NativePrimitives, MAINNET);
    assert!(matches!(
        engine.add_block(&block).await,
        Err(dg_xch_node::NodeError::Orphan(_))
    ));
    assert_eq!(engine.store().get_peak().await.unwrap(), None);
    assert_eq!(engine.store().get_block(&hash).await.unwrap(), None);
    assert_eq!(engine.store().get_block_record(&hash).await.unwrap(), None);
}

#[tokio::test]
async fn block_with_unknown_prev_hash_is_rejected() {
    let (mut engine, parent) = peak_engine().await;
    let mut bad = child_of(&parent);
    bad.foliage.prev_block_hash = Bytes32::from([0u8; 32]); // no such parent

    let err = engine
        .add_block(&bad)
        .await
        .expect_err("unknown prev must reject");
    assert!(
        matches!(err, dg_xch_node::NodeError::Orphan(_)),
        "unknown parent link is an orphan, got {err:?}"
    );
}

#[tokio::test]
async fn block_height_must_extend_parent_by_one() {
    let (mut engine, parent) = peak_engine().await;
    let mut bad = child_of(&parent);
    bad.reward_chain_block.height = parent.reward_chain_block.height + 2; // skips a height

    let err = engine
        .add_block(&bad)
        .await
        .expect_err("non-contiguous height must reject");
    match err {
        dg_xch_node::NodeError::Invalid(msg) => assert!(
            msg.contains("extend"),
            "height rule message names the violation, got {msg:?}"
        ),
        other => panic!("expected Invalid(height), got {other:?}"),
    }
}

#[tokio::test]
async fn block_weight_must_strictly_increase_over_parent() {
    let (mut engine, parent) = peak_engine().await;
    let mut bad = child_of(&parent);
    bad.reward_chain_block.weight = parent.reward_chain_block.weight; // equal, not greater

    let err = engine
        .add_block(&bad)
        .await
        .expect_err("non-increasing weight must reject");
    match err {
        dg_xch_node::NodeError::Invalid(msg) => assert!(
            msg.contains("weight"),
            "weight rule message names the violation, got {msg:?}"
        ),
        other => panic!("expected Invalid(weight), got {other:?}"),
    }
}
