use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::traits::SizedBytes;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Error;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RewardShare {
    pub launcher_id: Bytes32,
    pub payout_puzzle_hash: Bytes32,
    pub points: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Payout {
    pub puzzle_hash: Bytes32,
    pub amount: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Distribution {
    pub payouts: Vec<Payout>,
    pub pool_fee: u64,
    pub transaction_fee: u64,
}

pub fn distribute(
    reward: u64,
    pool_fee_basis_points: u16,
    transaction_fee: u64,
    shares: &[RewardShare],
) -> Result<Distribution, Error> {
    if pool_fee_basis_points > 10_000 || shares.is_empty() {
        return Err(Error::other("invalid pool fee or empty reward shares"));
    }
    let mut ordered = BTreeMap::new();
    let mut points = 0u128;
    for share in shares {
        if share.points == 0 || ordered.insert(share.launcher_id.bytes(), share).is_some() {
            return Err(Error::other(
                "zero points or duplicate launcher in reward shares",
            ));
        }
        points = points
            .checked_add(u128::from(share.points))
            .ok_or_else(|| Error::other("total points overflow"))?;
    }
    let pool_fee = u64::try_from(u128::from(reward) * u128::from(pool_fee_basis_points) / 10_000)
        .map_err(Error::other)?;
    let available = reward
        .checked_sub(pool_fee)
        .and_then(|amount| amount.checked_sub(transaction_fee))
        .ok_or_else(|| Error::other("fees exceed collected rewards"))?;
    let mut assigned = 0u64;
    let mut allocations = Vec::with_capacity(ordered.len());
    for (launcher, share) in ordered {
        let weighted = u128::from(available) * u128::from(share.points);
        let amount = u64::try_from(weighted / points).map_err(Error::other)?;
        assigned = assigned
            .checked_add(amount)
            .ok_or_else(|| Error::other("allocation overflow"))?;
        allocations.push((
            weighted % points,
            launcher,
            share.payout_puzzle_hash,
            amount,
        ));
    }
    allocations.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
    let remaining = usize::try_from(available - assigned).map_err(Error::other)?;
    if remaining > allocations.len() {
        return Err(Error::other("inconsistent reward remainder"));
    }
    for allocation in allocations.iter_mut().take(remaining) {
        allocation.3 = allocation
            .3
            .checked_add(1)
            .ok_or_else(|| Error::other("allocation overflow"))?;
    }
    let mut grouped = BTreeMap::<[u8; 32], u64>::new();
    for (_, _, destination, amount) in allocations {
        if amount != 0 {
            let balance = grouped.entry(destination.bytes()).or_default();
            *balance = balance
                .checked_add(amount)
                .ok_or_else(|| Error::other("payout overflow"))?;
        }
    }
    Ok(Distribution {
        payouts: grouped
            .into_iter()
            .map(|(puzzle_hash, amount)| Payout {
                puzzle_hash: puzzle_hash.into(),
                amount,
            })
            .collect(),
        pool_fee,
        transaction_fee,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shares() -> Vec<RewardShare> {
        (1..=3)
            .map(|index| RewardShare {
                launcher_id: [index; 32].into(),
                payout_puzzle_hash: [index; 32].into(),
                points: 1,
            })
            .collect()
    }

    #[test]
    fn payouts_conserve_every_mojo_and_are_order_independent() {
        let mut shares = shares();
        let expected = distribute(101, 100, 2, &shares).unwrap();
        assert_eq!(
            expected
                .payouts
                .iter()
                .map(|payout| payout.amount)
                .sum::<u64>(),
            98
        );
        assert_eq!(expected.pool_fee, 1);
        assert_eq!(expected.payouts[0].amount, 33);
        shares.reverse();
        assert_eq!(distribute(101, 100, 2, &shares).unwrap(), expected);
    }

    #[test]
    fn large_amounts_and_shared_destinations_remain_exact() {
        let mut shares = shares();
        for share in &mut shares {
            share.points = u64::MAX;
            share.payout_puzzle_hash = [9; 32].into();
        }
        let result = distribute(u64::MAX, 0, 0, &shares).unwrap();
        assert_eq!(result.payouts.len(), 1);
        assert_eq!(result.payouts[0].amount, u64::MAX);
        assert!(distribute(1, 10_001, 0, &shares).is_err());
        assert!(distribute(1, 0, 2, &shares).is_err());
        shares.push(shares[0].clone());
        assert!(distribute(100, 0, 0, &shares).is_err());
    }
}
