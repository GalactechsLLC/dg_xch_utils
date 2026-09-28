use dg_xch_core::blockchain::coin::Coin;
use dg_xch_core::blockchain::coin_spend::CoinSpend;
use dg_xch_core::clvm::program::SerializedProgram;
use dg_xch_puzzles::pool_v2::{PlotNft, PoolConfig, reward_puzzle};

fn fixture() -> serde_json::Value {
    serde_json::from_str(include_str!("fixtures/pool_v2.json")).unwrap()
}

fn definition(fixture: &serde_json::Value) -> PlotNft {
    PlotNft {
        launcher_id: [4; 32].into(),
        genesis_challenge: [3; 32].into(),
        synthetic_public_key: serde_json::from_value(fixture["public_key"].clone()).unwrap(),
        pool: Some(PoolConfig {
            target: [5; 32].into(),
            relative_lock_height: 100,
            memoization: SerializedProgram::from_bytes(&[0x80]),
        }),
        exiting: false,
    }
}

#[test]
fn puzzle_hashes_match_pinned_chia_reference() {
    let fixture = fixture();
    for vector in fixture["vectors"].as_array().unwrap() {
        let mut definition = definition(&fixture);
        definition.exiting = vector["exiting"].as_bool().unwrap();
        if !vector["pooling"].as_bool().unwrap() {
            definition.pool = None;
        }
        assert_eq!(
            hex::encode(definition.puzzle().unwrap().tree_hash()),
            vector["puzzle_hash"].as_str().unwrap()
        );
        assert_eq!(
            hex::encode(definition.inner_puzzle().unwrap().tree_hash()),
            vector["inner_hash"].as_str().unwrap()
        );
        assert_eq!(
            hex::encode(reward_puzzle(definition.launcher_id).unwrap().tree_hash()),
            vector["reward_hash"].as_str().unwrap()
        );
    }
}

#[test]
fn reward_claim_matches_reference_bytes_and_outputs() {
    let fixture = fixture();
    let mut definition = definition(&fixture);
    definition.launcher_id = serde_json::from_value(fixture["launcher_id"].clone()).unwrap();
    let singleton: Coin = serde_json::from_value(fixture["singleton"].clone()).unwrap();
    let reward: Coin = serde_json::from_value(fixture["reward"].clone()).unwrap();
    let parent: CoinSpend = serde_json::from_value(fixture["parent_spend"].clone()).unwrap();
    let expected: Vec<CoinSpend> = serde_json::from_value(fixture["claims"].clone()).unwrap();
    let claims = definition
        .claim_reward(singleton, &parent, reward, 128)
        .unwrap();
    assert_eq!(claims, expected);
    let additions: Vec<_> = claims
        .iter()
        .flat_map(|spend| spend.compute_additions_with_cost(500_000_000).unwrap().0)
        .collect();
    assert!(
        additions
            .iter()
            .any(|coin| coin.amount == reward.amount && coin.puzzle_hash == [5; 32].into())
    );
    assert!(
        additions
            .iter()
            .any(|coin| coin.amount == 1 && coin.puzzle_hash == singleton.puzzle_hash)
    );
    assert!(
        definition
            .claim_reward(singleton, &parent, reward, 129)
            .is_err()
    );
}

#[test]
fn launch_matches_reference_singleton_and_memo_bytes() {
    let fixture = fixture();
    let definition = definition(&fixture);
    let origin = Coin {
        parent_coin_info: [1; 32].into(),
        puzzle_hash: [2; 32].into(),
        amount: 100,
    };
    let launch = PlotNft::launch(
        origin,
        definition.genesis_challenge,
        definition.synthetic_public_key,
        definition.pool,
        [6; 32].into(),
    )
    .unwrap();
    let expected: CoinSpend = serde_json::from_value(fixture["parent_spend"].clone()).unwrap();
    assert_eq!(launch.spends[1], expected);
    assert_eq!(
        launch.singleton,
        serde_json::from_value::<Coin>(fixture["singleton"].clone()).unwrap()
    );
    let additions = launch.spends[0]
        .compute_additions_with_cost(500_000_000)
        .unwrap()
        .0;
    assert_eq!(additions, vec![expected.coin]);
}
