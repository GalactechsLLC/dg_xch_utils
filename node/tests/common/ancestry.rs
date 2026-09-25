#![allow(dead_code)]

use dg_xch_core::blockchain::block_record::BlockRecord;
use dg_xch_core::blockchain::full_block::FullBlock;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_stores::BlockStore;

pub async fn seed_mainnet_parent<S: BlockStore>(store: &S) {
    let record: BlockRecord =
        serde_json::from_str(include_str!("../fixtures/block_record_4999999.json"))
            .expect("parent record fixture");
    let block: FullBlock =
        serde_json::from_str(include_str!("../fixtures/full_block_4999999.json"))
            .expect("parent block fixture");
    assert_eq!(
        record.header_hash,
        block.header_hash().expect("parent hash")
    );
    assert_eq!(
        record.header_hash,
        serde_json::from_str::<FullBlock>(include_str!("../fixtures/full_block_5000000.json"))
            .expect("child fixture")
            .prev_header_hash()
    );
    store
        .add_block_records(&[record])
        .await
        .expect("parent record");
    let mut batch = store.begin().await.expect("begin parent body");
    store
        .append_many(&mut batch, &[block])
        .await
        .expect("parent body");
    store.commit(batch).await.expect("commit parent body");
    assert_eq!(store.get_peak().await.expect("peak"), None);
}

pub async fn seed_synthetic_parent<S: BlockStore>(store: &S, block: &FullBlock) {
    let mut parent: BlockRecord =
        serde_json::from_str(include_str!("../fixtures/block_record_4999999.json"))
            .expect("parent record fixture");
    parent.header_hash = block.prev_header_hash();
    parent.prev_hash = Bytes32::from([0xee; 32]);
    parent.height = block.height() - 1;
    parent.weight = block.reward_chain_block.weight - 1;
    parent.timestamp = None;
    parent.prev_transaction_block_height = block.height() - 2;
    parent.prev_transaction_block_hash = None;
    store
        .add_block_records(&[parent])
        .await
        .expect("synthetic parent");
}
