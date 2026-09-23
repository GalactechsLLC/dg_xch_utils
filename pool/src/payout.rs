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
    if (distribution.payouts.is_empty() && distribution.pool_fee == 0)
        || distribution.payouts.len() > 500
    {
        return Err(Error::other(
            "payout batch needs between 1 and 500 destinations",
        ));
    }
    let puzzle = puzzle_for_pk(key.sk_to_pk().to_bytes().into())?;
    if puzzle.tree_hash() != input.puzzle_hash {
        return Err(Error::other(
            "payout signing key does not control the reward input",
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounting::{RewardShare, distribute};
    use dg_xch_core::consensus::constants::MAINNET;

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
