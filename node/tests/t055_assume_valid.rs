mod common;

use dg_xch_core::blockchain::sized_bytes::Bytes96;
use dg_xch_core::consensus::constants::MAINNET;
use dg_xch_node::{AddBlockOutcome, Chaser, Engine, NativePrimitives, SyncConfig};
use dg_xch_stores::BlockStore;
use dg_xch_stores::types::BlockStatus;

fn block_with_bad_signature() -> dg_xch_core::blockchain::full_block::FullBlock {
    let mut block = common::load_full_block(5_000_000);
    block
        .transactions_info
        .as_mut()
        .expect("tx block")
        .aggregated_signature = Bytes96::from([0xab_u8; 96]);
    let ti_hash = dg_xch_core::consensus::block_generator::transactions_info_hash(
        block.transactions_info.as_ref().expect("tx block"),
    )
    .expect("ti hash");
    let ftb = block
        .foliage_transaction_block
        .as_mut()
        .expect("foliage tx block");
    ftb.transactions_info_hash = ti_hash;
    let ftb_hash = block
        .foliage_transaction_block
        .as_ref()
        .expect("foliage tx block")
        .hash()
        .expect("ftb hash");
    block.foliage.foliage_transaction_block_hash = Some(ftb_hash);
    block
}

// Default milestone = 0 validates everything — the corrupted signature is rejected.
#[tokio::test]
async fn default_milestone_validates_signatures() {
    let block = block_with_bad_signature();
    let store = common::new_store().await;
    common::ancestry::seed_mainnet_parent(&store).await;
    let mut engine = Engine::new(store, NativePrimitives, MAINNET);
    assert_eq!(engine.assume_valid(), 0, "fresh genesis default is off");
    let result = engine.add_block(&block).await;
    assert!(
        matches!(
            result,
            Err(dg_xch_node::NodeError::Consensus(
                dg_xch_core::errors::ChiaError::BadAggregateSignature
            ))
        ),
        "expected the signature check to reject the block, got {result:?}"
    );
}

// Below a set milestone, script/sig validation is bypassed — the same corrupted-signature block is
// confirmed (coins still derived from the generator), and its durable status records the bypass.
#[tokio::test]
async fn below_milestone_bypasses_signature_but_still_confirms() {
    let block = block_with_bad_signature();
    let hh = block.header_hash().unwrap();
    let store = common::new_store().await;
    common::ancestry::seed_mainnet_parent(&store).await;
    let mut engine = Engine::new(store, NativePrimitives, MAINNET).with_assume_valid(5_000_001);

    let outcome = engine
        .add_block(&block)
        .await
        .expect("assume-valid confirms below the milestone despite the bad signature");
    assert_eq!(outcome, AddBlockOutcome::NewPeak { height: 5_000_000 });

    // Coins were still applied (derived from the generator, not the skipped signature).
    let (ref_adds, _) = common::load_adds_rems(5_000_000);
    let gen_names: Vec<_> = ref_adds
        .iter()
        .filter(|c| !c.coinbase)
        .map(|c| c.coin.name())
        .collect();
    let got = dg_xch_stores::CoinStore::get_coin_records(engine.store(), &gen_names)
        .await
        .unwrap();
    assert_eq!(
        got.len(),
        gen_names.len(),
        "assume-valid still applies the block's coins"
    );

    // The durable per-block status records that this block was accepted under bypass, not full validation.
    let status = engine.store().get_status(&hh).await.unwrap();
    assert_eq!(
        status,
        BlockStatus::Bypass,
        "status is durably Bypass below the milestone"
    );
}

// The milestone threads from SyncConfig through the chaser into the engine (the seam is wired through
// the whole pipeline, not a one-off), and defaults to 0.
#[tokio::test]
async fn milestone_threads_from_sync_config_into_the_engine() {
    let store = common::new_store().await;
    let engine = Engine::new(store, NativePrimitives, MAINNET);
    let chaser = Chaser::new(
        engine,
        SyncConfig {
            assume_valid: 5_000_001,
            ..SyncConfig::default()
        },
    );
    assert_eq!(chaser.engine().assume_valid(), 5_000_001);

    let store = common::new_store().await;
    let default_chaser = Chaser::new(
        Engine::new(store, NativePrimitives, MAINNET),
        SyncConfig::default(),
    );
    assert_eq!(default_chaser.engine().assume_valid(), 0, "default off");
}
