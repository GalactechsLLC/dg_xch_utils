use crate::clients::api::full_node::FullnodeAPI;
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
use reqwest::Client;
use serde_json::{json, Map};
use std::collections::HashMap;
use std::io::{Error, ErrorKind};

use crate::clients::api::responses::{
    AdditionsAndRemovalsResp, BlockRecordAryResp, BlockRecordResp, BlockchainStateResp,
    CoinRecordAryResp, CoinRecordResp, CoinSpendResp, FullBlockAryResp, FullBlockResp,
    InitialFreezePeriodResp, MempoolItemResp, MempoolItemsResp, MempoolTXResp, NetSpaceResp,
    NetworkInfoResp, SignagePointOrEOSResp, TXResp, UnfinishedBlockAryResp,
};
use crate::clients::rpc::{get_client, get_url, post};

pub struct FullnodeClient {
    client: Client,
    host: String,
    port: u16,
}

impl FullnodeClient {
    pub fn new(host: &str, port: u16, ssl_path: Option<String>) -> Self {
        FullnodeClient {
            client: get_client(ssl_path).unwrap_or_default(),
            host: host.to_string(),
            port,
        }
    }
}

#[async_trait]
impl FullnodeAPI for FullnodeClient {
    async fn get_blockchain_state(&self) -> Result<BlockchainState, Error> {
        Ok(post::<BlockchainStateResp>(
            &self.client,
            &get_url(self.host.as_str(), self.port, "get_blockchain_state"),
            &Map::new(),
        )
        .await?
        .blockchain_state)
    }
    async fn get_block(&self, header_hash: &Bytes32) -> Result<FullBlock, Error> {
        let mut request_body = Map::new();
        request_body.insert("header_hash".to_string(), json!(header_hash));
        Ok(post::<FullBlockResp>(
            &self.client,
            &get_url(self.host.as_str(), self.port, "get_block"),
            &request_body,
        )
        .await?
        .block)
    }
    async fn get_blocks(
        &self,
        start: u32,
        end: u32,
        exclude_header_hash: bool,
    ) -> Result<Vec<FullBlock>, Error> {
        let mut request_body = Map::new();
        request_body.insert("start".to_string(), json!(start));
        request_body.insert("end".to_string(), json!(end));
        request_body.insert(
            "exclude_header_hash".to_string(),
            json!(if exclude_header_hash { "True" } else { "False" }),
        );
        Ok(post::<FullBlockAryResp>(
            &self.client,
            &get_url(self.host.as_str(), self.port, "get_blocks"),
            &request_body,
        )
        .await?
        .blocks)
    }
    async fn get_all_blocks(&self, start: u32, end: u32) -> Result<Vec<FullBlock>, Error> {
        self.get_blocks(start, end, true).await
    }
    async fn get_block_record_by_height(&self, height: u32) -> Result<BlockRecord, Error> {
        let mut request_body = Map::new();
        request_body.insert("height".to_string(), json!(height));
        Ok(post::<BlockRecordResp>(
            &self.client,
            &get_url(self.host.as_str(), self.port, "get_block_record_by_height"),
            &request_body,
        )
        .await?
        .block_record)
    }
    async fn get_block_record(&self, header_hash: &Bytes32) -> Result<BlockRecord, Error> {
        let mut request_body = Map::new();
        request_body.insert("header_hash".to_string(), json!(header_hash));
        Ok(post::<BlockRecordResp>(
            &self.client,
            &get_url(self.host.as_str(), self.port, "get_block_record"),
            &request_body,
        )
        .await?
        .block_record)
    }
    async fn get_block_records(&self, start: u32, end: u32) -> Result<Vec<BlockRecord>, Error> {
        let mut request_body = Map::new();
        request_body.insert("start".to_string(), json!(start));
        request_body.insert("end".to_string(), json!(end));
        Ok(post::<BlockRecordAryResp>(
            &self.client,
            &get_url(self.host.as_str(), self.port, "get_block_records"),
            &request_body,
        )
        .await?
        .block_records)
    }
    async fn get_unfinished_block_headers(&self) -> Result<Vec<UnfinishedBlock>, Error> {
        Ok(post::<UnfinishedBlockAryResp>(
            &self.client,
            &get_url(
                self.host.as_str(),
                self.port,
                "get_unfinished_block_headers",
            ),
            &Map::new(),
        )
        .await?
        .headers)
    }
    async fn get_network_space(
        &self,
        older_block_header_hash: &Bytes32,
        newer_block_header_hash: &Bytes32,
    ) -> Result<u64, Error> {
        let mut request_body = Map::new();
        request_body.insert(
            "older_block_header_hash".to_string(),
            json!(older_block_header_hash),
        );
        request_body.insert(
            "newer_block_header_hash".to_string(),
            json!(newer_block_header_hash),
        );
        Ok(post::<NetSpaceResp>(
            &self.client,
            &get_url(self.host.as_str(), self.port, "get_network_space"),
            &request_body,
        )
        .await?
        .space)
    }
    async fn get_network_space_by_height(
        &self,
        older_block_height: u32,
        newer_block_height: u32,
    ) -> Result<u64, Error> {
        let older_block = self.get_block_record_by_height(older_block_height).await?;
        let newer_block = self.get_block_record_by_height(newer_block_height).await?;
        self.get_network_space(&older_block.header_hash, &newer_block.header_hash)
            .await
    }
    async fn get_additions_and_removals(
        &self,
        header_hash: &Bytes32,
    ) -> Result<(Vec<CoinRecord>, Vec<CoinRecord>), Error> {
        let mut request_body = Map::new();
        request_body.insert("header_hash".to_string(), json!(header_hash));
        let resp = post::<AdditionsAndRemovalsResp>(
            &self.client,
            &get_url(self.host.as_str(), self.port, "get_additions_and_removals"),
            &request_body,
        )
        .await?;
        Ok((resp.additions, resp.removals))
    }
    async fn get_initial_freeze_period(&self) -> Result<u64, Error> {
        Ok(post::<InitialFreezePeriodResp>(
            &self.client,
            &get_url(self.host.as_str(), self.port, "get_initial_freeze_period"),
            &Map::new(),
        )
        .await?
        .initial_freeze_end_timestamp)
    }
    async fn get_network_info(&self) -> Result<NetworkInfo, Error> {
        let resp = post::<NetworkInfoResp>(
            &self.client,
            &get_url(self.host.as_str(), self.port, "get_network_info"),
            &Map::new(),
        )
        .await?;
        Ok(NetworkInfo {
            network_name: resp.network_name,
            network_prefix: resp.network_prefix,
        })
    }
    async fn get_recent_signage_point_or_eos(
        &self,
        sp_hash: Option<&Bytes32>,
        challenge_hash: Option<&Bytes32>,
    ) -> Result<SignagePointOrEOS, Error> {
        if sp_hash.is_some() && challenge_hash.is_some() {
            return Err(Error::new(ErrorKind::InvalidInput, "InvalidArgument get_recent_signage_point_or_eos: One of sp_hash or challenge_hash must be None"));
        }
        let mut request_body = Map::new();
        if sp_hash.is_some() {
            request_body.insert("sp_hash".to_string(), json!(sp_hash));
        } else if challenge_hash.is_some() {
            request_body.insert("challenge_hash".to_string(), json!(challenge_hash));
        }
        let resp = post::<SignagePointOrEOSResp>(
            &self.client,
            &get_url(
                self.host.as_str(),
                self.port,
                "get_recent_signage_point_or_eos",
            ),
            &request_body,
        )
        .await?;
        Ok(SignagePointOrEOS {
            signage_point: resp.signage_point,
            eos: resp.eos,
            time_received: resp.time_received,
            reverted: resp.reverted,
        })
    }
    async fn get_coin_records_by_puzzle_hash(
        &self,
        puzzle_hash: &Bytes32,
        include_spent_coins: bool,
        start_height: u32,
        end_height: u32,
    ) -> Result<Vec<CoinRecord>, Error> {
        let mut request_body = Map::new();
        request_body.insert("puzzle_hash".to_string(), json!(puzzle_hash));
        request_body.insert(
            "include_spent_coins".to_string(),
            json!(include_spent_coins),
        );
        request_body.insert("start_height".to_string(), json!(start_height));
        request_body.insert("end_height".to_string(), json!(end_height));
        Ok(post::<CoinRecordAryResp>(
            &self.client,
            &get_url(
                self.host.as_str(),
                self.port,
                "get_coin_records_by_puzzle_hash",
            ),
            &request_body,
        )
        .await?
        .coin_records)
    }
    async fn get_coin_records_by_puzzle_hashes(
        &self,
        puzzle_hashes: Vec<&Bytes32>,
        include_spent_coins: bool,
        start_height: u32,
        end_height: u32,
    ) -> Result<Vec<CoinRecord>, Error> {
        let mut request_body = Map::new();
        request_body.insert("puzzle_hashes".to_string(), json!(puzzle_hashes));
        request_body.insert(
            "include_spent_coins".to_string(),
            json!(include_spent_coins),
        );
        request_body.insert("start_height".to_string(), json!(start_height));
        request_body.insert("end_height".to_string(), json!(end_height));
        Ok(post::<CoinRecordAryResp>(
            &self.client,
            &get_url(
                self.host.as_str(),
                self.port,
                "get_coin_records_by_puzzle_hashes",
            ),
            &request_body,
        )
        .await?
        .coin_records)
    }

    async fn get_coin_record_by_name(&self, name: &Bytes32) -> Result<Option<CoinRecord>, Error> {
        let mut request_body = Map::new();
        request_body.insert("name".to_string(), json!(name));
        Ok(post::<CoinRecordResp>(
            &self.client,
            &get_url(self.host.as_str(), self.port, "get_coin_record_by_name"),
            &request_body,
        )
        .await?
        .coin_record)
    }
    async fn get_coin_records_by_parent_ids(
        &self,
        parent_ids: Vec<&Bytes32>,
        include_spent_coins: bool,
        start_height: u32,
        end_height: u32,
    ) -> Result<Vec<CoinRecord>, Error> {
        let mut request_body = Map::new();
        request_body.insert("parent_ids".to_string(), json!(parent_ids));
        request_body.insert(
            "include_spent_coins".to_string(),
            json!(include_spent_coins),
        );
        request_body.insert("start_height".to_string(), json!(start_height));
        request_body.insert("end_height".to_string(), json!(end_height));
        Ok(post::<CoinRecordAryResp>(
            &self.client,
            &get_url(
                self.host.as_str(),
                self.port,
                "get_coin_records_by_parent_ids",
            ),
            &request_body,
        )
        .await?
        .coin_records)
    }
    async fn push_tx(&self, spend_bundle: &SpendBundle) -> Result<TXStatus, Error> {
        let mut request_body = Map::new();
        request_body.insert("spend_bundle".to_string(), json!(spend_bundle));
        Ok(post::<TXResp>(
            &self.client,
            &get_url(self.host.as_str(), self.port, "push_tx"),
            &request_body,
        )
        .await?
        .status)
    }
    async fn get_puzzle_and_solution(
        &self,
        coin_id: &Bytes32,
        height: u32,
    ) -> Result<CoinSpend, Error> {
        let mut request_body = Map::new();
        request_body.insert("coin_id".to_string(), json!(coin_id));
        request_body.insert("height".to_string(), json!(height));
        Ok(post::<CoinSpendResp>(
            &self.client,
            &get_url(self.host.as_str(), self.port, "get_puzzle_and_solution"),
            &request_body,
        )
        .await?
        .coin_solution)
    }
    async fn get_coin_spend(&self, coin_record: &CoinRecord) -> Result<CoinSpend, Error> {
        self.get_puzzle_and_solution(&coin_record.coin.name(), coin_record.spent_block_index)
            .await
    }
    async fn get_all_mempool_tx_ids(&self) -> Result<Vec<String>, Error> {
        Ok(post::<MempoolTXResp>(
            &self.client,
            &get_url(self.host.as_str(), self.port, "get_all_mempool_tx_ids"),
            &Map::new(),
        )
        .await?
        .tx_ids)
    }
    async fn get_all_mempool_items(&self) -> Result<HashMap<String, MemPoolItem>, Error> {
        Ok(post::<MempoolItemsResp>(
            &self.client,
            &get_url(self.host.as_str(), self.port, "get_all_mempool_items"),
            &Map::new(),
        )
        .await?
        .mempool_items)
    }
    async fn get_mempool_item_by_tx_id(&self, tx_id: &str) -> Result<MemPoolItem, Error> {
        let mut request_body = Map::new();
        request_body.insert("tx_id".to_string(), json!(tx_id));
        Ok(post::<MempoolItemResp>(
            &self.client,
            &get_url(self.host.as_str(), self.port, "get_mempool_item_by_tx_id"),
            &request_body,
        )
        .await?
        .mempool_item)
    }
}
