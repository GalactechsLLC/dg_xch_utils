use crate::api::full_node::FullnodeAPI;
use crate::api::responses::{AutoFarmResp, EmptyResponse};
use crate::api::simulator::SimulatorAPI;
use crate::rpc::full_node::{FullnodeClient, UrlFunction};
use crate::rpc::{get_http_client, get_insecure_url, post, ChiaRpcError};
use async_trait::async_trait;
use dg_xch_core::blockchain::block_record::BlockRecord;
use dg_xch_core::blockchain::blockchain_state::BlockchainState;
use dg_xch_core::blockchain::coin_record::CoinRecord;
use dg_xch_core::blockchain::coin_spend::CoinSpend;
use dg_xch_core::blockchain::full_block::FullBlock;
use dg_xch_core::blockchain::mempool_item::MempoolItem;
use dg_xch_core::blockchain::network_info::NetworkInfo;
use dg_xch_core::blockchain::signage_point_or_eos::SignagePointOrEOS;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::blockchain::spend_bundle::SpendBundle;
use dg_xch_core::blockchain::tx_status::TXStatus;
use dg_xch_core::blockchain::unfinished_header_block::UnfinishedHeaderBlock;
use dg_xch_core::protocols::full_node::{BlockCountMetrics, FeeEstimate};
use dg_xch_keys::encode_puzzle_hash;
use reqwest::Client;
use serde_json::{json, Map};
use std::collections::HashMap;
use std::hash::RandomState;
use std::sync::Arc;

pub struct SimulatorClient {
    client: Client,
    pub full_node_client: FullnodeClient,
    pub host: String,
    pub port: u16,
    pub additional_headers: Option<HashMap<String, String>>,
    url_function: UrlFunction,
}

impl SimulatorClient {
    pub fn new(
        host: &str,
        port: u16,
        timeout: u64,
        additional_headers: &Option<HashMap<String, String>>,
    ) -> Self {
        SimulatorClient {
            client: get_http_client(timeout).unwrap(),
            full_node_client: FullnodeClient::new_simulator(host, port, timeout),
            host: host.to_string(),
            port,
            additional_headers: additional_headers.clone(),
            url_function: Arc::new(get_insecure_url),
        }
    }
}
#[async_trait]
impl SimulatorAPI for SimulatorClient {
    async fn farm_blocks(
        &self,
        address: Bytes32,
        blocks: i64,
        transaction_block: bool,
    ) -> Result<EmptyResponse, ChiaRpcError> {
        let mut request_body = Map::new();
        request_body.insert(
            "address".to_string(),
            json!(
                encode_puzzle_hash(&address, "xch").map_err(|e| ChiaRpcError {
                    error: Some(format!("{}", e)),
                    success: false,
                })?
            ),
        );
        request_body.insert("blocks".to_string(), json!(blocks));
        request_body.insert("guarantee_tx_block".to_string(), json!(transaction_block));
        Ok(post::<EmptyResponse, RandomState>(
            &self.client,
            &(self.url_function)(self.host.as_str(), self.port, "farm_block"),
            &request_body,
            &self.additional_headers,
        )
        .await?)
    }

    async fn set_auto_farming(&self, should_auto_farm: bool) -> Result<AutoFarmResp, ChiaRpcError> {
        let mut request_body = Map::new();
        request_body.insert("should_auto_farm".to_string(), json!(should_auto_farm));
        Ok(post::<AutoFarmResp, RandomState>(
            &self.client,
            &(self.url_function)(self.host.as_str(), self.port, "set_auto_farming"),
            &request_body,
            &self.additional_headers,
        )
        .await?)
    }

    async fn get_auto_farming(&self) -> Result<AutoFarmResp, ChiaRpcError> {
        Ok(post::<AutoFarmResp, RandomState>(
            &self.client,
            &(self.url_function)(self.host.as_str(), self.port, "get_auto_farming"),
            &Map::new(),
            &self.additional_headers,
        )
        .await?)
    }
}
#[async_trait]
impl FullnodeAPI for SimulatorClient {
    async fn get_blockchain_state(&self) -> Result<BlockchainState, ChiaRpcError> {
        self.full_node_client.get_blockchain_state().await
    }
    async fn get_block(&self, header_hash: &Bytes32) -> Result<FullBlock, ChiaRpcError> {
        self.full_node_client.get_block(header_hash).await
    }
    async fn get_blocks(
        &self,
        start: u32,
        end: u32,
        exclude_header_hash: bool,
        exclude_reorged: bool,
    ) -> Result<Vec<FullBlock>, ChiaRpcError> {
        self.full_node_client
            .get_blocks(start, end, exclude_header_hash, exclude_reorged)
            .await
    }
    async fn get_all_blocks(&self, start: u32, end: u32) -> Result<Vec<FullBlock>, ChiaRpcError> {
        self.full_node_client.get_all_blocks(start, end).await
    }
    async fn get_block_count_metrics(&self) -> Result<BlockCountMetrics, ChiaRpcError> {
        self.full_node_client.get_block_count_metrics().await
    }
    async fn get_block_record_by_height(&self, height: u32) -> Result<BlockRecord, ChiaRpcError> {
        self.full_node_client
            .get_block_record_by_height(height)
            .await
    }
    async fn get_block_record(&self, header_hash: &Bytes32) -> Result<BlockRecord, ChiaRpcError> {
        self.full_node_client.get_block_record(header_hash).await
    }
    async fn get_block_records(
        &self,
        start: u32,
        end: u32,
    ) -> Result<Vec<BlockRecord>, ChiaRpcError> {
        self.full_node_client.get_block_records(start, end).await
    }
    async fn get_unfinished_block_headers(
        &self,
    ) -> Result<Vec<UnfinishedHeaderBlock>, ChiaRpcError> {
        self.full_node_client.get_unfinished_block_headers().await
    }
    async fn get_network_space(
        &self,
        older_block_header_hash: &Bytes32,
        newer_block_header_hash: &Bytes32,
    ) -> Result<u64, ChiaRpcError> {
        self.full_node_client
            .get_network_space(older_block_header_hash, newer_block_header_hash)
            .await
    }
    async fn get_network_space_by_height(
        &self,
        older_block_height: u32,
        newer_block_height: u32,
    ) -> Result<u64, ChiaRpcError> {
        self.full_node_client
            .get_network_space_by_height(older_block_height, newer_block_height)
            .await
    }
    async fn get_additions_and_removals(
        &self,
        header_hash: &Bytes32,
    ) -> Result<(Vec<CoinRecord>, Vec<CoinRecord>), ChiaRpcError> {
        self.full_node_client
            .get_additions_and_removals(header_hash)
            .await
    }
    async fn get_initial_freeze_period(&self) -> Result<u64, ChiaRpcError> {
        self.full_node_client.get_initial_freeze_period().await
    }
    async fn get_network_info(&self) -> Result<NetworkInfo, ChiaRpcError> {
        self.full_node_client.get_network_info().await
    }
    async fn get_recent_signage_point_or_eos(
        &self,
        sp_hash: Option<&Bytes32>,
        challenge_hash: Option<&Bytes32>,
    ) -> Result<SignagePointOrEOS, ChiaRpcError> {
        self.full_node_client
            .get_recent_signage_point_or_eos(sp_hash, challenge_hash)
            .await
    }
    async fn get_coin_records_by_puzzle_hash(
        &self,
        puzzle_hash: &Bytes32,
        include_spent_coins: Option<bool>,
        start_height: Option<u32>,
        end_height: Option<u32>,
    ) -> Result<Vec<CoinRecord>, ChiaRpcError> {
        self.full_node_client
            .get_coin_records_by_puzzle_hash(
                puzzle_hash,
                include_spent_coins,
                start_height,
                end_height,
            )
            .await
    }
    async fn get_coin_records_by_puzzle_hashes(
        &self,
        puzzle_hashes: &[Bytes32],
        include_spent_coins: Option<bool>,
        start_height: Option<u32>,
        end_height: Option<u32>,
    ) -> Result<Vec<CoinRecord>, ChiaRpcError> {
        self.full_node_client
            .get_coin_records_by_puzzle_hashes(
                puzzle_hashes,
                include_spent_coins,
                start_height,
                end_height,
            )
            .await
    }
    async fn get_coin_record_by_name(
        &self,
        name: &Bytes32,
    ) -> Result<Option<CoinRecord>, ChiaRpcError> {
        self.full_node_client.get_coin_record_by_name(name).await
    }
    async fn get_coin_records_by_names(
        &self,
        names: &[Bytes32],
        include_spent_coins: bool,
        start_height: u32,
        end_height: u32,
    ) -> Result<Vec<CoinRecord>, ChiaRpcError> {
        self.full_node_client
            .get_coin_records_by_names(names, include_spent_coins, start_height, end_height)
            .await
    }
    async fn get_coin_records_by_parent_ids(
        &self,
        parent_ids: &[Bytes32],
        include_spent_coins: bool,
        start_height: u32,
        end_height: u32,
    ) -> Result<Vec<CoinRecord>, ChiaRpcError> {
        self.full_node_client
            .get_coin_records_by_parent_ids(
                parent_ids,
                include_spent_coins,
                start_height,
                end_height,
            )
            .await
    }
    async fn get_coin_records_by_hint(
        &self,
        hint: &Bytes32,
        include_spent_coins: bool,
        start_height: u32,
        end_height: u32,
    ) -> Result<Vec<CoinRecord>, ChiaRpcError> {
        self.full_node_client
            .get_coin_records_by_hint(hint, include_spent_coins, start_height, end_height)
            .await
    }
    async fn push_tx(&self, spend_bundle: &SpendBundle) -> Result<TXStatus, ChiaRpcError> {
        self.full_node_client.push_tx(spend_bundle).await
    }
    async fn get_puzzle_and_solution(
        &self,
        coin_id: &Bytes32,
        height: u32,
    ) -> Result<CoinSpend, ChiaRpcError> {
        self.full_node_client
            .get_puzzle_and_solution(coin_id, height)
            .await
    }
    async fn get_coin_spend(&self, coin_record: &CoinRecord) -> Result<CoinSpend, ChiaRpcError> {
        self.full_node_client.get_coin_spend(coin_record).await
    }
    async fn get_all_mempool_tx_ids(&self) -> Result<Vec<Bytes32>, ChiaRpcError> {
        self.full_node_client.get_all_mempool_tx_ids().await
    }
    async fn get_all_mempool_items(&self) -> Result<HashMap<Bytes32, MempoolItem>, ChiaRpcError> {
        self.full_node_client.get_all_mempool_items().await
    }

    async fn get_mempool_item_by_tx_id(&self, tx_id: &str) -> Result<MempoolItem, ChiaRpcError> {
        self.full_node_client.get_mempool_item_by_tx_id(tx_id).await
    }
    async fn get_mempool_items_by_coin_name(
        &self,
        coin_name: &Bytes32,
    ) -> Result<Vec<MempoolItem>, ChiaRpcError> {
        self.full_node_client
            .get_mempool_items_by_coin_name(coin_name)
            .await
    }
    async fn get_fee_estimate(
        &self,
        cost: Option<u64>,
        spend_bundle: Option<SpendBundle>,
        spend_type: Option<String>,
        target_times: &[u64],
    ) -> Result<FeeEstimate, ChiaRpcError> {
        self.full_node_client
            .get_fee_estimate(cost, spend_bundle, spend_type, target_times)
            .await
    }
}
