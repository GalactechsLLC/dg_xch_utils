use dg_xch_core::consensus::chain_definition::ChainDefinition;
use dg_xch_core::consensus::constants::MAINNET;
use dg_xch_timelord::GenesisWork;

#[test]
fn genesis_work_uses_fork_challenges_and_chia_timing() {
    let definition = ChainDefinition::default();
    let work = GenesisWork::from_chain_definition(&definition).unwrap();
    assert_eq!(
        work.challenge_chain_challenge,
        definition.constants().unwrap().genesis_challenge
    );
    assert_eq!(work.reward_chain_challenge, work.challenge_chain_challenge);
    assert_ne!(work.challenge_chain_challenge, MAINNET.genesis_challenge);
    assert_eq!(work.difficulty, MAINNET.difficulty_starting);
    assert_eq!(work.sub_slot_iters, MAINNET.sub_slot_iters_starting);
    assert_eq!(work.discriminant_size_bits, MAINNET.discriminant_size_bits);
}
