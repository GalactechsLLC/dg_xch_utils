use crate::types::blockchain::block_record::BlockRecord;
use crate::types::blockchain::blockchain_state::BlockchainState;
use crate::types::blockchain::coin_record::CoinRecord;
use crate::types::blockchain::coin_spend::CoinSpend;
use crate::types::blockchain::full_block::FullBlock;
use crate::types::blockchain::mem_pool_item::MemPoolItem;
use crate::types::blockchain::network_info::NetworkInfo;
use crate::types::blockchain::signage_point_or_eos::SignagePointOrEOS;
use crate::types::blockchain::sized_bytes::Bytes32;
use crate::types::blockchain::spend_bundle::SpendBundle;
use crate::types::blockchain::tx_status::TXStatus;
use crate::types::blockchain::unfinished_block::UnfinishedBlock;
use async_trait::async_trait;
use std::collections::HashMap;
use std::io::Error;

#[async_trait]
pub trait FullnodeAPI {
    async fn get_blockchain_state(&self) -> Result<BlockchainState, Error>;
    async fn get_block(&self, header_hash: &Bytes32) -> Result<FullBlock, Error>;
    async fn get_blocks(
        &self,
        start: u32,
        end: u32,
        exclude_header_hash: bool,
    ) -> Result<Vec<FullBlock>, Error>;
    async fn get_all_blocks(&self, start: u32, end: u32) -> Result<Vec<FullBlock>, Error>;
    async fn get_block_record_by_height(&self, height: u32) -> Result<BlockRecord, Error>;
    async fn get_block_record(&self, header_hash: &Bytes32) -> Result<BlockRecord, Error>;
    async fn get_block_records(&self, start: u32, end: u32) -> Result<Vec<BlockRecord>, Error>;
    async fn get_unfinished_block_headers(&self) -> Result<Vec<UnfinishedBlock>, Error>;
    async fn get_network_space(
        &self,
        older_block_header_hash: &Bytes32,
        newer_block_header_hash: &Bytes32,
    ) -> Result<u64, Error>;
    async fn get_network_space_by_height(
        &self,
        older_block_height: u32,
        newer_block_height: u32,
    ) -> Result<u64, Error>;
    async fn get_additions_and_removals(
        &self,
        header_hash: &Bytes32,
    ) -> Result<(Vec<CoinRecord>, Vec<CoinRecord>), Error>;
    async fn get_initial_freeze_period(&self) -> Result<u64, Error>;
    async fn get_network_info(&self) -> Result<NetworkInfo, Error>;
    async fn get_recent_signage_point_or_eos(
        &self,
        sp_hash: Option<&Bytes32>,
        challenge_hash: Option<&Bytes32>,
    ) -> Result<SignagePointOrEOS, Error>;
    async fn get_coin_records_by_puzzle_hash(
        &self,
        puzzle_hash: &Bytes32,
        include_spent_coins: bool,
        start_height: u32,
        end_height: u32,
    ) -> Result<Vec<CoinRecord>, Error>;
    async fn get_coin_records_by_puzzle_hashes(
        &self,
        puzzle_hashes: Vec<&Bytes32>,
        include_spent_coins: bool,
        start_height: u32,
        end_height: u32,
    ) -> Result<Vec<CoinRecord>, Error>;
    async fn get_coin_record_by_name(&self, name: &Bytes32) -> Result<Option<CoinRecord>, Error>;
    async fn get_coin_records_by_parent_ids(
        &self,
        parent_ids: Vec<&Bytes32>,
        include_spent_coins: bool,
        start_height: u32,
        end_height: u32,
    ) -> Result<Vec<CoinRecord>, Error>;
    async fn push_tx(&self, spend_bundle: &SpendBundle) -> Result<TXStatus, Error>;
    async fn get_puzzle_and_solution(
        &self,
        coin_id: &Bytes32,
        height: u32,
    ) -> Result<CoinSpend, Error>;
    async fn get_coin_spend(&self, coin_record: &CoinRecord) -> Result<CoinSpend, Error>;
    async fn get_all_mempool_tx_ids(&self) -> Result<Vec<String>, Error>;
    async fn get_all_mempool_items(&self) -> Result<HashMap<String, MemPoolItem>, Error>;
    async fn get_mempool_item_by_tx_id(&self, tx_id: &str) -> Result<MemPoolItem, Error>;
}
