//! The asset builders need puzzles and authorization, not a particular key type.
use crate::{Wallet, WalletStore, memory_wallet::MemoryWallet};
use async_trait::async_trait;
use dg_xch_core::{
    blockchain::{coin_spend::CoinSpend, sized_bytes::Bytes32, spend_bundle::SpendBundle},
    clvm::program::Program,
};
use num_traits::ToPrimitive;
use std::{collections::HashMap, io::Error};

#[async_trait]
pub trait TransactionWallet: Send + Sync {
    async fn owner_puzzle(&self, puzzle_hash: &Bytes32) -> Result<Program<'static>, Error>;
    fn conditions_solution(&self, conditions: Program<'static>) -> Result<Program<'static>, Error>;
    async fn authorize(&self, spends: Vec<CoinSpend>, height: u32) -> Result<SpendBundle, Error>;
}

#[async_trait]
impl TransactionWallet for MemoryWallet {
    async fn owner_puzzle(&self, puzzle_hash: &Bytes32) -> Result<Program<'static>, Error> {
        Ok(self.puzzle_for_puzzle_hash(puzzle_hash).await?.to_owned())
    }

    fn conditions_solution(&self, conditions: Program<'static>) -> Result<Program<'static>, Error> {
        dg_xch_puzzles::p2_delegated_puzzle_or_hidden_puzzle::solution_for_conditions(
            conditions.sexp().to_owned(),
        )
    }

    async fn authorize(&self, spends: Vec<CoinSpend>, _height: u32) -> Result<SpendBundle, Error> {
        let constants = &self.wallet_info().constants;
        if !constants.signature_profile.is_bls() {
            return Err(Error::other("BLS wallet requires BLS chain rules"));
        }
        let store = self.wallet_store();
        crate::common::sign_coin_spends(
            spends,
            |key| {
                let key = *key;
                let store = store.clone();
                async move { store.lock().await.secret_key_for_public_key(&key).await }
            },
            HashMap::new(),
            constants.agg_sig_me_additional_data.as_ref(),
            constants
                .max_block_cost_clvm
                .to_u64()
                .ok_or_else(|| Error::other("invalid network cost limit"))?,
        )
        .await
    }
}
