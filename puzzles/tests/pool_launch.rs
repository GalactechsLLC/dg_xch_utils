use dg_xch_core::blockchain::coin::Coin;
use dg_xch_core::blockchain::coin_spend::CoinSpend;
use dg_xch_core::clvm::program::{Program, SerializedProgram};
use dg_xch_core::clvm::sexp::SExp;
use dg_xch_core::pool::PoolState;
use dg_xch_puzzles::clvm_puzzles::get_delay_puzzle_info_from_launcher_spend;
use dg_xch_puzzles::pool_launch::launch_v1;

#[test]
fn launcher_delay_metadata_round_trips_and_rejects_missing_fields() {
    let origin = Coin {
        parent_coin_info: [1; 32].into(),
        puzzle_hash: [2; 32].into(),
        amount: 100,
    };
    let key = blst::min_pk::SecretKey::key_gen(&[42; 32], &[]).unwrap();
    let state = PoolState {
        version: 1,
        state: 3,
        target_puzzle_hash: [5; 32].into(),
        owner_pubkey: key.sk_to_pk().to_bytes().into(),
        pool_url: Some("https://pool.example".into()),
        relative_lock_height: 100,
    };
    let launch = launch_v1(origin, &state, [3; 32].into(), 3600, [6; 32].into()).unwrap();
    assert_eq!(
        get_delay_puzzle_info_from_launcher_spend(&launch.spends[0]).unwrap(),
        (3600, [6; 32].into())
    );
    assert_eq!(
        launch.spends[0]
            .compute_additions_with_cost(500_000_000)
            .unwrap()
            .0,
        vec![launch.singleton]
    );
    for metadata in [
        SExp::from(Vec::<SExp>::new()),
        SExp::from(vec![SExp::from(("h", vec![6u8; 32]))]),
    ] {
        let malformed = CoinSpend {
            coin: origin,
            puzzle_reveal: SerializedProgram::from_bytes(&[1]),
            solution: Program::to(vec![SExp::from(0u8), SExp::from(1u8), metadata])
                .serialized()
                .unwrap(),
        };
        assert!(get_delay_puzzle_info_from_launcher_spend(&malformed).is_err());
    }
}
