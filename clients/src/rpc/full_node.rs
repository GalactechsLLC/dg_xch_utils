use crate::api::full_node::{FullnodeAPI, FullnodeExtAPI};
use crate::api::responses::{
    AdditionsAndRemovalsResp, BlockRecordAryResp, BlockRecordResp, BlockchainStateResp,
    CoinRecordAryResp, CoinRecordResp, CoinSpendResp, FullBlockAryResp, FullBlockResp,
    InitialFreezePeriodResp, MempoolItemResp, MempoolItemsResp, MempoolTXResp, NetSpaceResp,
    NetworkInfoResp, SignagePointOrEOSResp, SingletonByLauncherIdResp, TXResp,
    UnfinishedBlockAryResp,
};
use crate::api::responses::{
    BlockCountMetricsResp, CoinHintsResp, CoinSpendMapResp, HintedAdditionsAndRemovalsResp,
    MempoolItemAryResp, PaginatedCoinRecordAryResp,
};
use crate::rpc::{get_client, get_http_client, get_insecure_url, get_url, post, ChiaRpcError};
use crate::ClientSSLConfig;
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
use log::error;
use reqwest::Client;
use serde_json::{json, Map};
use std::collections::HashMap;
use std::hash::RandomState;
use std::io::Error;
use std::sync::Arc;

pub type UrlFunction = Arc<dyn Fn(&str, u16, &str) -> String + Send + Sync + 'static>;

#[derive(Clone)]
pub struct FullnodeClient {
    client: Client,
    pub secure: bool,
    pub host: String,
    pub port: u16,
    pub ssl_path: Option<ClientSSLConfig>,
    pub additional_headers: Option<HashMap<String, String>>,
    url_function: UrlFunction,
}

impl FullnodeClient {
    pub fn new(
        host: &str,
        port: u16,
        timeout: u64,
        ssl_path: Option<ClientSSLConfig>,
        additional_headers: &Option<HashMap<String, String>>,
    ) -> Result<Self, Error> {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        Ok(FullnodeClient {
            client: get_client(&ssl_path, timeout)?,
            secure: true,
            host: host.to_string(),
            port,
            ssl_path,
            additional_headers: additional_headers.clone(),
            url_function: Arc::new(get_url),
        })
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
}

#[async_trait]
impl FullnodeAPI for FullnodeClient {
    async fn get_blockchain_state(&self) -> Result<BlockchainState, ChiaRpcError> {
        Ok(post::<BlockchainStateResp, RandomState>(
            &self.client,
            &(self.url_function)(self.host.as_str(), self.port, "get_blockchain_state"),
            &Map::new(),
            &self.additional_headers,
        )
        .await?
        .blockchain_state)
    }
    async fn get_block(&self, header_hash: &Bytes32) -> Result<FullBlock, ChiaRpcError> {
        let mut request_body = Map::new();
        request_body.insert("header_hash".to_string(), json!(header_hash));
        Ok(post::<FullBlockResp, RandomState>(
            &self.client,
            &(self.url_function)(self.host.as_str(), self.port, "get_block"),
            &request_body,
            &self.additional_headers,
        )
        .await?
        .block)
    }
    async fn get_blocks(
        &self,
        start: u32,
        end: u32,
        exclude_header_hash: bool,
        exclude_reorged: bool,
    ) -> Result<Vec<FullBlock>, ChiaRpcError> {
        let mut request_body = Map::new();
        request_body.insert("start".to_string(), json!(start));
        request_body.insert("end".to_string(), json!(end));
        request_body.insert(
            "exclude_header_hash".to_string(),
            json!(exclude_header_hash),
        );
        request_body.insert("exclude_reorged".to_string(), json!(exclude_reorged));
        Ok(post::<FullBlockAryResp, RandomState>(
            &self.client,
            &(self.url_function)(self.host.as_str(), self.port, "get_blocks"),
            &request_body,
            &self.additional_headers,
        )
        .await?
        .blocks)
    }
    async fn get_all_blocks(&self, start: u32, end: u32) -> Result<Vec<FullBlock>, ChiaRpcError> {
        self.get_blocks(start, end, true, false).await
    }
    async fn get_block_count_metrics(&self) -> Result<BlockCountMetrics, ChiaRpcError> {
        Ok(post::<BlockCountMetricsResp, RandomState>(
            &self.client,
            &(self.url_function)(self.host.as_str(), self.port, "get_block_count_metrics"),
            &Map::new(),
            &self.additional_headers,
        )
        .await?
        .metrics)
    }
    async fn get_block_record_by_height(&self, height: u32) -> Result<BlockRecord, ChiaRpcError> {
        let mut request_body = Map::new();
        request_body.insert("height".to_string(), json!(height));
        Ok(post::<BlockRecordResp, RandomState>(
            &self.client,
            &(self.url_function)(self.host.as_str(), self.port, "get_block_record_by_height"),
            &request_body,
            &self.additional_headers,
        )
        .await?
        .block_record)
    }
    async fn get_block_record(&self, header_hash: &Bytes32) -> Result<BlockRecord, ChiaRpcError> {
        let mut request_body = Map::new();
        request_body.insert("header_hash".to_string(), json!(header_hash));
        Ok(post::<BlockRecordResp, RandomState>(
            &self.client,
            &(self.url_function)(self.host.as_str(), self.port, "get_block_record"),
            &request_body,
            &self.additional_headers,
        )
        .await?
        .block_record)
    }
    async fn get_block_records(
        &self,
        start: u32,
        end: u32,
    ) -> Result<Vec<BlockRecord>, ChiaRpcError> {
        let mut request_body = Map::new();
        request_body.insert("start".to_string(), json!(start));
        request_body.insert("end".to_string(), json!(end));
        Ok(post::<BlockRecordAryResp, RandomState>(
            &self.client,
            &(self.url_function)(self.host.as_str(), self.port, "get_block_records"),
            &request_body,
            &self.additional_headers,
        )
        .await?
        .block_records)
    }
    async fn get_unfinished_block_headers(
        &self,
    ) -> Result<Vec<UnfinishedHeaderBlock>, ChiaRpcError> {
        Ok(post::<UnfinishedBlockAryResp, RandomState>(
            &self.client,
            &(self.url_function)(
                self.host.as_str(),
                self.port,
                "get_unfinished_block_headers",
            ),
            &Map::new(),
            &self.additional_headers,
        )
        .await?
        .headers)
    }
    async fn get_network_space(
        &self,
        older_block_header_hash: &Bytes32,
        newer_block_header_hash: &Bytes32,
    ) -> Result<u64, ChiaRpcError> {
        let mut request_body = Map::new();
        request_body.insert(
            "older_block_header_hash".to_string(),
            json!(older_block_header_hash),
        );
        request_body.insert(
            "newer_block_header_hash".to_string(),
            json!(newer_block_header_hash),
        );
        Ok(post::<NetSpaceResp, RandomState>(
            &self.client,
            &(self.url_function)(self.host.as_str(), self.port, "get_network_space"),
            &request_body,
            &self.additional_headers,
        )
        .await?
        .space)
    }
    async fn get_network_space_by_height(
        &self,
        older_block_height: u32,
        newer_block_height: u32,
    ) -> Result<u64, ChiaRpcError> {
        let older_block = self.get_block_record_by_height(older_block_height).await?;
        let newer_block = self.get_block_record_by_height(newer_block_height).await?;
        self.get_network_space(&older_block.header_hash, &newer_block.header_hash)
            .await
    }
    async fn get_additions_and_removals(
        &self,
        header_hash: &Bytes32,
    ) -> Result<(Vec<CoinRecord>, Vec<CoinRecord>), ChiaRpcError> {
        let mut request_body = Map::new();
        request_body.insert("header_hash".to_string(), json!(header_hash));
        let resp = post::<AdditionsAndRemovalsResp, RandomState>(
            &self.client,
            &(self.url_function)(self.host.as_str(), self.port, "get_additions_and_removals"),
            &request_body,
            &self.additional_headers,
        )
        .await?;
        Ok((resp.additions, resp.removals))
    }
    async fn get_initial_freeze_period(&self) -> Result<u64, ChiaRpcError> {
        Ok(post::<InitialFreezePeriodResp, RandomState>(
            &self.client,
            &(self.url_function)(self.host.as_str(), self.port, "get_initial_freeze_period"),
            &Map::new(),
            &self.additional_headers,
        )
        .await?
        .initial_freeze_end_timestamp)
    }
    async fn get_network_info(&self) -> Result<NetworkInfo, ChiaRpcError> {
        let resp = post::<NetworkInfoResp, RandomState>(
            &self.client,
            &(self.url_function)(self.host.as_str(), self.port, "get_network_info"),
            &Map::new(),
            &self.additional_headers,
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
    ) -> Result<SignagePointOrEOS, ChiaRpcError> {
        if sp_hash.is_some() && challenge_hash.is_some() {
            return Err(ChiaRpcError {
                error: Some("InvalidArgument get_recent_signage_point_or_eos: One of sp_hash or challenge_hash must be None".to_string()),
                success: false,
            });
        }
        let mut request_body = Map::new();
        if sp_hash.is_some() {
            request_body.insert("sp_hash".to_string(), json!(sp_hash));
        } else if challenge_hash.is_some() {
            request_body.insert("challenge_hash".to_string(), json!(challenge_hash));
        }
        let resp = post::<SignagePointOrEOSResp, RandomState>(
            &self.client,
            &(self.url_function)(
                self.host.as_str(),
                self.port,
                "get_recent_signage_point_or_eos",
            ),
            &request_body,
            &self.additional_headers,
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
        include_spent_coins: Option<bool>,
        start_height: Option<u32>,
        end_height: Option<u32>,
    ) -> Result<Vec<CoinRecord>, ChiaRpcError> {
        let mut request_body = Map::new();
        request_body.insert("puzzle_hash".to_string(), json!(puzzle_hash));
        if let Some(include_spent_coins) = include_spent_coins {
            request_body.insert(
                "include_spent_coins".to_string(),
                json!(include_spent_coins),
            );
        }
        if let Some(start_height) = start_height {
            request_body.insert("start_height".to_string(), json!(start_height));
        }
        if let Some(end_height) = end_height {
            request_body.insert("end_height".to_string(), json!(end_height));
        }
        Ok(post::<CoinRecordAryResp, RandomState>(
            &self.client,
            &(self.url_function)(
                self.host.as_str(),
                self.port,
                "get_coin_records_by_puzzle_hash",
            ),
            &request_body,
            &self.additional_headers,
        )
        .await?
        .coin_records)
    }
    async fn get_coin_records_by_puzzle_hashes(
        &self,
        puzzle_hashes: &[Bytes32],
        include_spent_coins: Option<bool>,
        start_height: Option<u32>,
        end_height: Option<u32>,
    ) -> Result<Vec<CoinRecord>, ChiaRpcError> {
        let mut request_body = Map::new();
        request_body.insert("puzzle_hashes".to_string(), json!(puzzle_hashes));
        request_body.insert(
            "include_spent_coins".to_string(),
            json!(include_spent_coins.unwrap_or(true)),
        );
        if let Some(sh) = start_height {
            request_body.insert("start_height".to_string(), json!(sh));
        }
        if let Some(eh) = end_height {
            request_body.insert("end_height".to_string(), json!(eh));
        }
        Ok(post::<CoinRecordAryResp, RandomState>(
            &self.client,
            &(self.url_function)(
                self.host.as_str(),
                self.port,
                "get_coin_records_by_puzzle_hashes",
            ),
            &request_body,
            &self.additional_headers,
        )
        .await?
        .coin_records)
    }
    async fn get_coin_record_by_name(
        &self,
        name: &Bytes32,
    ) -> Result<Option<CoinRecord>, ChiaRpcError> {
        let mut request_body = Map::new();
        request_body.insert("name".to_string(), json!(name));
        Ok(post::<CoinRecordResp, RandomState>(
            &self.client,
            &(self.url_function)(self.host.as_str(), self.port, "get_coin_record_by_name"),
            &request_body,
            &self.additional_headers,
        )
        .await?
        .coin_record)
    }
    async fn get_coin_records_by_names(
        &self,
        names: &[Bytes32],
        include_spent_coins: Option<bool>,
        start_height: Option<u32>,
        end_height: Option<u32>,
    ) -> Result<Vec<CoinRecord>, ChiaRpcError> {
        let mut request_body = Map::new();
        request_body.insert("names".to_string(), json!(names));
        if let Some(v) = include_spent_coins {
            request_body.insert("include_spent_coins".to_string(), json!(v));
        }
        if let Some(v) = start_height {
            request_body.insert("start_height".to_string(), json!(v));
        }
        if let Some(v) = end_height {
            request_body.insert("end_height".to_string(), json!(v));
        }
        Ok(post::<CoinRecordAryResp, RandomState>(
            &self.client,
            &(self.url_function)(self.host.as_str(), self.port, "get_coin_records_by_names"),
            &request_body,
            &self.additional_headers,
        )
        .await?
        .coin_records)
    }
    async fn get_coin_records_by_parent_ids(
        &self,
        parent_ids: &[Bytes32],
        include_spent_coins: Option<bool>,
        start_height: Option<u32>,
        end_height: Option<u32>,
    ) -> Result<Vec<CoinRecord>, ChiaRpcError> {
        //todo make options
        let mut request_body = Map::new();
        request_body.insert("parent_ids".to_string(), json!(parent_ids));
        request_body.insert(
            "include_spent_coins".to_string(),
            json!(include_spent_coins),
        );
        request_body.insert("start_height".to_string(), json!(start_height));
        request_body.insert("end_height".to_string(), json!(end_height));
        Ok(post::<CoinRecordAryResp, RandomState>(
            &self.client,
            &(self.url_function)(
                self.host.as_str(),
                self.port,
                "get_coin_records_by_parent_ids",
            ),
            &request_body,
            &self.additional_headers,
        )
        .await?
        .coin_records)
    }
    async fn get_coin_records_by_hint(
        &self,
        hint: &Bytes32,
        include_spent_coins: Option<bool>,
        start_height: Option<u32>,
        end_height: Option<u32>,
    ) -> Result<Vec<CoinRecord>, ChiaRpcError> {
        //todo make options
        let mut request_body = Map::new();
        request_body.insert("hint".to_string(), json!(hint));
        request_body.insert(
            "include_spent_coins".to_string(),
            json!(include_spent_coins),
        );
        request_body.insert("start_height".to_string(), json!(start_height));
        request_body.insert("end_height".to_string(), json!(end_height));
        Ok(post::<CoinRecordAryResp, RandomState>(
            &self.client,
            &(self.url_function)(self.host.as_str(), self.port, "get_coin_records_by_hint"),
            &request_body,
            &self.additional_headers,
        )
        .await?
        .coin_records)
    }
    async fn push_tx(&self, spend_bundle: &SpendBundle) -> Result<TXStatus, ChiaRpcError> {
        let mut retries = 0;
        let mut request_body = Map::new();
        request_body.insert("spend_bundle".to_string(), json!(spend_bundle));
        while retries < 3 {
            match post::<TXResp, RandomState>(
                &self.client,
                &(self.url_function)(self.host.as_str(), self.port, "push_tx"),
                &request_body,
                &self.additional_headers,
            )
            .await
            {
                Ok(v) => {
                    return Ok(v.status);
                }
                Err(e) => {
                    error!("Failed to Push TX({retries}): {e:?}");
                    retries += 1;
                }
            }
        }
        Err(ChiaRpcError {
            error: Some("Failed to push TX After 3 Tries".to_string()),
            success: false,
        })
    }
    async fn get_puzzle_and_solution(
        &self,
        coin_id: &Bytes32,
        height: u32,
    ) -> Result<CoinSpend, ChiaRpcError> {
        let mut request_body = Map::new();
        request_body.insert("coin_id".to_string(), json!(coin_id));
        request_body.insert("height".to_string(), json!(height));
        Ok(post::<CoinSpendResp, RandomState>(
            &self.client,
            &(self.url_function)(self.host.as_str(), self.port, "get_puzzle_and_solution"),
            &request_body,
            &self.additional_headers,
        )
        .await?
        .coin_solution)
    }
    async fn get_coin_spend(&self, coin_record: &CoinRecord) -> Result<CoinSpend, ChiaRpcError> {
        self.get_puzzle_and_solution(&coin_record.coin.name(), coin_record.spent_block_index)
            .await
    }
    async fn get_all_mempool_tx_ids(&self) -> Result<Vec<Bytes32>, ChiaRpcError> {
        Ok(post::<MempoolTXResp, RandomState>(
            &self.client,
            &(self.url_function)(self.host.as_str(), self.port, "get_all_mempool_tx_ids"),
            &Map::new(),
            &self.additional_headers,
        )
        .await?
        .tx_ids)
    }
    async fn get_all_mempool_items(&self) -> Result<HashMap<Bytes32, MempoolItem>, ChiaRpcError> {
        Ok(post::<MempoolItemsResp, RandomState>(
            &self.client,
            &(self.url_function)(self.host.as_str(), self.port, "get_all_mempool_items"),
            &Map::new(),
            &self.additional_headers,
        )
        .await?
        .mempool_items)
    }
    async fn get_mempool_item_by_tx_id(&self, tx_id: &str) -> Result<MempoolItem, ChiaRpcError> {
        let mut request_body = Map::new();
        request_body.insert("tx_id".to_string(), json!(tx_id));
        Ok(post::<MempoolItemResp, RandomState>(
            &self.client,
            &(self.url_function)(self.host.as_str(), self.port, "get_mempool_item_by_tx_id"),
            &request_body,
            &self.additional_headers,
        )
        .await?
        .mempool_item)
    }
    async fn get_mempool_items_by_coin_name(
        &self,
        coin_name: &Bytes32,
    ) -> Result<Vec<MempoolItem>, ChiaRpcError> {
        let mut request_body = Map::new();
        request_body.insert("coin_name".to_string(), json!(coin_name));
        Ok(post::<MempoolItemAryResp, RandomState>(
            &self.client,
            &(self.url_function)(
                self.host.as_str(),
                self.port,
                "get_mempool_items_by_coin_name",
            ),
            &request_body,
            &self.additional_headers,
        )
        .await?
        .mempool_items)
    }
    async fn get_fee_estimate(
        &self,
        cost: Option<u64>,
        spend_bundle: Option<SpendBundle>,
        spend_type: Option<String>,
        target_times: &[u64],
    ) -> Result<FeeEstimate, ChiaRpcError> {
        let mut request_body = Map::new();
        request_body.insert("cost".to_string(), json!(cost));
        request_body.insert("spend_bundle".to_string(), json!(spend_bundle));
        request_body.insert("spend_type".to_string(), json!(spend_type));
        request_body.insert("target_times".to_string(), json!(target_times));
        post::<FeeEstimate, RandomState>(
            &self.client,
            &(self.url_function)(self.host.as_str(), self.port, "get_fee_estimate"),
            &request_body,
            &self.additional_headers,
        )
        .await
    }
}

#[async_trait]
impl FullnodeExtAPI for FullnodeClient {
    async fn get_additions_and_removals_with_hints(
        &self,
        header_hash: &Bytes32,
    ) -> Result<(Vec<HintedCoinRecord>, Vec<HintedCoinRecord>), ChiaRpcError> {
        let mut request_body = Map::new();
        request_body.insert("header_hash".to_string(), json!(header_hash));
        let resp = post::<HintedAdditionsAndRemovalsResp, RandomState>(
            &self.client,
            &(self.url_function)(
                self.host.as_str(),
                self.port,
                "get_additions_and_removals_with_hints",
            ),
            &request_body,
            &self.additional_headers,
        )
        .await?;
        Ok((resp.additions, resp.removals))
    }
    async fn get_singleton_by_launcher_id(
        &self,
        launcher_id: &Bytes32,
    ) -> Result<(CoinRecord, CoinSpend), ChiaRpcError> {
        let mut request_body = Map::new();
        request_body.insert("launcher_id".to_string(), json!(launcher_id));
        let resp = post::<SingletonByLauncherIdResp, RandomState>(
            &self.client,
            &(self.url_function)(
                self.host.as_str(),
                self.port,
                "get_singleton_by_launcher_id",
            ),
            &request_body,
            &self.additional_headers,
        )
        .await?;
        Ok((resp.coin_record, resp.parent_spend))
    }

    async fn get_coin_records_by_hints(
        &self,
        hints: &[Bytes32],
        include_spent_coins: Option<bool>,
        start_height: Option<u32>,
        end_height: Option<u32>,
    ) -> Result<Vec<CoinRecord>, ChiaRpcError> {
        let mut request_body = Map::new();
        request_body.insert("hints".to_string(), json!(hints));
        if let Some(include_spent_coins) = include_spent_coins {
            request_body.insert(
                "include_spent_coins".to_string(),
                json!(include_spent_coins),
            );
        }
        if let Some(start_height) = start_height {
            request_body.insert("start_height".to_string(), json!(start_height));
        }
        if let Some(end_height) = end_height {
            request_body.insert("end_height".to_string(), json!(end_height));
        }
        Ok(post::<CoinRecordAryResp, RandomState>(
            &self.client,
            &(self.url_function)(self.host.as_str(), self.port, "get_coin_records_by_hints"),
            &request_body,
            &self.additional_headers,
        )
        .await?
        .coin_records)
    }

    async fn get_coin_records_by_hints_paginated(
        &self,
        hints: &[Bytes32],
        include_spent_coins: Option<bool>,
        start_height: Option<u32>,
        end_height: Option<u32>,
        page_size: u32,
        last_id: Option<Bytes32>,
    ) -> Result<(Vec<PaginatedCoinRecord>, Option<Bytes32>, Option<i32>), ChiaRpcError> {
        let mut request_body = Map::new();
        request_body.insert("hints".to_string(), json!(hints));
        request_body.insert(
            "include_spent_coins".to_string(),
            json!(include_spent_coins.unwrap_or(true)),
        );
        if let Some(sh) = start_height {
            request_body.insert("start_height".to_string(), json!(sh));
        }
        if let Some(eh) = end_height {
            request_body.insert("end_height".to_string(), json!(eh));
        }
        request_body.insert("page_size".to_string(), json!(page_size));
        if let Some(li) = last_id {
            request_body.insert("last_id".to_string(), json!(li));
        }
        let resp = post::<PaginatedCoinRecordAryResp, RandomState>(
            &self.client,
            &(self.url_function)(
                self.host.as_str(),
                self.port,
                "get_coin_records_by_hints_paginated",
            ),
            &request_body,
            &self.additional_headers,
        )
        .await?;

        Ok((resp.coin_records, resp.last_id, resp.total_coin_count))
    }

    async fn get_coin_records_by_puzzle_hashes_paginated(
        &self,
        puzzle_hashes: &[Bytes32],
        include_spent_coins: Option<bool>,
        start_height: Option<u32>,
        end_height: Option<u32>,
        page_size: u32,
        last_id: Option<Bytes32>,
    ) -> Result<(Vec<PaginatedCoinRecord>, Option<Bytes32>, Option<i32>), ChiaRpcError> {
        let mut request_body = Map::new();
        request_body.insert("puzzle_hashes".to_string(), json!(puzzle_hashes));
        if let Some(isc) = include_spent_coins {
            request_body.insert("include_spent_coins".to_string(), json!(isc));
        }
        if let Some(sh) = start_height {
            request_body.insert("start_height".to_string(), json!(sh));
        }
        if let Some(eh) = end_height {
            request_body.insert("end_height".to_string(), json!(eh));
        }
        request_body.insert("page_size".to_string(), json!(page_size));
        if let Some(li) = last_id {
            request_body.insert("last_id".to_string(), json!(li));
        }
        let resp = post::<PaginatedCoinRecordAryResp, RandomState>(
            &self.client,
            &(self.url_function)(
                self.host.as_str(),
                self.port,
                "get_coin_records_by_puzzle_hashes_paginated",
            ),
            &request_body,
            &self.additional_headers,
        )
        .await?;

        Ok((resp.coin_records, resp.last_id, resp.total_coin_count))
    }

    async fn get_hints_by_coin_ids(
        &self,
        coin_ids: &[Bytes32],
    ) -> Result<HashMap<Bytes32, Bytes32>, ChiaRpcError> {
        let mut request_body = Map::new();
        request_body.insert("coin_ids".to_string(), json!(coin_ids));
        Ok(post::<CoinHintsResp, RandomState>(
            &self.client,
            &(self.url_function)(self.host.as_str(), self.port, "get_hints_by_coin_ids"),
            &request_body,
            &self.additional_headers,
        )
        .await?
        .coin_id_hints)
    }

    async fn get_puzzles_and_solutions_by_names(
        &self,
        names: &[Bytes32],
        include_spent_coins: Option<bool>,
        start_height: Option<u32>,
        end_height: Option<u32>,
    ) -> Result<HashMap<Bytes32, Option<CoinSpend>>, ChiaRpcError> {
        let mut request_body = Map::new();
        request_body.insert("names".to_string(), json!(names));
        request_body.insert(
            "include_spent_coins".to_string(),
            json!(include_spent_coins.unwrap_or(true)),
        );
        if let Some(sh) = start_height {
            request_body.insert("start_height".to_string(), json!(sh));
        }
        if let Some(eh) = end_height {
            request_body.insert("end_height".to_string(), json!(eh));
        }
        Ok(post::<CoinSpendMapResp, RandomState>(
            &self.client,
            &(self.url_function)(
                self.host.as_str(),
                self.port,
                "get_puzzles_and_solutions_by_names",
            ),
            &request_body,
            &self.additional_headers,
        )
        .await?
        .coin_solutions)
    }
}
