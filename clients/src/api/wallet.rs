use crate::rpc::ChiaRpcError;
use async_trait::async_trait;
use dg_xch_core::blockchain::announcement::Announcement;
use dg_xch_core::blockchain::coin::Coin;
use dg_xch_core::blockchain::transaction_record::TransactionRecord;
use dg_xch_core::blockchain::wallet_balance::WalletBalance;
use dg_xch_core::blockchain::wallet_info::WalletInfo;
use dg_xch_core::blockchain::wallet_sync::WalletSync;
use dg_xch_core::blockchain::wallet_type::AmountWithPuzzleHash;

#[async_trait]
pub trait WalletAPI {
    async fn log_in(&self, wallet_fingerprint: u32) -> Result<u32, ChiaRpcError>;
    async fn log_in_and_skip(&self, wallet_fingerprint: u32) -> Result<u32, ChiaRpcError>;
    async fn get_wallets(&self) -> Result<Vec<WalletInfo>, ChiaRpcError>;
    async fn get_wallet_balance(&self, wallet_id: u32) -> Result<Vec<WalletBalance>, ChiaRpcError>;
    async fn get_sync_status(&self) -> Result<WalletSync, ChiaRpcError>;
    async fn send_transaction(
        &self,
        wallet_id: u32,
        amount: u64,
        address: String,
        fee: u64,
    ) -> Result<TransactionRecord, ChiaRpcError>;
    async fn send_transaction_multi(
        &self,
        wallet_id: u32,
        additions: Vec<AmountWithPuzzleHash>,
        fee: u64,
    ) -> Result<TransactionRecord, ChiaRpcError>;
    async fn get_transaction(
        &self,
        wallet_id: u32,
        transaction_id: String,
    ) -> Result<TransactionRecord, ChiaRpcError>;
    async fn create_signed_transaction(
        &self,
        wallet_id: u32,
        additions: Vec<Coin>,
        coins: Vec<Coin>,
        coin_announcements: Vec<Announcement>,
        puzzle_announcements: Vec<Announcement>,
        fee: u64,
    ) -> Result<TransactionRecord, ChiaRpcError>;
}
