use crate::api::full_node::{FullnodeAPI, FullnodeExtAPI};
use crate::api::responses::{
    BlockCountMetricsResp, CoinHintsResp, CoinSpendMapResp, FeeEstimateResp,
    HintedAdditionsAndRemovalsResp, MempoolItemAryResp, PaginatedCoinRecordAryResp,
};
use async_trait::async_trait;
use dg_xch_core::blockchain::block_record::BlockRecord;
use dg_xch_core::blockchain::blockchain_state::BlockchainState;
use dg_xch_core::blockchain::coin_record::{CoinRecord, HintedCoinRecord};
use dg_xch_core::blockchain::coin_spend::CoinSpend;
use dg_xch_core::blockchain::full_block::FullBlock;
use dg_xch_core::blockchain::mempool_item::MempoolItem;
use dg_xch_core::blockchain::network_info::NetworkInfo;
use dg_xch_core::blockchain::signage_point_or_eos::SignagePointOrEOS;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::blockchain::spend_bundle::SpendBundle;
use dg_xch_core::blockchain::tx_status::TXStatus;
use dg_xch_core::blockchain::unfinished_block::UnfinishedBlock;
use dg_xch_core::protocols::full_node::BlockCountMetrics;
use dg_xch_core::protocols::full_node::FeeEstimate;
use reqwest::Client;
use serde_json::{json, Map};
use std::collections::HashMap;
use std::io::{Error, ErrorKind};

use crate::api::responses::{
    AdditionsAndRemovalsResp, BlockRecordAryResp, BlockRecordResp, BlockchainStateResp,
    CoinRecordAryResp, CoinRecordResp, CoinSpendResp, FullBlockAryResp, FullBlockResp,
    InitialFreezePeriodResp, MempoolItemResp, MempoolItemsResp, MempoolTXResp, NetSpaceResp,
    NetworkInfoResp, SignagePointOrEOSResp, TXResp, UnfinishedBlockAryResp,
};
use crate::rpc::{get_client, get_url, post};

pub struct FullnodeClient {
    client: Client,
    pub host: String,
    pub port: u16,
    pub ssl_path: Option<String>,
    pub additional_headers: Option<HashMap<String, String>>,
}

impl FullnodeClient {
    pub fn new(
        host: &str,
        port: u16,
        ssl_path: Option<String>,
        additional_headers: &Option<HashMap<String, String>>,
    ) -> Self {
        FullnodeClient {
            client: get_client(ssl_path.clone()).unwrap_or_default(),
            host: host.to_string(),
            port,
            ssl_path,
            additional_headers: additional_headers.clone(),
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
            &self.additional_headers,
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
            &self.additional_headers,
        )
        .await?
        .block)
    }
    async fn get_block_count_metrics(&self) -> Result<BlockCountMetrics, Error> {
        Ok(post::<BlockCountMetricsResp>(
            &self.client,
            &get_url(self.host.as_str(), self.port, "get_block_count_metrics"),
            &Map::new(),
            &self.additional_headers,
        )
        .await?
        .metrics)
    }
    async fn get_blocks(
        &self,
        start: u32,
        end: u32,
        exclude_header_hash: bool,
        exclude_reorged: bool,
    ) -> Result<Vec<FullBlock>, Error> {
        let mut request_body = Map::new();
        request_body.insert("start".to_string(), json!(start));
        request_body.insert("end".to_string(), json!(end));
        request_body.insert(
            "exclude_header_hash".to_string(),
            json!(if exclude_header_hash { "True" } else { "False" }),
        );
        request_body.insert(
            "exclude_reorged".to_string(),
            json!(if exclude_reorged { "True" } else { "False" }),
        );
        Ok(post::<FullBlockAryResp>(
            &self.client,
            &get_url(self.host.as_str(), self.port, "get_blocks"),
            &request_body,
            &self.additional_headers,
        )
        .await?
        .blocks)
    }
    async fn get_all_blocks(&self, start: u32, end: u32) -> Result<Vec<FullBlock>, Error> {
        self.get_blocks(start, end, true, false).await
    }
    async fn get_block_record_by_height(&self, height: u32) -> Result<BlockRecord, Error> {
        let mut request_body = Map::new();
        request_body.insert("height".to_string(), json!(height));
        Ok(post::<BlockRecordResp>(
            &self.client,
            &get_url(self.host.as_str(), self.port, "get_block_record_by_height"),
            &request_body,
            &self.additional_headers,
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
            &self.additional_headers,
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
            &self.additional_headers,
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
            &self.additional_headers,
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
            &self.additional_headers,
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
            &self.additional_headers,
        )
        .await?;
        Ok((resp.additions, resp.removals))
    }
    async fn get_initial_freeze_period(&self) -> Result<u64, Error> {
        Ok(post::<InitialFreezePeriodResp>(
            &self.client,
            &get_url(self.host.as_str(), self.port, "get_initial_freeze_period"),
            &Map::new(),
            &self.additional_headers,
        )
        .await?
        .initial_freeze_end_timestamp)
    }
    async fn get_network_info(&self) -> Result<NetworkInfo, Error> {
        let resp = post::<NetworkInfoResp>(
            &self.client,
            &get_url(self.host.as_str(), self.port, "get_network_info"),
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
    ) -> Result<Vec<CoinRecord>, Error> {
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
        Ok(post::<CoinRecordAryResp>(
            &self.client,
            &get_url(
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
    ) -> Result<Vec<CoinRecord>, Error> {
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
        Ok(post::<CoinRecordAryResp>(
            &self.client,
            &get_url(
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
    async fn get_coin_record_by_name(&self, name: &Bytes32) -> Result<Option<CoinRecord>, Error> {
        let mut request_body = Map::new();
        request_body.insert("name".to_string(), json!(name));
        Ok(post::<CoinRecordResp>(
            &self.client,
            &get_url(self.host.as_str(), self.port, "get_coin_record_by_name"),
            &request_body,
            &self.additional_headers,
        )
        .await?
        .coin_record)
    }
    async fn get_coin_record_by_names(
        &self,
        name: &[Bytes32],
        include_spent_coins: bool,
        start_height: u32,
        end_height: u32,
    ) -> Result<Vec<CoinRecord>, Error> {
        let mut request_body = Map::new();
        request_body.insert("names".to_string(), json!(name));
        request_body.insert(
            "include_spent_coins".to_string(),
            json!(include_spent_coins),
        );
        request_body.insert("start_height".to_string(), json!(start_height));
        request_body.insert("end_height".to_string(), json!(end_height));
        Ok(post::<CoinRecordAryResp>(
            &self.client,
            &get_url(self.host.as_str(), self.port, "get_coin_record_by_names"),
            &request_body,
            &self.additional_headers,
        )
        .await?
        .coin_records)
    }
    async fn get_coin_records_by_parent_ids(
        &self,
        parent_ids: &[Bytes32],
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
            &self.additional_headers,
        )
        .await?
        .coin_records)
    }
    async fn get_coin_records_by_hint(
        &self,
        hint: &Bytes32,
        include_spent_coins: bool,
        start_height: u32,
        end_height: u32,
    ) -> Result<Vec<CoinRecord>, Error> {
        let mut request_body = Map::new();
        request_body.insert("hint".to_string(), json!(hint));
        request_body.insert(
            "include_spent_coins".to_string(),
            json!(include_spent_coins),
        );
        request_body.insert("start_height".to_string(), json!(start_height));
        request_body.insert("end_height".to_string(), json!(end_height));
        Ok(post::<CoinRecordAryResp>(
            &self.client,
            &get_url(self.host.as_str(), self.port, "get_coin_records_by_hint"),
            &request_body,
            &self.additional_headers,
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
            &self.additional_headers,
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
            &self.additional_headers,
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
            &self.additional_headers,
        )
        .await?
        .tx_ids)
    }
    async fn get_all_mempool_items(&self) -> Result<HashMap<String, MempoolItem>, Error> {
        Ok(post::<MempoolItemsResp>(
            &self.client,
            &get_url(self.host.as_str(), self.port, "get_all_mempool_items"),
            &Map::new(),
            &self.additional_headers,
        )
        .await?
        .mempool_items)
    }
    async fn get_mempool_item_by_tx_id(&self, tx_id: &str) -> Result<MempoolItem, Error> {
        let mut request_body = Map::new();
        request_body.insert("tx_id".to_string(), json!(tx_id));
        Ok(post::<MempoolItemResp>(
            &self.client,
            &get_url(self.host.as_str(), self.port, "get_mempool_item_by_tx_id"),
            &request_body,
            &self.additional_headers,
        )
        .await?
        .mempool_item)
    }
    async fn get_mempool_items_by_coin_name(
        &self,
        coin_name: &Bytes32,
    ) -> Result<Vec<MempoolItem>, Error> {
        let mut request_body = Map::new();
        request_body.insert("coin_name".to_string(), json!(coin_name));
        Ok(post::<MempoolItemAryResp>(
            &self.client,
            &get_url(
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
        target_times: &[u64],
    ) -> Result<FeeEstimate, Error> {
        let mut request_body = Map::new();
        request_body.insert("target_times".to_string(), json!(target_times));
        request_body.insert("cost".to_string(), json!(cost));
        Ok(post::<FeeEstimateResp>(
            &self.client,
            &get_url(
                self.host.as_str(),
                self.port,
                "get_mempool_items_by_coin_name",
            ),
            &request_body,
            &self.additional_headers,
        )
        .await?
        .fee_estimate)
    }
}

#[async_trait]
impl FullnodeExtAPI for FullnodeClient {
    async fn get_additions_and_removals_with_hints(
        &self,
        header_hash: &Bytes32,
    ) -> Result<(Vec<HintedCoinRecord>, Vec<HintedCoinRecord>), Error> {
        let mut request_body = Map::new();
        request_body.insert("header_hash".to_string(), json!(header_hash));
        let resp = post::<HintedAdditionsAndRemovalsResp>(
            &self.client,
            &get_url(
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

    async fn get_coin_records_by_hints(
        &self,
        hints: &[Bytes32],
        include_spent_coins: bool,
        start_height: u32,
        end_height: u32,
    ) -> Result<Vec<CoinRecord>, Error> {
        let mut request_body = Map::new();
        request_body.insert("hints".to_string(), json!(hints));
        request_body.insert(
            "include_spent_coins".to_string(),
            json!(include_spent_coins),
        );
        request_body.insert("start_height".to_string(), json!(start_height));
        request_body.insert("end_height".to_string(), json!(end_height));
        Ok(post::<CoinRecordAryResp>(
            &self.client,
            &get_url(self.host.as_str(), self.port, "get_coin_records_by_hints"),
            &request_body,
            &self.additional_headers,
        )
        .await?
        .coin_records)
    }

    async fn get_coin_records_by_puzzle_hashes_paginated(
        &self,
        puzzle_hashes: &[Bytes32],
        include_spent_coins: Option<bool>,
        start_height: Option<u32>,
        end_height: Option<u32>,
        page_size: Option<u32>,
        last_id: Option<Bytes32>,
    ) -> Result<(Vec<CoinRecord>, Option<Bytes32>, Option<i32>), Error> {
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
        if let Some(ps) = page_size {
            request_body.insert("page_size".to_string(), json!(ps));
        }
        if let Some(li) = last_id {
            request_body.insert("last_id".to_string(), json!(li));
        }
        let resp = post::<PaginatedCoinRecordAryResp>(
            &self.client,
            &get_url(
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
    ) -> Result<HashMap<Bytes32, Bytes32>, Error> {
        let mut request_body = Map::new();
        request_body.insert("coin_ids".to_string(), json!(coin_ids));
        Ok(post::<CoinHintsResp>(
            &self.client,
            &get_url(self.host.as_str(), self.port, "get_hints_by_coin_ids"),
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
    ) -> Result<HashMap<Bytes32, CoinSpend>, Error> {
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
        Ok(post::<CoinSpendMapResp>(
            &self.client,
            &get_url(
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

#[tokio::test]
async fn test_extended_functions() {
    let fnc = FullnodeClient::new(
        "localhost",
        8555,
        Some("~/.chia/mainnet/config/ssl".to_string()),
        &None,
    );
    fnc.get_blockchain_state().await.unwrap();
    let (additions, _removals) = fnc
        .get_additions_and_removals_with_hints(&Bytes32::from(
            "0x499c034d9761ab329c0ce293006a55628bb9ea62cae3836901628f6a1afb0031",
        ))
        .await
        .unwrap();
    let mut hints = vec![];
    let mut puz_hashes = vec![];
    let mut coin_ids = vec![];
    for add in additions {
        if let Some(hint) = add.hint {
            hints.push(hint);
            puz_hashes.push(add.coin.puzzle_hash);
            coin_ids.push(add.coin.coin_id());
        }
    }
    let coin_hints = fnc.get_hints_by_coin_ids(&coin_ids).await.unwrap();
    for h in &hints {
        assert!(coin_hints.values().any(|v| v == h));
    }
    let by_hints = fnc
        .get_coin_records_by_hints(&hints, true, 4540000, 4542825)
        .await
        .unwrap();
    assert!(!by_hints.is_empty());
    let by_puz = fnc
        .get_coin_records_by_puzzle_hashes_paginated(
            &puz_hashes,
            Some(true),
            Some(4540000),
            Some(4542825),
            Some(2),
            None,
        )
        .await
        .unwrap();
    assert!(!by_puz.0.is_empty());
    assert!(by_puz.0.iter().all(|v| by_hints.contains(v)));
    assert!(!fnc
        .get_puzzles_and_solutions_by_names(&coin_ids, Some(true), Some(4540000), Some(4542825))
        .await
        .unwrap()
        .is_empty());
}
