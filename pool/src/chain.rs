use crate::service::{Membership, PoolChain};
use crate::store::{Farmer, PoolVersion};
use crate::verification::{PartialContext, verify_partial};
use async_trait::async_trait;
use dg_xch_clients::rpc::full_node::{FullnodeAPI, FullnodeClient, FullnodeHelpers};
use dg_xch_core::blockchain::coin_record::CoinRecord;
use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes48};
use dg_xch_core::blockchain::spend_bundle::SpendBundle;
use dg_xch_core::clvm::program::SerializedProgram;
use dg_xch_core::consensus::constants::ConsensusConstants;
use dg_xch_core::protocols::pool::PostPartialPayload;
use dg_xch_puzzles::clvm_puzzles::{launcher_id_to_p2_puzzle_hash, validate_puzzle_hash};
use std::io::Error;
use std::sync::Arc;
use std::time::Duration;

pub struct NodeChain {
    pub client: Arc<FullnodeClient>,
    pub constants: ConsensusConstants,
    pub trusted_genesis: Bytes32,
    pub target: Bytes32,
    pub relative_lock_height: u32,
    pub pool_memoization: SerializedProgram,
    pub partial_time_limit: u32,
}

impl NodeChain {
    pub fn v2_definition(
        &self,
        launcher: Bytes32,
        key: Bytes48,
    ) -> dg_xch_puzzles::pool_v2::PlotNft {
        dg_xch_puzzles::pool_v2::PlotNft {
            launcher_id: launcher,
            genesis_challenge: self.constants.genesis_challenge,
            synthetic_public_key: key,
            exiting: false,
            pool: Some(dg_xch_puzzles::pool_v2::PoolConfig {
                target: self.target,
                relative_lock_height: self.relative_lock_height,
                memoization: self.pool_memoization.clone(),
            }),
        }
    }

    pub async fn claim_reward(
        &self,
        farmer: &Farmer,
        reward: &CoinRecord,
    ) -> Result<SpendBundle, Error> {
        self.membership(
            farmer.version,
            farmer.launcher_id,
            farmer.authentication_public_key,
        )
        .await?;
        let parent = reward.coin.parent_coin_info;
        let height = u32::from_be_bytes(parent[28..32].try_into().map_err(Error::other)?);
        if !reward.coinbase
            || reward.spent
            || height == 0
            || reward.coin.puzzle_hash != farmer.contract_puzzle_hash
            || parent
                != dg_xch_core::consensus::coinbase::pool_parent_id(
                    height,
                    self.constants.genesis_challenge,
                )
            || reward.coin.amount != self.constants.rewards.pool_reward(height)
        {
            return Err(Error::other(
                "coin is not an unspent pool reward for this farmer",
            ));
        }
        let spends = if farmer.version == PoolVersion::V1 {
            let plot = tokio::time::timeout(
                Duration::from_secs(10),
                dg_xch_wallet::plotnft_utils::get_plotnft_by_launcher_id(
                    self.client.clone(),
                    farmer.launcher_id,
                    None,
                ),
            )
            .await
            .map_err(|_| Error::other("PlotNFT lookup timed out"))??
            .ok_or_else(|| Error::other("PlotNFT missing"))?;
            let launcher = self
                .client
                .get_coin_record_by_name(&farmer.launcher_id)
                .await?
                .ok_or_else(|| Error::other("PlotNFT launcher missing"))?;
            let parent = self
                .client
                .get_coin_record_by_name(&plot.singleton_coin.coin.parent_coin_info)
                .await?
                .ok_or_else(|| Error::other("PlotNFT parent missing"))?;
            let spend = self.client.get_coin_spend(&parent).await?;
            dg_xch_puzzles::clvm_puzzles::create_absorb_spend_with_reward(
                &spend,
                &plot.pool_state,
                launcher.coin,
                height,
                self.constants.genesis_challenge,
                u64::try_from(plot.delay_time).map_err(Error::other)?,
                plot.delay_puzzle_hash,
                reward.coin.amount,
            )?
        } else {
            let definition =
                self.v2_definition(farmer.launcher_id, farmer.authentication_public_key);
            let records = self
                .client
                .get_coin_records_by_puzzle_hash(
                    &definition.puzzle()?.tree_hash(),
                    Some(false),
                    None,
                    None,
                )
                .await?;
            if records.len() != 1 {
                return Err(Error::other("expected one active v2 PlotNFT"));
            }
            let singleton = records
                .first()
                .ok_or_else(|| Error::other("PlotNFT missing"))?;
            let parent = self
                .client
                .get_coin_record_by_name(&singleton.coin.parent_coin_info)
                .await?
                .ok_or_else(|| Error::other("PlotNFT parent missing"))?;
            definition.claim_reward(
                singleton.coin,
                &self.client.get_coin_spend(&parent).await?,
                reward.coin,
                height,
            )?
        };
        let bundle = SpendBundle {
            coin_spends: spends,
            ..SpendBundle::default()
        };
        bundle.validate(Some(500_000_000), 0, &self.constants, false)?;
        Ok(bundle)
    }

    pub async fn check_network(&self) -> Result<(), Error> {
        let genesis = self
            .client
            .get_block_record_by_height(0)
            .await
            .map_err(Error::from)?;
        if genesis.header_hash != self.trusted_genesis {
            return Err(Error::other(
                "pool node does not match the trusted genesis header",
            ));
        }
        let state = self
            .client
            .get_blockchain_state()
            .await
            .map_err(Error::from)?;
        if !state.sync.synced || state.sync.sync_mode || state.peak.is_none() {
            return Err(Error::other("pool node is not synchronized"));
        }
        Ok(())
    }

    async fn v1_membership(&self, launcher: Bytes32) -> Result<Membership, Error> {
        let launcher_record = self
            .client
            .get_coin_record_by_name(&launcher)
            .await
            .map_err(Error::from)?
            .ok_or_else(|| Error::other("PlotNFT launcher is missing"))?;
        if !launcher_record.spent
            || launcher_record.coin.amount != 1
            || launcher_record.coin.puzzle_hash
                != dg_xch_puzzles::singleton_launcher::SINGLETON_LAUNCHER_TREE_HASH
        {
            return Err(Error::other("invalid PlotNFT launcher"));
        }
        let plot = tokio::time::timeout(
            Duration::from_secs(10),
            dg_xch_wallet::plotnft_utils::get_plotnft_by_launcher_id(
                self.client.clone(),
                launcher,
                None,
            ),
        )
        .await
        .map_err(|_| Error::other("PlotNFT lookup timed out"))??
        .ok_or_else(|| Error::other("PlotNFT was not found"))?;
        let delay = u64::try_from(plot.delay_time).map_err(Error::other)?;
        if delay < 3600
            || plot.singleton_coin.spent
            || plot.singleton_coin.coin.amount != 1
            || plot.pool_state.version != 1
            || plot.pool_state.state != 3
            || plot.pool_state.target_puzzle_hash != self.target
            || plot.pool_state.relative_lock_height != self.relative_lock_height
            || !validate_puzzle_hash(
                launcher,
                plot.delay_puzzle_hash,
                delay,
                &plot.pool_state,
                plot.singleton_coin.coin.puzzle_hash,
                self.constants.genesis_challenge,
            )?
        {
            return Err(Error::other("PlotNFT is not actively pooling to this pool"));
        }
        Ok(Membership {
            owner_public_key: plot.pool_state.owner_pubkey,
            contract_puzzle_hash: launcher_id_to_p2_puzzle_hash(
                launcher,
                delay,
                plot.delay_puzzle_hash,
            )?,
        })
    }

    async fn v2_membership(&self, launcher: Bytes32, key: Bytes48) -> Result<Membership, Error> {
        let definition = self.v2_definition(launcher, key);
        let hash = definition.puzzle()?.tree_hash();
        let records = self
            .client
            .get_coin_records_by_puzzle_hash(&hash, Some(false), None, None)
            .await
            .map_err(Error::from)?;
        if records.len() != 1 {
            return Err(Error::other("expected exactly one active v2 PlotNFT"));
        }
        let record = records
            .first()
            .ok_or_else(|| Error::other("v2 PlotNFT is missing"))?;
        if record.spent || record.coin.amount != 1 || record.coin.puzzle_hash != hash {
            return Err(Error::other("invalid v2 PlotNFT coin"));
        }
        let parent_record = self
            .client
            .get_coin_record_by_name(&record.coin.parent_coin_info)
            .await
            .map_err(Error::from)?
            .ok_or_else(|| Error::other("missing v2 PlotNFT parent"))?;
        if !parent_record.spent || parent_record.spent_block_index != record.confirmed_block_index {
            return Err(Error::other("invalid v2 PlotNFT parent confirmation"));
        }
        let parent = self
            .client
            .get_coin_spend(&parent_record)
            .await
            .map_err(Error::from)?;
        if parent.coin != parent_record.coin
            || parent.puzzle_reveal.to_bytes().len() > 256 * 1024
            || parent.solution.to_bytes().len() > 256 * 1024
        {
            return Err(Error::other("invalid v2 PlotNFT parent spend"));
        }
        let puzzle = parent.puzzle_reveal.to_program()?;
        let (module, arguments) = puzzle.uncurry()?;
        let arguments = arguments.as_list();
        if module.tree_hash()
            != dg_xch_puzzles::singleton_top_layer_v1_1::SINGLETON_TOP_LAYER_V1_1_TREE_HASH
            || puzzle.tree_hash() != parent.coin.puzzle_hash
            || arguments.len() != 2
            || arguments.first().map(|argument| argument.tree_hash())
                != Some(dg_xch_puzzles::pool_v2::singleton_struct(launcher).tree_hash())
            || !parent
                .compute_additions_with_cost(500_000_000)?
                .0
                .contains(&record.coin)
        {
            return Err(Error::other("invalid v2 PlotNFT singleton lineage"));
        }
        Ok(Membership {
            owner_public_key: key,
            contract_puzzle_hash: dg_xch_puzzles::pool_v2::reward_puzzle(launcher)?.tree_hash(),
        })
    }
}

#[async_trait]
impl PoolChain for NodeChain {
    async fn membership(
        &self,
        version: PoolVersion,
        launcher: Bytes32,
        authentication_key: Bytes48,
    ) -> Result<Membership, Error> {
        self.check_network().await?;
        match version {
            PoolVersion::V1 => self.v1_membership(launcher).await,
            PoolVersion::V2 => self.v2_membership(launcher, authentication_key).await,
        }
    }

    async fn partial(
        &self,
        payload: &PostPartialPayload,
        farmer: &Farmer,
        now: u64,
    ) -> Result<Bytes32, Error> {
        self.check_network().await?;
        let state = self
            .client
            .get_blockchain_state()
            .await
            .map_err(Error::from)?;
        let peak = state.peak.ok_or_else(|| Error::other("node has no peak"))?;
        let signage = self
            .client
            .get_recent_signage_point_or_eos(
                (!payload.end_of_sub_slot).then_some(&payload.sp_hash),
                payload.end_of_sub_slot.then_some(&payload.sp_hash),
            )
            .await
            .map_err(Error::from)?;
        verify_partial(
            payload,
            &PartialContext {
                constants: &self.constants,
                signage: &signage,
                next_height: peak
                    .height
                    .checked_add(1)
                    .ok_or_else(|| Error::other("height overflow"))?,
                previous_transaction_height: if peak.is_transaction_block() {
                    peak.height
                } else {
                    peak.prev_transaction_block_height
                },
                contract_puzzle_hash: farmer.contract_puzzle_hash,
                difficulty: farmer.difficulty,
                now,
                time_limit: self.partial_time_limit,
            },
        )
    }
}
