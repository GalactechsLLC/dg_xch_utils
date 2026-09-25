use super::*;
use crate::node::hardening::{Source, fixture, node};
use dg_xch_core::clvm::program::SerializedProgram;
use dg_xch_core::consensus::block_generator::{
    transactions_generator_root, transactions_info_hash,
};
use dg_xch_core::utils::hash_256;
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};

#[test]
fn seed_authentication_checks_every_hash_link() {
    let original = fixture();
    let height = original.height();
    assert!(verified_seed_generator(&original, height).is_some());
    let mut changed = original.clone();
    let generator = SerializedProgram::from(vec![0x01, 0x02, 0x03]);
    changed.transactions_generator = Some(generator.clone());
    assert!(verified_seed_generator(&changed, height).is_none());
    changed.transactions_info.as_mut().unwrap().generator_root =
        transactions_generator_root(&generator);
    assert_eq!(
        changed.header_hash().unwrap(),
        original.header_hash().unwrap()
    );
    assert!(verified_seed_generator(&changed, height).is_none());
    changed
        .foliage_transaction_block
        .as_mut()
        .unwrap()
        .transactions_info_hash =
        transactions_info_hash(changed.transactions_info.as_ref().unwrap()).unwrap();
    assert!(verified_seed_generator(&changed, height).is_none());
    changed.foliage.foliage_transaction_block_hash = Some(
        hash_256(
            changed
                .foliage_transaction_block
                .as_ref()
                .unwrap()
                .to_bytes(ChiaProtocolVersion::default())
                .unwrap(),
        )
        .into(),
    );
    assert!(verified_seed_generator(&changed, height).is_some());
    assert_ne!(
        changed.header_hash().unwrap(),
        original.header_hash().unwrap()
    );
    changed.reward_chain_block.height += 1;
    assert!(verified_seed_generator(&changed, height + 1).is_none());
}

#[tokio::test]
async fn unanchored_seed_requires_independent_matching_witness() {
    let (_directory, node) = node().await;
    let block = fixture();
    let height = block.height();
    let source: Arc<dyn BlockRangeSource> = Source::new(1, vec![block.clone()]);
    assert!(
        fetch_seed_refs(&node, &source, None, &[height])
            .await
            .is_empty()
    );
    assert!(
        fetch_seed_refs(&node, &source, Some(&source), &[height])
            .await
            .is_empty()
    );
    let missing: Arc<dyn BlockRangeSource> = Source::new(2, Vec::new());
    assert!(
        fetch_seed_refs(&node, &source, Some(&missing), &[height])
            .await
            .is_empty()
    );
    assert!(node.seed_ref_cache.lock().await.is_empty());
    let witness: Arc<dyn BlockRangeSource> = Source::new(2, vec![block]);
    assert_eq!(
        fetch_seed_refs(&node, &source, Some(&witness), &[height])
            .await
            .len(),
        1
    );
    assert_eq!(node.seed_ref_cache.lock().await.len(), 1);
    let queue = Arc::new(BlockQueue::new(
        height,
        1024 * 1024,
        node.sync_metrics.clone(),
    ));
    rebase_to_peak(&node, &queue).await;
    assert!(node.seed_ref_cache.lock().await.is_empty());
}

#[tokio::test]
async fn stored_record_authenticates_seed_and_overrides_witness() {
    let (_directory, node) = node().await;
    let block = fixture();
    let height = block.height();
    let records: Vec<BlockRecord> =
        serde_json::from_str(include_str!("../../fixtures/block_records.json")).unwrap();
    let record = records
        .into_iter()
        .find(|record| record.height == height)
        .unwrap();
    assert_eq!(record.header_hash, block.header_hash().unwrap());
    node.store.add_block_records(&[record]).await.unwrap();
    node.store
        .set_peak(&block.header_hash().unwrap())
        .await
        .unwrap();
    let source: Arc<dyn BlockRangeSource> = Source::new(1, vec![block.clone()]);
    assert_eq!(
        fetch_seed_refs(&node, &source, None, &[height]).await.len(),
        1
    );
    node.seed_ref_cache.lock().await.clear();
    let mut forged = block;
    forged.foliage.prev_block_hash = Bytes32::default();
    assert!(verified_seed_generator(&forged, height).is_some());
    let source: Arc<dyn BlockRangeSource> = Source::new(1, vec![forged.clone()]);
    let witness: Arc<dyn BlockRangeSource> = Source::new(2, vec![forged]);
    assert!(
        fetch_seed_refs(&node, &source, Some(&witness), &[height])
            .await
            .is_empty()
    );
    assert!(node.seed_ref_cache.lock().await.is_empty());
}
