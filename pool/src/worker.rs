use crate::chain::NodeChain;
use crate::payout::sign_payout;
use crate::service::PoolService;
use crate::store::RewardClaim;
use blst::min_pk::SecretKey;
use dg_xch_clients::rpc::full_node::{FullnodeAPI, FullnodeHelpers};
use dg_xch_core::blockchain::spend_bundle::SpendBundle;
use dg_xch_core::blockchain::tx_status::TXStatus;
use std::collections::HashSet;
use std::io::Error;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub struct RewardWorker {
    pub pool: Arc<PoolService>,
    pub chain: Arc<NodeChain>,
    pub key: SecretKey,
    pub confirmations: u32,
    pub transaction_fee: u64,
}

impl RewardWorker {
    async fn broadcast(&self, bundle: &SpendBundle) -> Result<(), Error> {
        match self.chain.client.push_tx(bundle).await? {
            TXStatus::SUCCESS | TXStatus::PENDING => Ok(()),
            TXStatus::FAILED => Err(Error::other("node rejected pool transaction")),
        }
    }

    pub async fn tick(&self) -> Result<(), Error> {
        self.chain.check_network().await?;
        let peak = self
            .chain
            .client
            .get_blockchain_state()
            .await?
            .peak
            .ok_or_else(|| Error::other("pool node has no peak"))?;
        let claims = self.pool.store.lock().await.claims().await?;
        let mut pending = HashSet::new();
        let mut known = HashSet::new();
        for claim in claims {
            known.insert(claim.reward.name());
            let payout = self
                .chain
                .client
                .get_coin_records_by_names(&[claim.payout_coin.name()], Some(true), None, None)
                .await?
                .into_iter()
                .next();
            let Some(payout) = payout else {
                pending.insert(claim.launcher_id);
                self.broadcast(&claim.bundle).await?;
                continue;
            };
            if payout.coin != claim.payout_coin {
                return Err(Error::other("pool reward output mismatch"));
            }
            if peak.height.saturating_sub(payout.confirmed_block_index) < self.confirmations {
                pending.insert(claim.launcher_id);
                continue;
            }
            let batch = self
                .pool
                .store
                .lock()
                .await
                .batch(payout.coin.name())
                .await?;
            if payout.spent {
                let bundle = batch.and_then(|batch| batch.bundle).ok_or_else(|| {
                    Error::other("pool reward was spent outside the payout journal")
                })?;
                let actual = self.chain.client.get_coin_spend(&payout).await?;
                if bundle.coin_spends.as_slice() != [actual] {
                    return Err(Error::other(
                        "on-chain pool payout differs from the signed journal",
                    ));
                }
                self.pool
                    .store
                    .lock()
                    .await
                    .confirm_batch(payout.coin.name(), payout.spent_block_index)
                    .await?;
                continue;
            }
            let distribution = if let Some(batch) = &batch {
                batch.distribution.clone()
            } else {
                let mut store = self.pool.store.lock().await;
                if !store.has_unpaid_partials(claim.cutoff).await? {
                    continue;
                }
                store
                    .prepare_distribution(
                        payout.coin.name(),
                        payout.coin.amount,
                        self.pool.fee_basis_points,
                        self.transaction_fee,
                        claim.cutoff,
                    )
                    .await?
            };
            let bundle = match batch.and_then(|batch| batch.bundle) {
                Some(bundle) => bundle,
                None => {
                    let bundle = sign_payout(
                        payout.coin,
                        &distribution,
                        self.chain.target,
                        &self.key,
                        &self.chain.constants,
                    )
                    .await?;
                    self.pool
                        .store
                        .lock()
                        .await
                        .save_signed_batch(payout.coin.name(), &bundle)
                        .await?;
                    bundle
                }
            };
            self.broadcast(&bundle).await?;
        }
        let farmers = self.pool.store.lock().await.farmers().await?;
        for farmer in farmers {
            if pending.contains(&farmer.launcher_id) {
                continue;
            }
            let rewards = self
                .chain
                .client
                .get_coin_records_by_puzzle_hash(
                    &farmer.contract_puzzle_hash,
                    Some(false),
                    None,
                    None,
                )
                .await?;
            for reward in rewards {
                if !reward.coinbase
                    || known.contains(&reward.coin.name())
                    || peak.height.saturating_sub(reward.confirmed_block_index) < self.confirmations
                {
                    continue;
                }
                let bundle = self.chain.claim_reward(&farmer, &reward).await?;
                let mut destinations = Vec::new();
                for spend in &bundle.coin_spends {
                    destinations.extend(
                        spend
                            .compute_additions_with_cost(500_000_000)?
                            .0
                            .into_iter()
                            .filter(|coin| {
                                coin.puzzle_hash == self.chain.target
                                    && coin.amount == reward.coin.amount
                            }),
                    );
                }
                if destinations.len() != 1 {
                    return Err(Error::other(
                        "reward claim must pay the pool target exactly once",
                    ));
                }
                let payout_coin = *destinations
                    .first()
                    .ok_or_else(|| Error::other("reward claim has no pool output"))?;
                let cutoff = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_err(Error::other)?
                    .as_secs();
                self.pool
                    .store
                    .lock()
                    .await
                    .save_claim(&RewardClaim {
                        launcher_id: farmer.launcher_id,
                        reward: reward.coin,
                        payout_coin,
                        cutoff,
                        bundle: bundle.clone(),
                    })
                    .await?;
                self.broadcast(&bundle).await?;
                break;
            }
        }
        Ok(())
    }

    pub async fn run(self) {
        loop {
            if let Err(error) = self.tick().await {
                log::warn!("Pool reward processing paused: {error}");
            }
            tokio::time::sleep(Duration::from_secs(10)).await;
        }
    }
}
