mod chain_user;

use crate::simulator::chain_user::ChainUser;
use crate::wallets::memory_wallet::{MemoryWallet, MemoryWalletConfig, MemoryWalletStore};
use crate::wallets::{Wallet, WalletInfo};
use bip39::Mnemonic;
use dg_xch_clients::api::simulator::SimulatorAPI;
use dg_xch_clients::rpc::simulator::SimulatorClient;
use dg_xch_clients::ClientSSLConfig;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::blockchain::wallet_type::WalletType;
use dg_xch_core::consensus::constants::ConsensusConstants;
use dg_xch_keys::{decode_puzzle_hash, key_from_mnemonic};
use lazy_static::lazy_static;
use std::collections::HashMap;
use std::io::{Error, ErrorKind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio::time;

lazy_static! {
    pub static ref UTIL_ADDRESS: Bytes32 =
        decode_puzzle_hash("xch1ye5dzd44kkatnxx2je4s2agpwtqds5lsm5mlyef7plum5danxalq2dnqap")
            .unwrap();
}

pub struct Simulator {
    network: ConsensusConstants,
    client: SimulatorClient,
    run: Arc<AtomicBool>,
    background: Mutex<Option<JoinHandle<()>>>,
}
impl Simulator {
    pub fn new(
        host: &str,
        port: u16,
        timeout: u64,
        ssl_path: Option<ClientSSLConfig>,
        additional_headers: &Option<HashMap<String, String>>,
        network: Option<ConsensusConstants>,
    ) -> Self {
        Self {
            network: network.unwrap_or_default(),
            client: SimulatorClient::new(host, port, timeout, ssl_path, additional_headers),
            run: Arc::new(AtomicBool::new(false)),
            background: Mutex::new(None),
        }
    }
    pub fn client(&self) -> &SimulatorClient {
        &self.client
    }
    pub async fn new_user(&self, name: &str) -> Result<ChainUser<'_>, Error> {
        let mnemonic = Mnemonic::generate(24).map_err(|e| Error::new(ErrorKind::Other, e))?;
        let secret_key =
            key_from_mnemonic(&mnemonic).map_err(|e| Error::new(ErrorKind::Other, e))?;
        Ok(ChainUser {
            simulator: self,
            wallet: MemoryWallet::create_simulator(
                WalletInfo {
                    id: 0,
                    name: format!("{name}'s Wallet"),
                    wallet_type: WalletType::StandardWallet,
                    constants: Arc::new(self.network.clone()),
                    master_sk: secret_key.clone(),
                    wallet_store: Arc::new(Mutex::new(MemoryWalletStore::new(secret_key, 0))),
                    data: "".to_string(),
                },
                MemoryWalletConfig {
                    fullnode_host: self.client.host.clone(),
                    fullnode_port: self.client.port,
                    fullnode_ssl_path: self.client.ssl_path.clone(),
                    additional_headers: self.client.additional_headers.clone(),
                },
            ),
            name: name.to_string(),
        })
    }
    pub async fn next_blocks(&self, blocks: i64, call_per_block: bool) -> Result<(), Error> {
        if call_per_block {
            for _ in 0..blocks {
                self.client.farm_blocks(*UTIL_ADDRESS, 1, true).await?;
            }
        } else {
            self.client.farm_blocks(*UTIL_ADDRESS, blocks, true).await?;
        }
        Ok(())
    }
    pub async fn farm_coins(
        &self,
        address: Bytes32,
        blocks: i64,
        transaction_block: bool,
    ) -> Result<(), Error> {
        self.client
            .farm_blocks(address, blocks, transaction_block)
            .await
            .map(|_| ())
    }
    pub async fn is_auto_farming(&self) -> Result<bool, Error> {
        self.client
            .get_auto_farming()
            .await
            .map(|r| r.auto_farm_enabled)
    }
    pub async fn run(&self, block_interval: Option<Duration>) -> Result<(), Error> {
        let mut block_interval = time::interval(block_interval.unwrap_or(Duration::from_secs(19)));
        let run = self.run.clone();
        if let Some(background) = &mut *self.background.lock().await {
            if run.load(Ordering::Relaxed) {
                return Err(Error::new(
                    ErrorKind::AlreadyExists,
                    "Simulator Already Running",
                ));
            } else {
                run.store(false, Ordering::Relaxed);
                background.await?;
            }
        }
        run.store(true, Ordering::Relaxed);
        *self.background.lock().await = Some(tokio::spawn(async move {
            while run.load(Ordering::Relaxed) {
                block_interval.tick().await;
            }
        }));
        Ok(())
    }
    pub async fn stop(&self) -> Result<(), Error> {
        self.run.store(false, Ordering::Relaxed);
        if let Some(background) = &mut *self.background.lock().await {
            background.await?;
            Ok(())
        } else {
            Err(Error::new(
                ErrorKind::AlreadyExists,
                "Simulator Not Running",
            ))
        }
    }
}
