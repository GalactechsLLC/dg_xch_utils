use crate::api::responses::UrlFunction;
use crate::api::responses::full_node_responses::{
    AdditionsAndRemovalsResp, BlockCountMetricsResp, BlockRecordAryResp, BlockRecordResp,
    BlockchainStateResp, CoinHintsResp, CoinRecordAryResp, CoinRecordResp, CoinSpendMapResp,
    CoinSpendResp, FeeEstimateResp, FullBlockAryResp, FullBlockResp,
    HintedAdditionsAndRemovalsResp, MempoolItemAryResp, MempoolItemResp, MempoolItemsResp,
    MempoolTXResp, NetSpaceResp, NetworkInfoResp, PaginatedCoinRecordAryResp,
    SignagePointOrEOSResp, SingletonByLauncherIdResp, TXResp, UnfinishedBlockAryResp,
};
use crate::rpc::{ChiaRpcError, get_client, get_http_client, get_insecure_url, get_url};
use crate::{ClientSSLConfig, generate_rpc};
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
use dg_xch_core::protocols::full_node::{BlockCountMetrics, FeeEstimate};
use reqwest::Client;
use std::collections::HashMap;
use std::io::Error;
use std::sync::Arc;

#[derive(Clone)]
pub struct FullnodeClient {
    pub client: Client,
    pub secure: bool,
    pub host: String,
    pub port: u16,
    pub ssl_path: Option<ClientSSLConfig>,
    pub additional_headers: Option<HashMap<String, String>>,
    pub url_function: UrlFunction,
}

impl FullnodeClient {
    pub async fn get_node_details(&self) -> Result<serde_json::Value, Error> {
        let response: serde_json::Value = crate::rpc::post(
            &self.client,
            &(self.url_function)(&self.host, self.port, "get_node_details"),
            &serde_json::Map::new(),
            &self.additional_headers,
        )
        .await?;
        if response.get("success").and_then(serde_json::Value::as_bool) != Some(true) {
            return Err(Error::other(
                "node diagnostics unavailable on this RPC server",
            ));
        }
        response
            .get("node_details")
            .cloned()
            .ok_or_else(|| Error::other("missing node diagnostics"))
    }

    pub fn new_verified(
        host: &str,
        port: u16,
        timeout: u64,
        ssl_path: Option<ClientSSLConfig>,
        additional_headers: &Option<HashMap<String, String>>,
    ) -> Result<Self, Error> {
        Ok(Self {
            client: get_client(&ssl_path, timeout)?,
            secure: true,
            host: host.to_owned(),
            port,
            ssl_path,
            additional_headers: additional_headers.clone(),
            url_function: Arc::new(get_url),
        })
    }

    pub fn new(
        host: &str,
        port: u16,
        timeout: u64,
        ssl_path: Option<ClientSSLConfig>,
        additional_headers: &Option<HashMap<String, String>>,
    ) -> Result<Self, Error> {
        Self::new_verified(host, port, timeout, ssl_path, additional_headers)
    }
    pub fn new_simulator(host: &str, port: u16, timeout: u64) -> Result<Self, Error> {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        Ok(FullnodeClient {
            client: get_http_client(timeout)?,
            secure: false,
            host: host.to_string(),
            port,
            ssl_path: None,
            additional_headers: None,
            url_function: Arc::new(get_insecure_url),
        })
    }
    pub fn dummy() -> Self {
        Self {
            client: Default::default(),
            secure: false,
            host: "localhost".to_string(),
            port: 8555,
            ssl_path: None,
            additional_headers: None,
            url_function: Arc::new(get_url),
        }
    }
}

generate_rpc!(
    FullnodeAPI,
    FullnodeClient,
    [
        (get_blockchain_state, [], BlockchainStateResp, BlockchainState),
        (get_block, [header_hash: &Bytes32], FullBlockResp, FullBlock),
        (get_blocks, [
            start: u32,
            end: u32,
            exclude_header_hash: bool,
            exclude_reorged: bool
        ], FullBlockAryResp, Vec<FullBlock>),
        (get_block_count_metrics, [], BlockCountMetricsResp, BlockCountMetrics),
        (get_block_record_by_height, [height: u32], BlockRecordResp, BlockRecord),
        (get_block_record, [header_hash: &Bytes32], BlockRecordResp, BlockRecord),
        (get_block_records, [start: u32, end: u32], BlockRecordAryResp, Vec<BlockRecord>),
        (get_unfinished_block_headers, [], UnfinishedBlockAryResp, Vec<UnfinishedHeaderBlock>),
        (get_network_space, [
            older_block_header_hash: &Bytes32,
            newer_block_header_hash: &Bytes32
        ], NetSpaceResp, u64),
        (get_additions_and_removals, [
            header_hash: &Bytes32
        ], AdditionsAndRemovalsResp, (Vec<CoinRecord>, Vec<CoinRecord>)),
        (get_network_info, [], NetworkInfoResp, NetworkInfo),
        (get_recent_signage_point_or_eos, [
            sp_hash: Option<&Bytes32>,
            challenge_hash: Option<&Bytes32>
        ] , SignagePointOrEOSResp, SignagePointOrEOS),
        (get_coin_records_by_puzzle_hash, [
            puzzle_hash: &Bytes32,
            include_spent_coins: Option<bool>,
            start_height: Option<u32>,
            end_height: Option<u32>
        ], CoinRecordAryResp, Vec<CoinRecord>),
        (get_coin_records_by_puzzle_hashes, [
            puzzle_hashes: &[Bytes32],
            include_spent_coins: Option<bool>,
            start_height: Option<u32>,
            end_height: Option<u32>
        ], CoinRecordAryResp, Vec<CoinRecord>),
        (get_coin_record_by_name, [name: &Bytes32], CoinRecordResp, Option<CoinRecord>),
        (get_coin_records_by_names, [
            names: &[Bytes32],
            include_spent_coins: Option<bool>,
            start_height: Option<u32>,
            end_height: Option<u32>
        ], CoinRecordAryResp, Vec<CoinRecord>),
        (get_coin_records_by_parent_ids, [
            parent_ids: &[Bytes32],
            include_spent_coins: Option<bool>,
            start_height: Option<u32>,
            end_height: Option<u32>
        ], CoinRecordAryResp, Vec<CoinRecord>),
        (get_coin_records_by_hint, [
            hint: &Bytes32,
            include_spent_coins: Option<bool>,
            start_height: Option<u32>,
            end_height: Option<u32>
        ], CoinRecordAryResp, Vec<CoinRecord>),
        (get_puzzle_and_solution, [
            coin_id: &Bytes32,
            height: u32
        ], CoinSpendResp, CoinSpend),
        (get_all_mempool_tx_ids, [], MempoolTXResp, Vec<Bytes32>),
        (get_all_mempool_items, [], MempoolItemsResp, HashMap<Bytes32, MempoolItem>),
        (get_mempool_item_by_tx_id, [tx_id: &str], MempoolItemResp, MempoolItem),
        (get_mempool_items_by_coin_name, [coin_name: &Bytes32], MempoolItemAryResp, Vec<MempoolItem>),
        (get_fee_estimate, [
            cost: Option<u64>,
            spend_bundle: Option<SpendBundle>,
            spend_type: Option<String>,
            target_times: &[u64]
        ], FeeEstimateResp, FeeEstimate),
        (push_tx, [spend_bundle: &SpendBundle], TXResp, TXStatus),
    ]
);

#[async_trait]
pub trait FullnodeHelpers: FullnodeAPI + Send + Sync {
    async fn get_all_blocks(&self, start: u32, end: u32) -> Result<Vec<FullBlock>, ChiaRpcError> {
        self.get_blocks(start, end, true, false).await
    }
    async fn get_network_space_by_height(
        &self,
        old_h: u32,
        new_h: u32,
    ) -> Result<u64, ChiaRpcError> {
        let older = self.get_block_record_by_height(old_h).await?;
        let newer = self.get_block_record_by_height(new_h).await?;
        self.get_network_space(&older.header_hash, &newer.header_hash)
            .await
    }
    async fn get_coin_spend(&self, cr: &CoinRecord) -> Result<CoinSpend, ChiaRpcError> {
        self.get_puzzle_and_solution(&cr.coin.name(), cr.spent_block_index)
            .await
    }
}
impl<T> FullnodeHelpers for T where T: FullnodeAPI + Send + Sync {}

generate_rpc!(
    FullnodeExtAPI,
    FullnodeClient,
    [
        (get_additions_and_removals_with_hints, [
            header_hash: &Bytes32
        ], HintedAdditionsAndRemovalsResp, (Vec<HintedCoinRecord>, Vec<HintedCoinRecord>)),
        (get_singleton_by_launcher_id, [
            launcher_id: &Bytes32
        ], SingletonByLauncherIdResp, (CoinRecord, CoinSpend)),
        (get_coin_records_by_hints_paginated, [
            hints: &[Bytes32],
            include_spent_coins: Option<bool>,
            start_height: Option<u32>,
            end_height: Option<u32>,
            page_size: u32,
            last_id: Option<Bytes32>
        ], PaginatedCoinRecordAryResp, (Vec<PaginatedCoinRecord>, Option<Bytes32>, Option<i32>)),
        (get_coin_records_by_puzzle_hashes_paginated, [
            puzzle_hashes: &[Bytes32],
            include_spent_coins: Option<bool>,
            start_height: Option<u32>,
            end_height: Option<u32>,
            page_size: u32,
            last_id: Option<Bytes32>
        ], PaginatedCoinRecordAryResp, (Vec<PaginatedCoinRecord>, Option<Bytes32>, Option<i32>)),
        (get_coin_records_by_hints, [
            hints: &[Bytes32],
            include_spent_coins: Option<bool>,
            start_height: Option<u32>,
            end_height: Option<u32>
        ], CoinRecordAryResp, Vec<CoinRecord>),
        (get_hints_by_coin_ids, [
            coin_ids: &[Bytes32]
        ], CoinHintsResp, HashMap<Bytes32, Bytes32>),
        (get_puzzles_and_solutions_by_names, [
            names: &[Bytes32],
            include_spent_coins: Option<bool>,
            start_height: Option<u32>,
            end_height: Option<u32>
        ], CoinSpendMapResp, HashMap<Bytes32, Option<CoinSpend>>)
    ]
);
