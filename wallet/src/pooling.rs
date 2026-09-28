use crate::Wallet;
use crate::memory_wallet::MemoryWallet;
use dg_xch_core::blockchain::announcement::Announcement;
use dg_xch_core::blockchain::coin::Coin;
use dg_xch_core::blockchain::condition_with_args::Message;
use dg_xch_core::blockchain::spend_bundle::SpendBundle;
use dg_xch_puzzles::pool_launch::PoolLaunch;
use std::io::Error;

pub async fn fund_launch(
    wallet: &MemoryWallet,
    origin: Coin,
    launch: PoolLaunch,
    fee: u64,
) -> Result<SpendBundle, Error> {
    let launcher = launch
        .spends
        .first()
        .ok_or_else(|| Error::other("PlotNFT launch has no launcher spend"))?;
    if launcher.coin.parent_coin_info != origin.name()
        || launcher.coin.name() != launch.launcher_id
        || launcher.coin.amount != 1
        || launch.spends.len() > 2
    {
        return Err(Error::other(
            "PlotNFT launch is not bound to the funding coin",
        ));
    }
    let mut announcements = vec![Announcement {
        origin_info: launcher.coin.name(),
        message: Message::new(launcher.solution.to_program()?.tree_hash().to_vec())?,
        morph_bytes: None,
    }];
    if let Some(revision) = launch.spends.get(1) {
        if revision.coin.parent_coin_info != launch.launcher_id {
            return Err(Error::other(
                "PlotNFT revision does not descend from the launcher",
            ));
        }
        announcements.push(Announcement {
            origin_info: revision.coin.name(),
            message: Message::new(Vec::new())?,
            morph_bytes: None,
        });
    }
    let transaction = wallet
        .generate_signed_transaction(
            1,
            &launcher.coin.puzzle_hash,
            fee,
            Some(origin.name()),
            Some(vec![origin]),
            None,
            true,
            Some(&announcements),
            None,
            None,
            false,
            None,
            None,
            None,
            None,
            Some(true),
        )
        .await?;
    let mut bundle = transaction
        .spend_bundle
        .ok_or_else(|| Error::other("wallet did not sign the PlotNFT funding transaction"))?;
    bundle.coin_spends.extend(launch.spends);
    bundle.validate(Some(500_000_000), 0, &wallet.wallet_info().constants, false)?;
    Ok(bundle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use dg_xch_clients::rpc::full_node::FullnodeClient;
    use dg_xch_core::blockchain::sized_bytes::Bytes32;
    use dg_xch_core::clvm::program::SerializedProgram;
    use dg_xch_core::consensus::constants::{ConsensusConstants, TESTNET_11};
    use dg_xch_core::pool::PoolState;
    use dg_xch_core::traits::SizedBytes;
    use std::sync::Arc;

    fn submit(simulator: &mut chia_sdk_test::Simulator, bundle: &SpendBundle) {
        let spends = bundle
            .coin_spends
            .iter()
            .map(|spend| {
                chia_protocol::CoinSpend::new(
                    chia_protocol::Coin::new(
                        chia_protocol::Bytes32::new(spend.coin.parent_coin_info.bytes()),
                        chia_protocol::Bytes32::new(spend.coin.puzzle_hash.bytes()),
                        spend.coin.amount,
                    ),
                    spend.puzzle_reveal.to_bytes().into(),
                    spend.solution.to_bytes().into(),
                )
            })
            .collect();
        let signature =
            chia_bls::Signature::from_bytes(&bundle.aggregated_signature.bytes()).unwrap();
        simulator
            .new_transaction(chia_protocol::SpendBundle::new(spends, signature))
            .unwrap();
    }

    #[tokio::test]
    async fn v1_and_v2_launches_pass_reference_consensus() {
        let client = FullnodeClient::new_simulator("127.0.0.1", 1, 1).unwrap();
        let constants = Arc::new(ConsensusConstants {
            simulated: true,
            ..TESTNET_11
        });
        let key = blst::min_pk::SecretKey::key_gen(&[77; 32], &[]).unwrap();
        let wallet = MemoryWallet::new(key, &client, constants.clone()).unwrap();
        let owner = wallet.get_puzzle_hash(false).await.unwrap();
        let public = wallet.public_key(0).await.unwrap();
        for version in [1, 2] {
            let mut simulator = chia_sdk_test::Simulator::new();
            let funding = simulator.new_coin(chia_protocol::Bytes32::new(owner.bytes()), 10_000);
            let origin = Coin {
                parent_coin_info: funding.parent_coin_info.to_bytes().into(),
                puzzle_hash: owner,
                amount: funding.amount,
            };
            let launch = if version == 1 {
                dg_xch_puzzles::pool_launch::launch_v1(
                    origin,
                    &PoolState {
                        version: 1,
                        state: 3,
                        target_puzzle_hash: [5; 32].into(),
                        owner_pubkey: public,
                        pool_url: Some("https://pool.example".into()),
                        relative_lock_height: 100,
                    },
                    constants.genesis_challenge,
                    3600,
                    owner,
                )
                .unwrap()
            } else {
                dg_xch_puzzles::pool_v2::PlotNft::launch(
                    origin,
                    constants.genesis_challenge,
                    public,
                    Some(dg_xch_puzzles::pool_v2::PoolConfig {
                        target: [5; 32].into(),
                        relative_lock_height: 100,
                        memoization: SerializedProgram::from_bytes(&[0x80]),
                    }),
                    owner,
                )
                .unwrap()
            };
            let singleton = launch.singleton;
            let launcher = launch.spends[0].coin;
            let parent = launch.spends.last().unwrap().clone();
            let contract = launch.contract_puzzle_hash;
            let bundle = fund_launch(&wallet, origin, launch, 100).await.unwrap();
            submit(&mut simulator, &bundle);
            assert_ne!(singleton.puzzle_hash, Bytes32::default());
            let reward = dg_xch_core::consensus::coinbase::create_pool_coin(
                1,
                contract,
                dg_xch_core::consensus::block_rewards::calculate_pool_reward(1),
                constants.genesis_challenge,
            );
            simulator.insert_coin(chia_protocol::Coin::new(
                chia_protocol::Bytes32::new(reward.parent_coin_info.bytes()),
                chia_protocol::Bytes32::new(reward.puzzle_hash.bytes()),
                reward.amount,
            ));
            let claims = if version == 1 {
                let state = PoolState {
                    version: 1,
                    state: 3,
                    target_puzzle_hash: [5; 32].into(),
                    owner_pubkey: public,
                    pool_url: Some("https://pool.example".into()),
                    relative_lock_height: 100,
                };
                dg_xch_puzzles::clvm_puzzles::create_absorb_spend(
                    &parent,
                    &state,
                    launcher,
                    1,
                    constants.genesis_challenge,
                    3600,
                    owner,
                )
                .unwrap()
            } else {
                dg_xch_puzzles::pool_v2::PlotNft {
                    launcher_id: launcher.name(),
                    genesis_challenge: constants.genesis_challenge,
                    synthetic_public_key: public,
                    exiting: false,
                    pool: Some(dg_xch_puzzles::pool_v2::PoolConfig {
                        target: [5; 32].into(),
                        relative_lock_height: 100,
                        memoization: SerializedProgram::from_bytes(&[0x80]),
                    }),
                }
                .claim_reward(singleton, &parent, reward, 1)
                .unwrap()
            };
            let claim = SpendBundle {
                coin_spends: claims,
                ..SpendBundle::default()
            };
            claim
                .validate(Some(500_000_000), 0, &constants, false)
                .unwrap();
            submit(&mut simulator, &claim);
        }
    }
}
