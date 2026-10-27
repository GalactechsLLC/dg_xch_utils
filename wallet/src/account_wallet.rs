//! Signature-selected backend for the shared persistent wallet session.
use crate::{
    Wallet, WalletStore, accounts::AccountSecret, memory_wallet::MemoryWallet,
    signing::TransactionWallet,
};
use async_trait::async_trait;
use dg_xch_core::{
    blockchain::{
        coin_record::CoinRecord, coin_spend::CoinSpend, sized_bytes::Bytes32,
        spend_bundle::SpendBundle,
    },
    clvm::program::Program,
    consensus::constants::ConsensusConstants,
};
use std::{
    io::Error,
    sync::{Arc, atomic::Ordering},
};

pub(crate) enum AccountWallet {
    Bls(MemoryWallet),
    #[cfg(feature = "falcon")]
    Falcon(FalconAccountWallet),
}
#[cfg(feature = "falcon")]
pub(crate) struct FalconAccountWallet {
    seed: zeroize::Zeroizing<[u8; 64]>,
    constants: Arc<ConsensusConstants>,
    keys: tokio::sync::Mutex<
        std::collections::HashMap<Bytes32, Arc<dg_xch_core::signatures::falcon::FalconSigner>>,
    >,
    derivations: tokio::sync::Mutex<std::collections::HashMap<(u32, bool), Bytes32>>,
    index: std::sync::atomic::AtomicU32,
}
impl AccountWallet {
    pub fn new(
        secret: AccountSecret,
        client: &dg_xch_clients::rpc::full_node::FullnodeClient,
        constants: Arc<ConsensusConstants>,
    ) -> Result<Self, Error> {
        match secret {
            AccountSecret::Bls(secret) => {
                Ok(Self::Bls(MemoryWallet::new(secret, client, constants)?))
            }
            #[cfg(feature = "falcon")]
            AccountSecret::Falcon(seed) => {
                if constants.signature_profile.is_bls() {
                    return Err(Error::other("account signature profile mismatch"));
                }
                Ok(Self::Falcon(FalconAccountWallet {
                    seed,
                    constants,
                    keys: tokio::sync::Mutex::new(Default::default()),
                    derivations: tokio::sync::Mutex::new(Default::default()),
                    index: 0.into(),
                }))
            }
        }
    }
    pub async fn set_index(&self, index: u32) {
        match self {
            Self::Bls(wallet) => wallet
                .wallet_store()
                .lock()
                .await
                .current_index
                .store(index, Ordering::Relaxed),
            #[cfg(feature = "falcon")]
            Self::Falcon(wallet) => wallet.index.store(index, Ordering::Relaxed),
        }
    }
    pub async fn current_index(&self) -> u32 {
        match self {
            Self::Bls(wallet) => wallet.wallet_store().lock().await.current_index(),
            #[cfg(feature = "falcon")]
            Self::Falcon(wallet) => wallet.index.load(Ordering::Relaxed),
        }
    }
    pub async fn restore_coins(&self, coins: Vec<CoinRecord>) {
        match self {
            Self::Bls(wallet) => {
                *wallet
                    .wallet_store()
                    .lock()
                    .await
                    .standard_coins()
                    .lock()
                    .await = coins
            }
            #[cfg(feature = "falcon")]
            Self::Falcon(_) => {}
        }
    }
    pub async fn puzzle_hashes(
        &self,
        start: usize,
        count: usize,
        hardened: bool,
    ) -> Result<Vec<Bytes32>, Error> {
        match self {
            Self::Bls(wallet) => wallet.puzzle_hashes(start, count, hardened).await,
            #[cfg(feature = "falcon")]
            Self::Falcon(wallet) => {
                use dg_xch_core::signatures::{SignatureSigner, falcon::FalconSigner};
                let end = start
                    .checked_add(count)
                    .filter(|end| *end <= 100_000)
                    .ok_or_else(|| Error::other("wallet derivation limit exceeded"))?;
                let mut keys = wallet.keys.lock().await;
                let mut derivations = wallet.derivations.lock().await;
                let mut hashes = Vec::with_capacity(count);
                for index in start..end {
                    if let Some(hash) = derivations.get(&(index as u32, hardened)) {
                        hashes.push(*hash);
                        continue;
                    }
                    let signer = Arc::new(
                        FalconSigner::from_seed(&wallet.seed, 0, index as u32, hardened)
                            .map_err(Error::other)?,
                    );
                    let hash = crate::falcon::puzzle(&signer.public_key()).tree_hash();
                    keys.entry(hash).or_insert(signer);
                    derivations.insert((index as u32, hardened), hash);
                    hashes.push(hash);
                }
                Ok(hashes)
            }
        }
    }
    pub async fn get_puzzle_hash(&self, new: bool) -> Result<Bytes32, Error> {
        match self {
            Self::Bls(wallet) => wallet.get_puzzle_hash(new).await,
            #[cfg(feature = "falcon")]
            Self::Falcon(_) => {
                let mut index = self.current_index().await;
                if new {
                    index = index
                        .checked_add(1)
                        .filter(|i| *i < 100_000)
                        .ok_or_else(|| Error::other("wallet derivation limit exceeded"))?;
                    self.set_index(index).await;
                }
                Ok(self.puzzle_hashes(index as usize, 1, false).await?[0])
            }
        }
    }
    pub async fn payment(
        &self,
        coins: &[CoinRecord],
        destination: Bytes32,
        amount: u64,
        fee: u64,
        height: u32,
    ) -> Result<SpendBundle, Error> {
        #[allow(irrefutable_let_patterns)]
        if let Self::Bls(wallet) = self {
            return wallet
                .generate_simple_signed_transaction(amount, fee, destination)
                .await?
                .spend_bundle
                .ok_or_else(|| Error::other("transaction has no spend bundle"));
        }
        #[cfg(feature = "falcon")]
        {
            use crate::conditions::{Conditions, announcement_id};
            let needed = u128::from(amount) + u128::from(fee);
            let mut total = 0u128;
            let mut inputs = Vec::new();
            for record in coins.iter().filter(|r| !r.spent) {
                self.owner_puzzle(&record.coin.puzzle_hash).await?;
                inputs.push(record.coin);
                total += u128::from(record.coin.amount);
                if total >= needed {
                    break;
                }
            }
            if total < needed {
                return Err(Error::other("insufficient funds"));
            }
            let change = u64::try_from(total - needed).map_err(Error::other)?;
            let mut output =
                Conditions::new()
                    .reserve_fee(fee)
                    .create_coin(destination, amount, None);
            if change > 0 {
                output = output.create_coin(self.get_puzzle_hash(true).await?, change, None);
            }
            let mut spends = Vec::new();
            for (i, coin) in inputs.iter().enumerate() {
                let mut conditions = if i == 0 {
                    output.clone()
                } else {
                    Conditions::new()
                };
                if inputs.len() > 1 {
                    conditions = conditions
                        .create_coin_announcement(b"dgx-wallet-inputs".to_vec())
                        .assert_coin_announcement(announcement_id(
                            inputs[(i + 1) % inputs.len()].name(),
                            b"dgx-wallet-inputs",
                        ));
                }
                spends.push(CoinSpend {
                    coin: *coin,
                    puzzle_reveal: self.owner_puzzle(&coin.puzzle_hash).await?.serialized()?,
                    solution: self
                        .conditions_solution(conditions.program())?
                        .serialized()?,
                });
            }
            self.authorize(spends, height).await
        }
        #[cfg(not(feature = "falcon"))]
        {
            let _ = (coins, height);
            unreachable!()
        }
    }
}
#[async_trait]
impl TransactionWallet for AccountWallet {
    fn consensus_constants(&self) -> &ConsensusConstants {
        match self {
            Self::Bls(w) => w.consensus_constants(),
            #[cfg(feature = "falcon")]
            Self::Falcon(w) => &w.constants,
        }
    }
    async fn owner_puzzle(&self, hash: &Bytes32) -> Result<Program<'static>, Error> {
        match self {
            Self::Bls(w) => w.owner_puzzle(hash).await,
            #[cfg(feature = "falcon")]
            Self::Falcon(w) => {
                use dg_xch_core::signatures::SignatureSigner;
                w.keys
                    .lock()
                    .await
                    .get(hash)
                    .map(|key| crate::falcon::puzzle(&key.public_key()))
                    .ok_or_else(|| Error::other("wallet does not own puzzle"))
            }
        }
    }
    fn conditions_solution(&self, conditions: Program<'static>) -> Result<Program<'static>, Error> {
        match self {
            Self::Bls(w) => w.conditions_solution(conditions),
            #[cfg(feature = "falcon")]
            Self::Falcon(_) => Ok(Program::to(vec![conditions.sexp().to_owned()])),
        }
    }
    async fn authorize(
        &self,
        coin_spends: Vec<CoinSpend>,
        height: u32,
    ) -> Result<SpendBundle, Error> {
        match self {
            Self::Bls(w) => w.authorize(coin_spends, height).await,
            #[cfg(feature = "falcon")]
            Self::Falcon(w) => {
                use dg_xch_core::{
                    consensus::block_generator::conditions_from_spend_bundle,
                    signatures::{
                        SignatureSigner, extension::FalconAuthorization,
                        falcon_condition::transaction_reference,
                    },
                };
                let mut bundle = SpendBundle {
                    coin_spends,
                    ..SpendBundle::empty()
                };
                let conditions = conditions_from_spend_bundle(&bundle, height, &w.constants)
                    .map_err(Error::other)?;
                let keys = w.keys.lock().await;
                let signatures = conditions
                    .falcon_signatures
                    .iter()
                    .map(|r| {
                        let key = keys
                            .values()
                            .find(|key| key.public_key() == r.public_key.const_bytes())
                            .ok_or_else(|| {
                                Error::other("wallet does not own a required signing key")
                            })?;
                        key.sign_message(r.message.as_slice()).map_err(Error::other)
                    })
                    .collect::<Result<Vec<_>, Error>>()?;
                bundle.aggregated_signature = transaction_reference(
                    w.constants.genesis_challenge,
                    &conditions.falcon_signatures,
                );
                bundle.authorization = FalconAuthorization {
                    genesis: w.constants.genesis_challenge,
                    header: Vec::new(),
                    transactions: signatures,
                    labrador: None,
                }
                .into();
                Ok(bundle)
            }
        }
    }
}

#[cfg(all(test, feature = "falcon"))]
mod tests {
    use super::*;
    use dg_xch_core::{
        blockchain::coin::Coin, consensus::constants::MAINNET, signatures::SignatureProfile,
    };
    #[tokio::test]
    async fn falcon_account_recovers_addresses_and_spends_across_derivations() {
        let mut constants = MAINNET;
        constants.signature_profile = SignatureProfile::FnDsa512V2;
        constants.falcon_aggregation = None;
        let constants = Arc::new(constants);
        let make = || {
            AccountWallet::Falcon(FalconAccountWallet {
                seed: zeroize::Zeroizing::new([42; 64]),
                constants: constants.clone(),
                keys: tokio::sync::Mutex::new(Default::default()),
                derivations: tokio::sync::Mutex::new(Default::default()),
                index: 0.into(),
            })
        };
        let wallet = make();
        let hashes = wallet.puzzle_hashes(0, 2, false).await.unwrap();
        assert_eq!(hashes, make().puzzle_hashes(0, 2, false).await.unwrap());
        assert_ne!(hashes[0], hashes[1]);
        assert_ne!(hashes, wallet.puzzle_hashes(0, 2, true).await.unwrap());
        let coins = hashes
            .iter()
            .enumerate()
            .map(|(i, hash)| CoinRecord {
                coin: Coin {
                    parent_coin_info: Bytes32::from([i as u8 + 1; 32]),
                    puzzle_hash: *hash,
                    amount: 100,
                },
                confirmed_block_index: 0,
                spent_block_index: 0,
                coinbase: false,
                timestamp: 0,
                spent: false,
            })
            .collect::<Vec<_>>();
        let bundle = wallet
            .payment(&coins, Bytes32::from([99; 32]), 150, 1, 0)
            .await
            .unwrap();
        let conditions = dg_xch_core::consensus::block_generator::conditions_from_spend_bundle(
            &bundle, 0, &constants,
        )
        .unwrap();
        assert_eq!(conditions.falcon_signatures.len(), 2);
        dg_xch_core::consensus::block_generator::validate_block_authorization(
            &conditions,
            &bundle
                .authorization
                .as_ref()
                .unwrap()
                .transaction_witness()
                .unwrap(),
            &constants,
        )
        .unwrap();
        assert_eq!(wallet.current_index().await, 1);
        assert!(
            wallet
                .payment(&coins[..1], hashes[0], 150, 0, 0)
                .await
                .is_err()
        );
    }
}
