use crate::{Chaser, Engine, NativePrimitives, SyncConfig};
use dg_xch_core::blockchain::full_block::FullBlock;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::consensus::constants::MAINNET;
use dg_xch_stores::BlockStore;
use std::sync::Arc;
use std::time::Duration;

const START: u32 = 100;
const N: u32 = 8;

fn build_chain(base: &FullBlock, start: u32, end: u32, parent: Bytes32) -> Vec<FullBlock> {
    let mut parent = parent;
    (start..=end)
        .map(|height| {
            let mut block = base.clone();
            block.reward_chain_block.height = height;
            block.reward_chain_block.weight = 1_000_000 + u128::from(height) * 10;
            block.foliage.prev_block_hash = parent;
            parent = block.header_hash().unwrap();
            block
        })
        .collect()
}

fn cfg() -> SyncConfig {
    SyncConfig {
        peers: 1,
        window: 32,
        batch: 32,
        request_timeout: Duration::from_secs(20),
        assume_valid: 10_000_000,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn split_confirmation_preserves_failure_prefix_and_restarts() {
    let base: FullBlock =
        serde_json::from_str(include_str!("../../../fixtures/full_block_5000000.json")).unwrap();
    let chain = build_chain(&base, START, START + N - 1, Bytes32::from([0xab; 32]));
    for boundary in [0, 3, 5, 7] {
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(
            dg_xch_stores::SqliteStore::open(&directory.path().join("chain.db"))
                .await
                .unwrap(),
        );
        let mut engine = Engine::new(store.clone(), NativePrimitives, MAINNET);
        engine.set_coalesce_coin_writes(true);
        let mut chaser = Chaser::new(engine, cfg());
        chaser.set_confirm_transaction_blocks(Some(3));
        let window = chaser.stage_window_pre(chain.clone(), None).await.unwrap();
        let verdict = crate::sync::WindowVerdict {
            confirm_upto: boundary,
            err: Some(crate::error::NodeError::Invalid("injected drain failure".into()).into()),
            vdf_micros: 0,
            sig_micros: 0,
        };
        assert!(chaser.confirm_window_pre(window, verdict).await.is_err());
        let expected = boundary
            .checked_sub(1)
            .map(|index| (chain[index].header_hash().unwrap(), chain[index].height()));
        assert_eq!(store.get_peak().await.unwrap(), expected);
        drop(chaser);
        let mut engine = Engine::new(store.clone(), NativePrimitives, MAINNET);
        engine.set_coalesce_coin_writes(true);
        let mut resumed = Chaser::new(engine, cfg());
        resumed.set_confirm_transaction_blocks(Some(3));
        let peak = resumed.follow_blocks(&chain[boundary..]).await.unwrap();
        assert_eq!(
            peak,
            Some((chain.last().unwrap().header_hash().unwrap(), START + N - 1))
        );
    }
}
