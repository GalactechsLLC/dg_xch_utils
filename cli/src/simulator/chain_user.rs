use crate::simulator::Simulator;
use crate::wallets::memory_wallet::MemoryWallet;
use crate::wallets::Wallet;
use dg_xch_clients::api::full_node::FullnodeAPI;
use log::info;
use std::io::Error;

pub struct ChainUser<'a> {
    pub simulator: &'a Simulator<'a>,
    pub wallet: MemoryWallet,
    pub name: String,
}
impl<'a> ChainUser<'a> {
    pub async fn refresh_coins(&self) -> Result<(), Error> {
        self.wallet.sync().await.map(|_| ())
    }
    pub async fn farm_coins(&self, num_coins: i64) -> Result<(), Error> {
        self.simulator
            .farm_coins(self.wallet.get_puzzle_hash(false).await?, num_coins, true)
            .await?;
        self.simulator.next_blocks(1, false).await?;
        info!("Syncing Coins After Farming");
        self.refresh_coins().await
    }
    pub async fn send_xch(&self, mojos: u64, receiver: &ChainUser<'a>) -> Result<(), Error> {
        let transaction = self
            .wallet
            .generate_simple_signed_transaction(
                mojos,
                0,
                receiver.wallet.get_puzzle_hash(false).await?,
            )
            .await?;
        self.simulator
            .client()
            .push_tx(&transaction.spend_bundle.unwrap())
            .await?;
        self.simulator.next_blocks(1, false).await?;
        self.refresh_coins().await?;
        receiver.refresh_coins().await
    }
}
