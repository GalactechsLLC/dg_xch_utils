use std::collections::{HashMap, HashSet};
use std::io::Error;
use std::sync::Arc;
use async_trait::async_trait;
use blst::min_pk::SecretKey;
use dg_xch_clients::rpc::full_node::FullnodeClient;
use dg_xch_core::blockchain::coin::Coin;
use dg_xch_core::blockchain::coin_record::CoinRecord;
use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes48};
use crate::wallets::{Wallet, WalletInfo, WalletStore};
use crate::wallets::common::DerivationRecord;

pub struct MemoryWalletConfig {
    pub fullnode_host: String,
    pub fullnode_port: u16,
    pub fullnode_ssl_path: Option<String>,
    pub additional_headers: Option<HashMap<String, String>>,
    pub secret_key: SecretKey
}

pub struct MemoryWalletStore {
    pub master_sk: SecretKey,
    pub current_index: u64,
    pub spent_coins: HashMap<Bytes32, CoinRecord>,
    pub unspent_coins: HashMap<Bytes32, CoinRecord>,
}
impl MemoryWalletStore {
    pub fn new(secret_key: SecretKey, starting_index: u64) -> Self {
        Self {
            master_sk: secret_key,
            current_index: starting_index,
            spent_coins: Default::default(),
            unspent_coins: Default::default(),
        }
    }
}
#[async_trait]
impl WalletStore for MemoryWalletStore {
    fn get_master_sk(&self) -> &SecretKey {
        todo!()
    }

    async fn get_max_send_amount(&self) -> u128 {
        todo!()
    }

    async fn get_confirmed_balance(&self) -> u128 {
        todo!()
    }

    async fn get_unconfirmed_balance(&self) -> u128 {
        todo!()
    }

    async fn get_spendable_balance(&self) -> u128 {
        todo!()
    }

    async fn get_pending_change_balance(&self) -> u128 {
        todo!()
    }

    async fn get_unused_derivation_record(&self) -> Result<DerivationRecord, Error> {
        todo!()
    }

    async fn get_derivation_record(&self) -> Result<DerivationRecord, Error> {
        todo!()
    }

    async fn get_derivation_record_at_index(&self, index: u64) -> Result<DerivationRecord, Error> {
        todo!()
    }

    async fn select_coins(&self, amount: u64, exclude: Option<&[Coin]>, min_coin_amount: Option<u64>, max_coin_amount: Option<u64>, exclude_coin_amounts: Option<&[u64]>) -> Result<HashSet<Coin>, Error> {
        todo!()
    }

    async fn populate_secret_key_for_puzzle_hash(&self, puz_hash: &Bytes32) -> Bytes48 {
        todo!()
    }

    async fn populate_secret_keys_for_coin_spends(&self) -> Bytes48 {
        todo!()
    }

    async fn secret_key_for_public_key(&self, public_key: &Bytes48) -> Result<SecretKey, Error> {
        todo!()
    }
}

pub struct MemoryWallet { //A wallet that is lost on restarts
    name: String,
    info: WalletInfo<MemoryWalletStore>,
    wallet_store: Arc<MemoryWalletStore>,
    fullnode_client: FullnodeClient,
}
impl MemoryWallet {
}
impl Wallet<MemoryWalletStore, MemoryWalletConfig> for MemoryWallet {
    fn create(name: &str, info: WalletInfo<MemoryWalletStore>, config: MemoryWalletConfig) -> Self {
        Self {
            fullnode_client: FullnodeClient::new(&config.fullnode_host, config.fullnode_port, config.fullnode_ssl_path, &config.additional_headers),
            name: name.to_string(),
            info,
            wallet_store: Arc::new(MemoryWalletStore::new(config.secret_key, 0)),
        }
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn wallet_info(&self) -> &WalletInfo<MemoryWalletStore> {
        &self.info
    }

    fn wallet_store(&self) -> Arc<MemoryWalletStore> {
        self.wallet_store.clone()
    }
}