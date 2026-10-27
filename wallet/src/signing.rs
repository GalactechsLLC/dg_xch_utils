//! The asset builders need puzzles and authorization, not a particular key type.
use crate::{Wallet, WalletStore, memory_wallet::MemoryWallet};
use async_trait::async_trait;
use dg_xch_core::{
    blockchain::{coin_spend::CoinSpend, sized_bytes::Bytes32, spend_bundle::SpendBundle},
    clvm::program::Program,
};
use num_traits::ToPrimitive;
use std::{collections::HashMap, io::Error};

/// Finalize authorization according to the selected chain, before exporting,
/// persisting, or broadcasting wallet transactions. ZK failures never downgrade.
pub async fn finalize_authorization(
    bundle: SpendBundle,
    constants: &dg_xch_core::consensus::constants::ConsensusConstants,
    height: u32,
) -> Result<SpendBundle, Error> {
    if constants.falcon_aggregation
        == Some(dg_xch_core::signatures::labrador::LabradorProfile::Packed11ZkV2)
    {
        #[cfg(feature = "labrador")]
        return crate::falcon::prove_zk(bundle, constants, height).await;
        #[cfg(not(feature = "labrador"))]
        return Err(Error::other(
            "wallet ZK requires a build with the labrador feature",
        ));
    }
    let _ = height;
    Ok(bundle)
}

#[async_trait]
pub trait TransactionWallet: Send + Sync {
    fn consensus_constants(&self) -> &dg_xch_core::consensus::constants::ConsensusConstants;
    async fn owner_puzzle(&self, puzzle_hash: &Bytes32) -> Result<Program<'static>, Error>;
    fn conditions_solution(&self, conditions: Program<'static>) -> Result<Program<'static>, Error>;
    async fn authorize(&self, spends: Vec<CoinSpend>, height: u32) -> Result<SpendBundle, Error>;
}

#[async_trait]
impl TransactionWallet for MemoryWallet {
    fn consensus_constants(&self) -> &dg_xch_core::consensus::constants::ConsensusConstants {
        &self.wallet_info().constants
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use dg_xch_core::{
        consensus::constants::MAINNET,
        signatures::{SignatureProfile, labrador::LabradorProfile},
    };

    #[tokio::test]
    async fn finalization_preserves_non_zk_profiles_and_rejects_missing_zk_authorization() {
        let bundle = SpendBundle::empty();
        assert_eq!(
            finalize_authorization(bundle.clone(), &MAINNET, 0)
                .await
                .unwrap(),
            bundle
        );
        let mut constants = MAINNET;
        constants.signature_profile = SignatureProfile::FnDsa512V2;
        for aggregation in [None, Some(LabradorProfile::Packed11V2)] {
            constants.falcon_aggregation = aggregation;
            assert_eq!(
                finalize_authorization(bundle.clone(), &constants, 0)
                    .await
                    .unwrap(),
                bundle
            );
        }
        constants.falcon_aggregation = Some(LabradorProfile::Packed11ZkV2);
        assert!(finalize_authorization(bundle, &constants, 0).await.is_err());
    }
}
