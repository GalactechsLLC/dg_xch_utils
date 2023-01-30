use crate::types::blockchain::coin::Coin;
use crate::types::blockchain::pending_payment::PendingPayment;
use crate::types::blockchain::transaction_record::TransactionRecord;
use crate::types::blockchain::wallet_balance::WalletBalance;
use crate::types::blockchain::wallet_info::WalletInfo;
use crate::types::blockchain::wallet_sync::WalletSync;
use async_trait::async_trait;
use std::io::Error;

#[async_trait]
pub trait WalletAPI {
    async fn log_in(&self, wallet_fingerprint: u32) -> Result<u32, Error>;
    async fn log_in_and_skip(&self, wallet_fingerprint: u32) -> Result<u32, Error>;
    async fn get_wallets(&self) -> Result<Vec<WalletInfo>, Error>;
    async fn get_wallet_balance(&self, wallet_id: u32) -> Result<Vec<WalletBalance>, Error>;
    async fn get_sync_status(&self) -> Result<WalletSync, Error>;
    async fn send_transaction(
        &self,
        wallet_id: u32,
        amount: u64,
        address: String,
        fee: u64,
    ) -> Result<TransactionRecord, Error>;
    async fn send_transaction_multi(
        &self,
        wallet_id: u32,
        additions: Vec<PendingPayment>,
        fee: u64,
    ) -> Result<TransactionRecord, Error>;
    async fn get_transaction(
        &self,
        wallet_id: u32,
        transaction_id: String,
    ) -> Result<TransactionRecord, Error>;
    async fn create_signed_transaction(
        &self,
        wallet_id: u32,
        additions: Vec<Coin>,
        coins: Vec<Coin>,
        fee: u64,
    ) -> Result<TransactionRecord, Error>;
}
