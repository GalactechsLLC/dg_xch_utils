use crate::api::responses::{
    LoginResp, SignedTransactionRecordResp, TransactionRecordResp, WalletBalanceResp,
    WalletInfoResp, WalletSyncResp,
};
use crate::api::wallet::WalletAPI;
use crate::rpc::{get_client, get_url, post, ChiaRpcError};
use crate::ClientSSLConfig;
use async_trait::async_trait;
use dg_xch_core::blockchain::announcement::Announcement;
use dg_xch_core::blockchain::coin::Coin;
use dg_xch_core::blockchain::transaction_record::TransactionRecord;
use dg_xch_core::blockchain::wallet_balance::WalletBalance;
use dg_xch_core::blockchain::wallet_info::WalletInfo;
use dg_xch_core::blockchain::wallet_sync::WalletSync;
use dg_xch_core::blockchain::wallet_type::AmountWithPuzzleHash;
use reqwest::Client;
use serde_json::{json, Map};
use std::collections::HashMap;
use std::hash::RandomState;

pub struct WalletClient {
    client: Client,
    host: String,
    port: u16,
    additional_headers: Option<HashMap<String, String>>,
}
impl WalletClient {
    #[must_use]
    pub fn new(
        host: &str,
        port: u16,
        timeout: u64,
        ssl_path: &Option<ClientSSLConfig>,
        additional_headers: Option<HashMap<String, String>>,
    ) -> Self {
        WalletClient {
            client: get_client(ssl_path, timeout).unwrap_or_default(),
            host: host.to_string(),
            port,
            additional_headers,
        }
    }
}
#[async_trait]
impl WalletAPI for WalletClient {
    async fn log_in(&self, wallet_fingerprint: u32) -> Result<u32, ChiaRpcError> {
        let mut request_body = Map::new();
        request_body.insert("wallet_fingerprint".to_string(), json!(wallet_fingerprint));
        Ok(post::<LoginResp, RandomState>(
            &self.client,
            &get_url(self.host.as_str(), self.port, "log_in"),
            &request_body,
            &self.additional_headers,
        )
        .await?
        .fingerprint)
    }
    async fn log_in_and_skip(&self, wallet_fingerprint: u32) -> Result<u32, ChiaRpcError> {
        let mut request_body = Map::new();
        request_body.insert("wallet_fingerprint".to_string(), json!(wallet_fingerprint));
        Ok(post::<LoginResp, RandomState>(
            &self.client,
            &get_url(self.host.as_str(), self.port, "log_in_and_skip"),
            &request_body,
            &self.additional_headers,
        )
        .await?
        .fingerprint)
    }
    async fn get_wallets(&self) -> Result<Vec<WalletInfo>, ChiaRpcError> {
        Ok(post::<WalletInfoResp, RandomState>(
            &self.client,
            &get_url(self.host.as_str(), self.port, "get_wallets"),
            &Map::new(),
            &self.additional_headers,
        )
        .await?
        .wallets)
    }
    async fn get_wallet_balance(&self, wallet_id: u32) -> Result<Vec<WalletBalance>, ChiaRpcError> {
        let mut request_body = Map::new();
        request_body.insert("wallet_id".to_string(), json!(wallet_id));
        Ok(post::<WalletBalanceResp, RandomState>(
            &self.client,
            &get_url(self.host.as_str(), self.port, "get_wallet_balance"),
            &request_body,
            &self.additional_headers,
        )
        .await?
        .wallets)
    }
    async fn get_sync_status(&self) -> Result<WalletSync, ChiaRpcError> {
        let resp = post::<WalletSyncResp, RandomState>(
            &self.client,
            &get_url(self.host.as_str(), self.port, "get_sync_status"),
            &Map::new(),
            &self.additional_headers,
        )
        .await?;
        Ok(WalletSync {
            genesis_initialized: resp.genesis_initialized,
            synced: resp.synced,
            syncing: resp.syncing,
        })
    }
    async fn send_transaction(
        &self,
        wallet_id: u32,
        amount: u64,
        address: String,
        fee: u64,
    ) -> Result<TransactionRecord, ChiaRpcError> {
        let mut request_body = Map::new();
        request_body.insert("wallet_id".to_string(), json!(wallet_id));
        request_body.insert("amount".to_string(), json!(amount));
        request_body.insert("address".to_string(), json!(address));
        request_body.insert("fee".to_string(), json!(fee));
        Ok(post::<TransactionRecordResp, RandomState>(
            &self.client,
            &get_url(self.host.as_str(), self.port, "SendTransaction"),
            &request_body,
            &self.additional_headers,
        )
        .await?
        .transaction)
    }
    async fn send_transaction_multi(
        &self,
        wallet_id: u32,
        additions: Vec<AmountWithPuzzleHash>,
        fee: u64,
    ) -> Result<TransactionRecord, ChiaRpcError> {
        let mut request_body = Map::new();
        request_body.insert("wallet_id".to_string(), json!(wallet_id));
        request_body.insert("additions".to_string(), json!(additions));
        request_body.insert("fee".to_string(), json!(fee));
        Ok(post::<TransactionRecordResp, RandomState>(
            &self.client,
            &get_url(self.host.as_str(), self.port, "send_transaction_multi"),
            &request_body,
            &self.additional_headers,
        )
        .await?
        .transaction)
    }
    async fn get_transaction(
        &self,
        wallet_id: u32,
        transaction_id: String,
    ) -> Result<TransactionRecord, ChiaRpcError> {
        let mut request_body = Map::new();
        request_body.insert("wallet_id".to_string(), json!(wallet_id));
        request_body.insert("transaction_id".to_string(), json!(transaction_id));
        Ok(post::<TransactionRecordResp, RandomState>(
            &self.client,
            &get_url(self.host.as_str(), self.port, "get_transaction"),
            &request_body,
            &self.additional_headers,
        )
        .await?
        .transaction)
    }
    async fn create_signed_transaction(
        &self,
        wallet_id: u32,
        additions: Vec<Coin>,
        coins: Vec<Coin>,
        coin_announcements: Vec<Announcement>,
        puzzle_announcements: Vec<Announcement>,
        fee: u64,
    ) -> Result<TransactionRecord, ChiaRpcError> {
        let mut request_body = Map::new();
        request_body.insert("wallet_id".to_string(), json!(wallet_id));
        request_body.insert("additions".to_string(), json!(additions));
        request_body.insert("coins".to_string(), json!(coins));
        request_body.insert("coin_announcements".to_string(), json!(coin_announcements));
        request_body.insert(
            "puzzle_announcements".to_string(),
            json!(puzzle_announcements),
        );
        request_body.insert("fee".to_string(), json!(fee));
        Ok(post::<SignedTransactionRecordResp, RandomState>(
            &self.client,
            &get_url(self.host.as_str(), self.port, "create_signed_transaction"),
            &request_body,
            &self.additional_headers,
        )
        .await?
        .signed_tx)
    }
}
