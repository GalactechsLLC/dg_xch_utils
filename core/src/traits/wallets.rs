use std::io::Error;
use async_trait::async_trait;
use blst::min_pk::SecretKey;
use secrets::SecretBox;
use crate::blockchain::coin_record::{CatCoinRecord, CoinRecord};
use crate::blockchain::sized_bytes::{Bytes32, Bytes48};
use crate::puzzles::p2_delegated_puzzle_or_hidden_puzzle::puzzle_hash_for_pk;

pub struct Derivation {
    pub index: u32,
    pub puzzle_hash: Bytes32,
    pub pubkey: Bytes48,
    pub hardened: bool,
}

#[derive(Clone, Copy, Debug)]
pub enum ReadOnlySource {
    Bech32(Bytes32),
    ObserverKey(Bytes48)
}

#[derive(Clone, Copy, Debug)]
pub enum WalletType {
    ReadOnly(ReadOnlySource),
    Master(Bytes32),
    Derivation(Bytes32)
}

#[async_trait]
pub trait Wallet {
    fn name(&self) -> &str;
    fn wallet_type(&self) -> WalletType;
    fn derivations(&self) -> usize;
    async fn standard_coins(&self) -> Vec<CoinRecord>;
    async fn cat_coins(&self) -> Vec<CatCoinRecord>;
    async fn nft_coins(&self) -> Vec<CoinRecord>;
    async fn secret_key(&self, index: u32, hardened: bool) -> SecretBox<Bytes32>;
    async fn save_puzzle_hash_and_public_key(&self, puzzle_hash: Bytes32, pubkey: Bytes48);
    async fn get_confirmed_balance(&self) -> u128;
    async fn get_unconfirmed_balance(&self) -> u128;
    async fn get_pending_change_balance(&self) -> u128;
    async fn get_spendable_balance(&self) -> u128 {
        let unspent: Vec<CoinRecord> = self
            .standard_coins()
            .await
            .iter()
            .filter(|v| !v.spent)
            .copied()
            .collect();
        if unspent.is_empty() {
            0
        } else {
            unspent.iter().map(|v| v.coin.amount as u128).sum()
        }
    }
    async fn get_derivation(
        &self,
        index: u32,
        hardened: bool,
    ) -> Result<Derivation, Error> {
        let wallet_sk = self.secret_key(index, hardened).await;
        let secret_key = *wallet_sk.borrow();
        let secret_key: SecretKey = secret_key.into();
        let pubkey = Bytes48::from(secret_key.sk_to_pk().to_bytes());
        let puzzle_hash = puzzle_hash_for_pk(pubkey)?;
        self.save_puzzle_hash_and_public_key(puzzle_hash, pubkey).await;
        Ok(Derivation {
            index,
            puzzle_hash,
            pubkey,
            hardened,
        })
    }
}