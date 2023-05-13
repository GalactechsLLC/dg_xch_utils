use crate::clients::api::wallet::WalletAPI;
use crate::types::blockchain::coin::Coin;
use crate::types::blockchain::pending_payment::PendingPayment;
use crate::types::blockchain::transaction_record::TransactionRecord;
use crate::types::blockchain::wallet_balance::WalletBalance;
use crate::types::blockchain::wallet_info::WalletInfo;
use crate::types::blockchain::wallet_sync::WalletSync;
use async_trait::async_trait;
use reqwest::Client;
use serde_json::{json, Map};
use std::collections::HashMap;
use std::io::Error;

use crate::clients::api::responses::{
    LoginResp, SignedTransactionRecordResp, TransactionRecordResp, WalletBalanceResp,
    WalletInfoResp, WalletSyncResp,
};
use crate::clients::rpc::{get_client, get_url, post};

pub struct WalletClient {
    client: Client,
    host: String,
    port: u16,
    additional_headers: Option<HashMap<String, String>>,
}
impl WalletClient {
    pub fn new(
        host: &str,
        port: u16,
        ssl_path: Option<String>,
        additional_headers: Option<HashMap<String, String>>,
    ) -> Self {
        WalletClient {
            client: get_client(ssl_path).unwrap_or_default(),
            host: host.to_string(),
            port,
            additional_headers,
        }
    }
}
#[async_trait]
impl WalletAPI for WalletClient {
    async fn log_in(&self, wallet_fingerprint: u32) -> Result<u32, Error> {
        let mut request_body = Map::new();
        request_body.insert("wallet_fingerprint".to_string(), json!(wallet_fingerprint));
        Ok(post::<LoginResp>(
            &self.client,
            &get_url(self.host.as_str(), self.port, "log_in"),
            &request_body,
            &self.additional_headers,
        )
        .await?
        .fingerprint)
    }
    async fn log_in_and_skip(&self, wallet_fingerprint: u32) -> Result<u32, Error> {
        let mut request_body = Map::new();
        request_body.insert("wallet_fingerprint".to_string(), json!(wallet_fingerprint));
        Ok(post::<LoginResp>(
            &self.client,
            &get_url(self.host.as_str(), self.port, "log_in_and_skip"),
            &request_body,
            &self.additional_headers,
        )
        .await?
        .fingerprint)
    }
    async fn get_wallets(&self) -> Result<Vec<WalletInfo>, Error> {
        Ok(post::<WalletInfoResp>(
            &self.client,
            &get_url(self.host.as_str(), self.port, "get_wallets"),
            &Map::new(),
            &self.additional_headers,
        )
        .await?
        .wallets)
    }
    async fn get_wallet_balance(&self, wallet_id: u32) -> Result<Vec<WalletBalance>, Error> {
        let mut request_body = Map::new();
        request_body.insert("wallet_id".to_string(), json!(wallet_id));
        Ok(post::<WalletBalanceResp>(
            &self.client,
            &get_url(self.host.as_str(), self.port, "get_wallet_balance"),
            &request_body,
            &self.additional_headers,
        )
        .await?
        .wallets)
    }
    async fn get_sync_status(&self) -> Result<WalletSync, Error> {
        let resp = post::<WalletSyncResp>(
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
    ) -> Result<TransactionRecord, Error> {
        let mut request_body = Map::new();
        request_body.insert("wallet_id".to_string(), json!(wallet_id));
        request_body.insert("amount".to_string(), json!(amount));
        request_body.insert("address".to_string(), json!(address));
        request_body.insert("fee".to_string(), json!(fee));
        Ok(post::<TransactionRecordResp>(
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
        additions: Vec<PendingPayment>,
        fee: u64,
    ) -> Result<TransactionRecord, Error> {
        let mut request_body = Map::new();
        request_body.insert("wallet_id".to_string(), json!(wallet_id));
        request_body.insert("additions".to_string(), json!(additions));
        request_body.insert("fee".to_string(), json!(fee));
        Ok(post::<TransactionRecordResp>(
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
    ) -> Result<TransactionRecord, Error> {
        let mut request_body = Map::new();
        request_body.insert("wallet_id".to_string(), json!(wallet_id));
        request_body.insert("transaction_id".to_string(), json!(transaction_id));
        Ok(post::<TransactionRecordResp>(
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
        fee: u64,
    ) -> Result<TransactionRecord, Error> {
        let mut request_body = Map::new();
        request_body.insert("wallet_id".to_string(), json!(wallet_id));
        request_body.insert("additions".to_string(), json!(additions));
        request_body.insert("coins".to_string(), json!(coins));
        request_body.insert("fee".to_string(), json!(fee));
        Ok(post::<SignedTransactionRecordResp>(
            &self.client,
            &get_url(self.host.as_str(), self.port, "create_signed_transaction"),
            &request_body,
            &self.additional_headers,
        )
        .await?
        .signed_tx)
    }
}
