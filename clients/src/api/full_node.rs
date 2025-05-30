use crate::rpc::ChiaRpcError;
use async_trait::async_trait;
use dg_xch_core::blockchain::block_record::BlockRecord;
use dg_xch_core::blockchain::blockchain_state::BlockchainState;
use dg_xch_core::blockchain::coin_record::{CoinRecord, HintedCoinRecord, PaginatedCoinRecord};
use dg_xch_core::blockchain::coin_spend::CoinSpend;
use dg_xch_core::blockchain::full_block::FullBlock;
use dg_xch_core::blockchain::mempool_item::MempoolItem;
use dg_xch_core::blockchain::network_info::NetworkInfo;
use dg_xch_core::blockchain::signage_point_or_eos::SignagePointOrEOS;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::blockchain::spend_bundle::SpendBundle;
use dg_xch_core::blockchain::tx_status::TXStatus;
use dg_xch_core::blockchain::unfinished_header_block::UnfinishedHeaderBlock;
use dg_xch_core::protocols::full_node::BlockCountMetrics;
use dg_xch_core::protocols::full_node::FeeEstimate;
use std::collections::HashMap;

#[async_trait]
pub trait FullnodeAPI {
    async fn get_blockchain_state(&self) -> Result<BlockchainState, ChiaRpcError>;
    async fn get_block(&self, header_hash: &Bytes32) -> Result<FullBlock, ChiaRpcError>;
    async fn get_blocks(
        &self,
        start: u32,
        end: u32,
        exclude_header_hash: bool,
        exclude_reorged: bool,
    ) -> Result<Vec<FullBlock>, ChiaRpcError>;
    async fn get_all_blocks(&self, start: u32, end: u32) -> Result<Vec<FullBlock>, ChiaRpcError>;
    async fn get_block_count_metrics(&self) -> Result<BlockCountMetrics, ChiaRpcError>;
    async fn get_block_record_by_height(&self, height: u32) -> Result<BlockRecord, ChiaRpcError>;
    async fn get_block_record(&self, header_hash: &Bytes32) -> Result<BlockRecord, ChiaRpcError>;
    async fn get_block_records(
        &self,
        start: u32,
        end: u32,
    ) -> Result<Vec<BlockRecord>, ChiaRpcError>;
    async fn get_unfinished_block_headers(
        &self,
    ) -> Result<Vec<UnfinishedHeaderBlock>, ChiaRpcError>;
    async fn get_network_space(
        &self,
        older_block_header_hash: &Bytes32,
        newer_block_header_hash: &Bytes32,
    ) -> Result<u64, ChiaRpcError>;
    async fn get_network_space_by_height(
        &self,
        older_block_height: u32,
        newer_block_height: u32,
    ) -> Result<u64, ChiaRpcError>;
    async fn get_additions_and_removals(
        &self,
        header_hash: &Bytes32,
    ) -> Result<(Vec<CoinRecord>, Vec<CoinRecord>), ChiaRpcError>;
    async fn get_initial_freeze_period(&self) -> Result<u64, ChiaRpcError>;
    async fn get_network_info(&self) -> Result<NetworkInfo, ChiaRpcError>;
    async fn get_recent_signage_point_or_eos(
        &self,
        sp_hash: Option<&Bytes32>,
        challenge_hash: Option<&Bytes32>,
    ) -> Result<SignagePointOrEOS, ChiaRpcError>;
    async fn get_coin_records_by_puzzle_hash(
        &self,
        puzzle_hash: &Bytes32,
        include_spent_coins: Option<bool>,
        start_height: Option<u32>,
        end_height: Option<u32>,
    ) -> Result<Vec<CoinRecord>, ChiaRpcError>;
    async fn get_coin_records_by_puzzle_hashes(
        &self,
        puzzle_hashes: &[Bytes32],
        include_spent_coins: Option<bool>,
        start_height: Option<u32>,
        end_height: Option<u32>,
    ) -> Result<Vec<CoinRecord>, ChiaRpcError>;
    async fn get_coin_record_by_name(
        &self,
        name: &Bytes32,
    ) -> Result<Option<CoinRecord>, ChiaRpcError>;
    async fn get_coin_records_by_names(
        &self,
        names: &[Bytes32],
        include_spent_coins: Option<bool>,
        start_height: Option<u32>,
        end_height: Option<u32>,
    ) -> Result<Vec<CoinRecord>, ChiaRpcError>;
    async fn get_coin_records_by_parent_ids(
        &self,
        parent_ids: &[Bytes32],
        include_spent_coins: Option<bool>,
        start_height: Option<u32>,
        end_height: Option<u32>,
    ) -> Result<Vec<CoinRecord>, ChiaRpcError>;
    async fn get_coin_records_by_hint(
        &self,
        hint: &Bytes32,
        include_spent_coins: Option<bool>,
        start_height: Option<u32>,
        end_height: Option<u32>,
    ) -> Result<Vec<CoinRecord>, ChiaRpcError>;
    async fn push_tx(&self, spend_bundle: &SpendBundle) -> Result<TXStatus, ChiaRpcError>;
    async fn get_puzzle_and_solution(
        &self,
        coin_id: &Bytes32,
        height: u32,
    ) -> Result<CoinSpend, ChiaRpcError>;
    async fn get_coin_spend(&self, coin_record: &CoinRecord) -> Result<CoinSpend, ChiaRpcError>;
    async fn get_all_mempool_tx_ids(&self) -> Result<Vec<Bytes32>, ChiaRpcError>;
    async fn get_all_mempool_items(&self) -> Result<HashMap<Bytes32, MempoolItem>, ChiaRpcError>;
    async fn get_mempool_item_by_tx_id(&self, tx_id: &str) -> Result<MempoolItem, ChiaRpcError>;
    async fn get_mempool_items_by_coin_name(
        &self,
        coin_name: &Bytes32,
    ) -> Result<Vec<MempoolItem>, ChiaRpcError>;
    async fn get_fee_estimate(
        &self,
        cost: Option<u64>,
        spend_bundle: Option<SpendBundle>,
        spend_type: Option<String>,
        target_times: &[u64],
    ) -> Result<FeeEstimate, ChiaRpcError>;
}

//Non Standard Endpoints supported by some modified nodes
#[async_trait]
pub trait FullnodeExtAPI {
    async fn get_additions_and_removals_with_hints(
        &self,
        header_hash: &Bytes32,
    ) -> Result<(Vec<HintedCoinRecord>, Vec<HintedCoinRecord>), ChiaRpcError>;
    async fn get_singleton_by_launcher_id(
        &self,
        launcher_id: &Bytes32,
    ) -> Result<(CoinRecord, CoinSpend), ChiaRpcError>;
    async fn get_coin_records_by_hints(
        &self,
        hints: &[Bytes32],
        include_spent_coins: Option<bool>,
        start_height: Option<u32>,
        end_height: Option<u32>,
    ) -> Result<Vec<CoinRecord>, ChiaRpcError>;
    async fn get_coin_records_by_hints_paginated(
        &self,
        hints: &[Bytes32],
        include_spent_coins: Option<bool>,
        start_height: Option<u32>,
        end_height: Option<u32>,
        page_size: u32,
        last_id: Option<Bytes32>,
    ) -> Result<(Vec<PaginatedCoinRecord>, Option<Bytes32>, Option<i32>), ChiaRpcError>;
    async fn get_coin_records_by_puzzle_hashes_paginated(
        &self,
        puzzle_hashes: &[Bytes32],
        include_spent_coins: Option<bool>,
        start_height: Option<u32>,
        end_height: Option<u32>,
        page_size: u32,
        last_id: Option<Bytes32>,
    ) -> Result<(Vec<PaginatedCoinRecord>, Option<Bytes32>, Option<i32>), ChiaRpcError>;
    async fn get_hints_by_coin_ids(
        &self,
        coin_ids: &[Bytes32],
    ) -> Result<HashMap<Bytes32, Bytes32>, ChiaRpcError>;
    async fn get_puzzles_and_solutions_by_names(
        &self,
        names: &[Bytes32],
        include_spent_coins: Option<bool>,
        start_height: Option<u32>,
        end_height: Option<u32>,
    ) -> Result<HashMap<Bytes32, Option<CoinSpend>>, ChiaRpcError>;
}
