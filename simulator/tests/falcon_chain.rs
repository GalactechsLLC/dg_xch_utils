#![cfg(feature = "falcon")]
use dg_xch_core::{
    blockchain::sized_bytes::Bytes32, consensus::chain_definition::ChainDefinition,
    signatures::falcon_block::FalconBlock,
};
use dg_xch_node::{Engine, NativePrimitives, falcon::FalconPrimitives};
use dg_xch_simulator_lib::{
    chain::ChainBuilder, error::SimError, falcon::FalconFarming, pos2::PlotSet,
};
use dg_xch_stores::{SqliteStore, traits::BlockStore};
use std::sync::Arc;

#[tokio::test]
#[ignore = "creates native CPU plots and verifies native VDFs"]
async fn falcon_chain_uses_native_consensus_and_rejects_bad_witnesses() {
    let directory = tempfile::tempdir().unwrap();
    let definition = ChainDefinition::falcon_development("dg_xch/falcon/integration/v1".into());
    let constants = definition.constants().unwrap();
    let farming = FalconFarming::default();
    let keys = (0..12)
        .map(|_| farming.create_keys(constants.genesis_challenge).unwrap())
        .collect();
    let plots = PlotSet::setup_keys(
        &directory.path().join("plots"),
        keys,
        18,
        2,
        constants.is_testnet,
    )
    .unwrap();
    let store = Arc::new(
        SqliteStore::open(&directory.path().join("producer.sqlite"))
            .await
            .unwrap(),
    );
    let replica = Arc::new(
        SqliteStore::open(&directory.path().join("replica.sqlite"))
            .await
            .unwrap(),
    );
    let mut chain = ChainBuilder::new_with_primitives(
        store.clone(),
        FalconPrimitives::default(),
        constants,
        plots,
        Bytes32::const_new([0x37; 32]),
    );
    chain.set_authorization_factory(Arc::new(move |block| farming.primitives(block, &constants)));
    let target = std::env::var("DGX_FALCON_TEST_BLOCKS")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(3);
    for height in 0..target {
        let outcome = if height == 0 {
            chain.farm_genesis().await
        } else {
            match chain.farm_next_tx().await {
                Err(SimError::SubSlotExhausted) => chain.farm_next_slot().await,
                other => other,
            }
        };
        outcome.unwrap_or_else(|e| panic!("height {height}: {e}"));
        let context = chain.engine().primitives().block().unwrap().clone();
        assert_eq!(context.block.height(), height);
        let encoded = context.to_bytes().unwrap();
        let decoded = FalconBlock::from_bytes(&encoded, constants.genesis_challenge).unwrap();
        assert_eq!(decoded, context);
        let mut bad = context.clone();
        bad.header[0].signature[41] ^= 1;
        assert!(FalconPrimitives::new(bad, &constants).is_err());
        let mut missing = context.clone();
        missing.header.pop();
        assert!(FalconPrimitives::new(missing, &constants).is_err());
        assert!(FalconBlock::from_bytes(&encoded, Bytes32::default()).is_err());
        let mut old = Engine::new(replica.clone(), NativePrimitives, constants);
        assert!(old.add_block(&context.block).await.is_err());
        let primitives = FalconPrimitives::new(decoded, &constants).unwrap();
        let mut validator = Engine::new(replica.clone(), primitives, constants);
        validator.add_block(&context.block).await.unwrap();
        assert_eq!(
            store.get_peak().await.unwrap(),
            replica.get_peak().await.unwrap()
        );
        eprintln!(
            "Falcon native chain height={height} header={} header_signatures={} bytes={}",
            context.block.header_hash().unwrap(),
            context.header.len(),
            encoded.len()
        );
    }
}
