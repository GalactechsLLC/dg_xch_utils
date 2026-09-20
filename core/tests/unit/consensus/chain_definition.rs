use super::*;
use crate::consensus::block_rewards::{calculate_base_farmer_reward, calculate_pool_reward};

#[cfg(feature = "bls")]
#[test]
fn chain_bound_signatures_cannot_replay_between_chia_and_fork() {
    use crate::blockchain::coin::Coin;
    use crate::blockchain::condition_with_args::{ConditionWithArgs, Message};
    use crate::blockchain::sized_bytes::Bytes48;
    use crate::blockchain::utils::pkm_pairs_for_conditions;
    use crate::clvm::bls_bindings::{sign, verify_signature};
    let secret = blst::min_pk::SecretKey::key_gen_v3(&[42; 32], &[]).unwrap();
    let public = secret.sk_to_pk();
    let public_bytes = Bytes48::const_new(public.to_bytes());
    let message = Message::new(b"fork signature test".to_vec()).unwrap();
    let coin = Coin {
        parent_coin_info: Bytes32::const_new([1; 32]),
        puzzle_hash: Bytes32::const_new([2; 32]),
        amount: 100,
    };
    let fork = ChainDefinition::default().constants().unwrap();
    for condition in [
        ConditionWithArgs::AggSigMe(public_bytes, message),
        ConditionWithArgs::AggSigParent(public_bytes, message),
        ConditionWithArgs::AggSigPuzzle(public_bytes, message),
        ConditionWithArgs::AggSigAmount(public_bytes, message),
        ConditionWithArgs::AggSigPuzzleAmount(public_bytes, message),
        ConditionWithArgs::AggSigParentAmount(public_bytes, message),
        ConditionWithArgs::AggSigParentPuzzle(public_bytes, message),
    ] {
        let fork_pairs = pkm_pairs_for_conditions(
            std::slice::from_ref(&condition),
            coin,
            fork.agg_sig_me_additional_data.as_ref(),
        )
        .unwrap();
        let chia_pairs = pkm_pairs_for_conditions(
            std::slice::from_ref(&condition),
            coin,
            MAINNET.agg_sig_me_additional_data.as_ref(),
        )
        .unwrap();
        let signature = sign(&secret, fork_pairs[0].2.data());
        assert!(verify_signature(
            &public,
            fork_pairs[0].2.data(),
            &signature
        ));
        assert!(!verify_signature(
            &public,
            chia_pairs[0].2.data(),
            &signature
        ));
        let signature = sign(&secret, chia_pairs[0].2.data());
        assert!(!verify_signature(
            &public,
            fork_pairs[0].2.data(),
            &signature
        ));
    }
}

#[test]
fn fork_preserves_chia_rules_except_identity_and_prefarm() {
    let definition: ChainDefinition =
        serde_json::from_str(include_str!("../../../../config/chains/dgx.json")).unwrap();
    assert_eq!(definition, ChainDefinition::default());
    let mut fork = definition.constants().unwrap();
    assert_eq!(
        fork.genesis_challenge,
        Bytes32::const_hex("4036187ee67b80180fcf458bcf0c2445c1da246c5e8c0369e0d6bb11bda8947a")
    );
    assert_eq!(
        fork.agg_sig_me_additional_data,
        Bytes32::const_hex("fc78c039220cac8226a4ad3be669debee55da8b7db7d883a3c541ea8ff5fa30d")
    );
    assert_ne!(fork.genesis_challenge, MAINNET.genesis_challenge);
    assert_ne!(
        fork.agg_sig_me_additional_data,
        MAINNET.agg_sig_me_additional_data
    );
    assert_ne!(fork.agg_sig_me_additional_data, fork.genesis_challenge);
    assert_eq!(fork.rewards.pool_reward(0), 0);
    assert_eq!(fork.rewards.farmer_reward(0), 0);
    for height in [
        1,
        5_045_759,
        5_045_760,
        10_091_520,
        15_137_280,
        20_183_040,
        u32::MAX,
    ] {
        assert_eq!(
            fork.rewards.pool_reward(height),
            calculate_pool_reward(height)
        );
        assert_eq!(
            fork.rewards.farmer_reward(height),
            calculate_base_farmer_reward(height)
        );
    }
    fork.rewards = MAINNET.rewards;
    fork.genesis_challenge = MAINNET.genesis_challenge;
    fork.agg_sig_me_additional_data = MAINNET.agg_sig_me_additional_data;
    fork.genesis_pre_farm_pool_puzzle_hash = MAINNET.genesis_pre_farm_pool_puzzle_hash;
    fork.genesis_pre_farm_farmer_puzzle_hash = MAINNET.genesis_pre_farm_farmer_puzzle_hash;
    fork.bech32_prefix = MAINNET.bech32_prefix;
    assert_eq!(fork, MAINNET);
}

#[test]
fn reward_configuration_changes_consensus_and_network_identity() {
    let definition = ChainDefinition::default();
    let mut changed = definition.clone();
    changed.rewards.initial_farmer = 123;
    changed.rewards.halving_interval = 10;
    let constants = changed.constants().unwrap();
    assert_eq!(constants.rewards.farmer_reward(1), 123);
    assert_eq!(constants.rewards.farmer_reward(10), 61);
    assert_ne!(
        definition.constants().unwrap().genesis_challenge,
        constants.genesis_challenge
    );
    assert_ne!(
        definition.constants().unwrap().agg_sig_me_additional_data,
        constants.agg_sig_me_additional_data
    );
    assert_ne!(
        definition.handshake_network_id().unwrap(),
        changed.handshake_network_id().unwrap()
    );
    assert_eq!(
        constants,
        serde_json::from_str::<ChainDefinition>(&serde_json::to_string_pretty(&changed).unwrap())
            .unwrap()
            .constants()
            .unwrap()
    );
}

#[test]
fn invalid_definitions_fail_closed() {
    let mut definition = ChainDefinition::default();
    definition.rewards.halving_interval = 0;
    assert!(definition.constants().is_err());
    definition.rewards = RewardSchedule::NO_PREFARM;
    definition.rewards.max_halvings = 64;
    assert!(definition.constants().is_err());
    definition.rewards = RewardSchedule::CHIA;
    assert!(definition.constants().is_err());
    definition.rewards = RewardSchedule::NO_PREFARM;
    definition.network_id = "mainnet".into();
    assert!(definition.constants().is_err());
    definition.network_id = "dgx".into();
    definition.genesis_seed.clear();
    assert!(definition.constants().is_err());
    assert!(
        serde_json::from_str::<ChainDefinition>(
            r#"{"network_id":"dgx","genesis_seed":"seed","unknown":1}"#
        )
        .is_err()
    );
}

#[test]
fn chia_schedule_preserves_prefarm_and_all_halving_boundaries() {
    for height in [
        0,
        1,
        5_045_759,
        5_045_760,
        10_091_519,
        10_091_520,
        15_137_279,
        15_137_280,
        20_183_039,
        20_183_040,
        u32::MAX,
    ] {
        assert_eq!(
            RewardSchedule::CHIA.pool_reward(height),
            calculate_pool_reward(height)
        );
        assert_eq!(
            RewardSchedule::CHIA.farmer_reward(height),
            calculate_base_farmer_reward(height)
        );
    }
}
