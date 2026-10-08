use dg_xch_core::blockchain::block_record::BlockRecord;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_stores::{BlockStatus, BlockStore};
use std::time::Duration;

fn header_at(records: &[BlockRecord], height: u32) -> Bytes32 {
    records
        .iter()
        .find(|r| r.height == height)
        .map(|r| r.header_hash)
        .expect("height present in fixture")
}

#[tokio::test]
async fn out_of_order_body_append_then_in_order_confirm() {
    let records = common::load_records();
    let store = common::new_store().await;
    store.add_block_records(&records).await.unwrap();

    // Bodies arrive out of height order in one batch (one commit = one fsync).
    let hi = common::load_full_block(5_000_004);
    let lo = common::load_full_block(5_000_000);
    let mut batch = store.begin().await.unwrap();
    store
        .append_many(&mut batch, &[hi.clone(), lo.clone()])
        .await
        .unwrap();
    store.commit(batch).await.unwrap();

    assert!(
        store
            .get_block(&lo.header_hash().unwrap())
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        store
            .get_block(&hi.header_hash().unwrap())
            .await
            .unwrap()
            .is_some()
    );

    let top = header_at(&records, 5_000_029);
    let links = store.set_peak(&top).await.unwrap();
    assert!(links > 0);
    assert_eq!(store.get_peak().await.unwrap().unwrap(), (top, 5_000_029));
}

#[tokio::test]
async fn get_unassociated_returns_next_candidate_heights_lacking_a_body() {
    let records = common::load_records();
    let store = common::new_store().await;
    store.add_block_records(&records).await.unwrap();

    assert_eq!(
        store.get_unassociated(3).await.unwrap(),
        vec![5_000_000, 5_000_001, 5_000_002]
    );

    let lo = common::load_full_block(5_000_000);
    let mut batch = store.begin().await.unwrap();
    store.append_many(&mut batch, &[lo]).await.unwrap();
    store.commit(batch).await.unwrap();

    assert_eq!(
        store.get_unassociated(3).await.unwrap(),
        vec![5_000_001, 5_000_002, 5_000_003]
    );
}

#[tokio::test]
async fn savepoint_reorg_rolls_back_with_peak_unchanged() {
    let records = common::load_records();
    let store = common::new_store().await;
    store.add_block_records(&records).await.unwrap();

    store
        .set_peak(&header_at(&records, 5_000_010))
        .await
        .unwrap();
    let sp = store.savepoint().await.unwrap();
    let peak_before = store.get_peak().await.unwrap();

    store
        .set_peak(&header_at(&records, 5_000_015))
        .await
        .unwrap();
    assert_eq!(store.get_peak().await.unwrap().unwrap().1, 5_000_015);

    let reverted = store.rollback(sp).await.unwrap();
    assert_eq!(
        reverted, 5,
        "rollback returns a count, not a changed-coin dict"
    );
    assert_eq!(
        store.get_peak().await.unwrap(),
        peak_before,
        "peak unchanged after rollback"
    );
}

#[tokio::test]
async fn per_block_status_is_durable() {
    let records = common::load_records();
    let store = common::new_store().await;
    store.add_block_records(&records).await.unwrap();
    let hh = header_at(&records, 5_000_007);

    assert_eq!(
        store.get_status(&hh).await.unwrap(),
        BlockStatus::Unvalidated
    );
    store.set_status(&hh, BlockStatus::Validated).await.unwrap();
    assert_eq!(store.get_status(&hh).await.unwrap(), BlockStatus::Validated);
    store.set_status(&hh, BlockStatus::Bypass).await.unwrap();
    assert_eq!(store.get_status(&hh).await.unwrap(), BlockStatus::Bypass);
}

#[tokio::test]
async fn point_reads_are_lock_free_during_an_open_write_batch() {
    let records = common::load_records();
    let store = common::new_store().await;
    store.add_block_records(&records).await.unwrap();
    let committed = header_at(&records, 5_000_003);

    // Open a batch (holds the single writer) and append an uncommitted body.
    let mut batch = store.begin().await.unwrap();
    let fb = common::load_full_block(5_000_000);
    store.append_many(&mut batch, &[fb]).await.unwrap();

    // A point read of already-committed data must not block on the open write batch.
    let read = tokio::time::timeout(Duration::from_secs(5), store.get_block_record(&committed))
        .await
        .expect("read did not block on the open batch")
        .unwrap();
    assert!(read.is_some());

    store.commit(batch).await.unwrap();
}

#[tokio::test]
async fn one_batch_carries_the_whole_per_block_apply() {
    use dg_xch_stores::CoinStore;
    let records = common::load_records();
    let store = common::new_store().await;
    store.add_block_records(&records[1..]).await.unwrap();

    // The engine's per-block shape: record, body, status, coin deltas, and the peak flip all ride
    // ONE open batch and become visible together on its single commit.
    let block = common::load_full_block(5_000_000);
    let hh = block.header_hash().unwrap();
    let (adds, _rems) = common::load_adds_rems(5_000_000);
    let mut batch = store.begin().await.unwrap();
    store
        .add_block_records_in(&mut batch, &records[..1])
        .await
        .unwrap();
    store
        .append_many(&mut batch, std::slice::from_ref(&block))
        .await
        .unwrap();
    store
        .set_status_in(&mut batch, &hh, BlockStatus::Validated)
        .await
        .unwrap();
    store
        .apply_block_in(&mut batch, 5_000_000, 1_700_000_000, &adds, &[])
        .await
        .unwrap();
    let links = store.set_peak_in(&mut batch, &hh).await.unwrap();
    store.commit(batch).await.unwrap();

    assert!(links > 0);
    assert_eq!(
        store.get_peak().await.unwrap().unwrap(),
        (hh, 5_000_000),
        "peak flip committed with the batch"
    );
    assert_eq!(store.get_status(&hh).await.unwrap(), BlockStatus::Validated);
    assert!(store.get_block(&hh).await.unwrap().is_some());
    let name = adds[0].coin.name();
    assert!(
        store.get_coin_record(&name).await.unwrap().is_some(),
        "coin deltas committed with the batch"
    );
}
