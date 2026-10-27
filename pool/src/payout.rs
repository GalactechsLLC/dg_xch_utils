use crate::accounting::Distribution;
use blst::min_pk::SecretKey;
use dg_xch_core::blockchain::coin::Coin;
use dg_xch_core::blockchain::coin_spend::CoinSpend;
use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes48};
use dg_xch_core::blockchain::spend_bundle::SpendBundle;
use dg_xch_core::clvm::sexp::SExp;
use dg_xch_core::consensus::constants::ConsensusConstants;
use dg_xch_core::traits::SizedBytes;
use dg_xch_puzzles::p2_delegated_puzzle_or_hidden_puzzle::{
    DEFAULT_HIDDEN_PUZZLE_TREE_HASH, calculate_synthetic_secret_key, puzzle_for_pk,
    solution_for_conditions,
};
use std::collections::{BTreeMap, HashMap};
use std::io::Error;

pub async fn sign_payout(
    input: Coin,
    distribution: &Distribution,
    fee_destination: Bytes32,
    key: &SecretKey,
    constants: &ConsensusConstants,
) -> Result<SpendBundle, Error> {
    let puzzle = puzzle_for_pk(key.sk_to_pk().to_bytes().into())?;
    if puzzle.tree_hash() != input.puzzle_hash {
        return Err(Error::other(
            "payout signing key does not control the reward input",
        ));
    }
    let conditions = payout_conditions(input, distribution, fee_destination)?;
    let solution = solution_for_conditions(conditions)?;
    let synthetic = calculate_synthetic_secret_key(key, DEFAULT_HIDDEN_PUZZLE_TREE_HASH)?;
    let public_key = Bytes48::from(synthetic.sk_to_pk().to_bytes());
    dg_xch_wallet::common::sign_coin_spends(
        vec![CoinSpend {
            coin: input,
            puzzle_reveal: puzzle.serialized()?,
            solution: solution.serialized()?,
        }],
        |requested| {
            let result = if *requested == public_key {
                Ok(synthetic.clone())
            } else {
                Err(Error::other("unexpected payout signing key"))
            };
            async move { result }
        },
        HashMap::new(),
        constants.agg_sig_me_additional_data.as_ref(),
        500_000_000,
    )
    .await
}

fn payout_conditions(
    input: Coin,
    distribution: &Distribution,
    fee_destination: Bytes32,
) -> Result<Vec<SExp<'static>>, Error> {
    if (distribution.payouts.is_empty() && distribution.pool_fee == 0)
        || distribution.payouts.len() > 500
    {
        return Err(Error::other(
            "payout batch needs between 1 and 500 destinations",
        ));
    }
    let mut total = distribution
        .transaction_fee
        .checked_add(distribution.pool_fee)
        .ok_or_else(|| Error::other("payout fee overflow"))?;
    let mut destinations = BTreeMap::<[u8; 32], u64>::new();
    for payout in &distribution.payouts {
        if payout.amount == 0
            || destinations
                .insert(payout.puzzle_hash.bytes(), payout.amount)
                .is_some()
        {
            return Err(Error::other("zero payout or duplicate payout destination"));
        }
        total = total
            .checked_add(payout.amount)
            .ok_or_else(|| Error::other("payout amount overflow"))?;
    }
    if total != input.amount {
        return Err(Error::other(
            "payout distribution does not conserve the reward",
        ));
    }
    if distribution.pool_fee != 0 {
        let amount = destinations.entry(fee_destination.bytes()).or_default();
        *amount = amount
            .checked_add(distribution.pool_fee)
            .ok_or_else(|| Error::other("payout fee overflow"))?;
    }
    if destinations.len() > 500 {
        return Err(Error::other("payout exceeds 500 total destinations"));
    }
    let mut conditions: Vec<SExp<'static>> = destinations
        .into_iter()
        .map(|(hash, amount)| {
            SExp::from(vec![
                SExp::from(51u8),
                SExp::from(Bytes32::from(hash)),
                SExp::from(amount),
            ])
        })
        .collect();
    if distribution.transaction_fee != 0 {
        conditions.push(SExp::from(vec![
            SExp::from(52u8),
            SExp::from(distribution.transaction_fee),
        ]));
    }
    Ok(conditions)
}

pub enum PayoutKey {
    Bls(SecretKey),
    #[cfg(feature = "falcon")]
    Falcon(std::sync::Arc<dg_xch_wallet::falcon::FalconWallet>),
}

impl PayoutKey {
    pub async fn sign(
        &self,
        input: Coin,
        distribution: &Distribution,
        destination: Bytes32,
        constants: &ConsensusConstants,
        height: u32,
    ) -> Result<SpendBundle, Error> {
        match self {
            Self::Bls(key) => {
                if !constants.signature_profile.is_bls() {
                    return Err(Error::other("BLS payout key cannot sign on a Falcon chain"));
                }
                let _ = height;
                sign_payout(input, distribution, destination, key, constants).await
            }
            #[cfg(feature = "falcon")]
            Self::Falcon(wallet) => {
                use dg_xch_wallet::signing::TransactionWallet;
                if constants.signature_profile.is_bls() {
                    return Err(Error::other("Falcon payout key cannot sign on a BLS chain"));
                }
                let puzzle = wallet.owner_puzzle(&input.puzzle_hash).await?;
                let conditions = dg_xch_core::clvm::program::Program::to(payout_conditions(
                    input,
                    distribution,
                    destination,
                )?);
                wallet
                    .authorize(
                        vec![CoinSpend {
                            coin: input,
                            puzzle_reveal: puzzle.serialized()?,
                            solution: wallet.conditions_solution(conditions)?.serialized()?,
                        }],
                        height,
                    )
                    .await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounting::{RewardShare, distribute};
    use dg_xch_core::consensus::constants::MAINNET;

    #[cfg(feature = "falcon")]
    #[tokio::test]
    async fn falcon_payout_uses_falcon_authorization_and_conserves_the_distribution() {
        use dg_xch_core::{
            consensus::chain_definition::ChainDefinition, signatures::falcon::FalconSigner,
        };
        let constants = ChainDefinition::falcon_development("Falcon pool payout".into())
            .constants()
            .unwrap();
        let wallet = std::sync::Arc::new(
            dg_xch_wallet::falcon::FalconWallet::new(
                FalconSigner::generate().unwrap(),
                std::sync::Arc::new(constants),
            )
            .unwrap(),
        );
        let target = wallet.puzzle_hash();
        let input = Coin {
            parent_coin_info: [1; 32].into(),
            puzzle_hash: target,
            amount: 100_000,
        };
        let shares = vec![
            RewardShare {
                launcher_id: [2; 32].into(),
                payout_puzzle_hash: [3; 32].into(),
                points: 1,
            },
            RewardShare {
                launcher_id: [4; 32].into(),
                payout_puzzle_hash: [5; 32].into(),
                points: 2,
            },
        ];
        let distribution = distribute(input.amount, 100, 100, &shares).unwrap();
        let key = PayoutKey::Falcon(wallet);
        let bundle = key
            .sign(input, &distribution, target, &constants, 128)
            .await
            .unwrap();
        bundle
            .validate(Some(500_000_000), 0, &constants, false)
            .unwrap();
        assert_eq!(bundle.authorization.as_ref().unwrap().transactions.len(), 1);
        assert_eq!(
            bundle.coin_spends[0].clone().reserved_fee().unwrap(),
            100.into()
        );
        assert_eq!(
            bundle
                .additions()
                .unwrap()
                .iter()
                .map(|c| c.amount)
                .sum::<u64>(),
            input.amount - 100
        );
        let mut invalid = bundle.clone();
        invalid.authorization.as_mut().unwrap().transactions[0][50] ^= 1;
        assert!(
            invalid
                .validate(Some(500_000_000), 0, &constants, false)
                .is_err()
        );
        let mut invalid_distribution = distribution.clone();
        invalid_distribution.payouts[0].amount += 1;
        assert!(
            key.sign(input, &invalid_distribution, target, &constants, 128)
                .await
                .is_err()
        );
        assert!(
            key.sign(input, &distribution, target, &MAINNET, 128)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn signed_payout_conserves_funds_and_validates_signature() {
        let key = SecretKey::key_gen(&[42; 32], &[]).unwrap();
        let input = Coin {
            parent_coin_info: [1; 32].into(),
            puzzle_hash: puzzle_for_pk(key.sk_to_pk().to_bytes().into())
                .unwrap()
                .tree_hash(),
            amount: 100_000,
        };
        let shares = (1..=3)
            .map(|index| RewardShare {
                launcher_id: [index; 32].into(),
                payout_puzzle_hash: [index; 32].into(),
                points: u64::from(index),
            })
            .collect::<Vec<_>>();
        let distribution = distribute(input.amount, 100, 100, &shares).unwrap();
        let bundle = sign_payout(input, &distribution, [1; 32].into(), &key, &MAINNET)
            .await
            .unwrap();
        bundle
            .validate(Some(500_000_000), 0, &MAINNET, false)
            .unwrap();
        let additions = bundle.coin_spends[0]
            .compute_additions_with_cost(500_000_000)
            .unwrap()
            .0;
        assert_eq!(additions.len(), 3);
        assert_eq!(
            additions.iter().map(|coin| coin.amount).sum::<u64>(),
            input.amount - 100
        );
        let mut invalid = distribution;
        invalid.payouts[0].amount += 1;
        assert!(
            sign_payout(input, &invalid, [1; 32].into(), &key, &MAINNET)
                .await
                .is_err()
        );
        let wrong_key = SecretKey::key_gen(&[43; 32], &[]).unwrap();
        assert!(
            sign_payout(input, &invalid, [1; 32].into(), &wrong_key, &MAINNET)
                .await
                .is_err()
        );
        let fee_only = distribute(input.amount, 10_000, 0, &shares).unwrap();
        let fee_bundle = sign_payout(input, &fee_only, input.puzzle_hash, &key, &MAINNET)
            .await
            .unwrap();
        fee_bundle
            .validate(Some(500_000_000), 0, &MAINNET, false)
            .unwrap();
        assert_eq!(fee_bundle.additions().unwrap()[0].amount, input.amount);
    }
}
