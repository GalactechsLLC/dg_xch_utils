use std::io::{Error, ErrorKind};
use dg_xch_core::blockchain::coin::Coin;
use dg_xch_core::blockchain::coin_spend::CoinSpend;
use dg_xch_core::blockchain::condition_opcode::ConditionOpcode;
use dg_xch_core::clvm::program::Program;
use dg_xch_core::clvm::sexp::{IntoSExp};
use dg_xch_serialize::hash_256;
use crate::clvm_puzzles::{SINGLETON_LAUNCHER, SINGLETON_LAUNCHER_HASH, SINGLETON_MOD, SINGLETON_MOD_HASH};

pub fn generate_launcher_coin(coin: &Coin, amount: u64) -> Coin{
    Coin {
        parent_coin_info: coin.name(),
        puzzle_hash: *SINGLETON_LAUNCHER_HASH,
        amount,
    }
}

pub fn  launch_conditions_and_coin_spend (
    coin: Coin,
    inner_puzzle: Program,
    comment: Program,
    amount: u64,
) -> Result<(Vec<Program>, CoinSpend), Error> {
    if (amount % 2) == 0 {
        return Err(Error::new(ErrorKind::InvalidInput, "Coin amount cannot be even. Subtract one mojo."));
    }
    let launcher_coin: Coin = generate_launcher_coin(&coin, amount);
    let args = vec![
        Program::to((SINGLETON_MOD_HASH.to_sexp(), (launcher_coin.name().to_sexp(), SINGLETON_LAUNCHER_HASH.to_sexp()).to_sexp())),
        Program::to(inner_puzzle),
    ];
    let curried_singleton: Program = SINGLETON_MOD.curry(&args)?;
    let launcher_solution = Program::to(
        vec![
            curried_singleton.tree_hash().to_sexp(),
            amount.to_sexp(),
            comment.to_sexp(),
        ]
    );
    let create_launcher = Program::to(
        vec![
            ConditionOpcode::CreateCoin.to_sexp(),
            SINGLETON_LAUNCHER_HASH.to_sexp(),
            amount.to_sexp(),
        ],
    );
    let mut buf = vec![0;64];
    buf[0..32].copy_from_slice(launcher_coin.name().to_sized_bytes());
    buf[32..64].copy_from_slice(launcher_solution.tree_hash().to_sized_bytes());
    let assert_launcher_announcement = Program::to(
        vec![
            ConditionOpcode::AssertCoinAnnouncement.to_sexp(),
            hash_256(&buf).to_sexp(),
        ],
    );
    let conditions = vec![create_launcher, assert_launcher_announcement];
    let launcher_coin_spend = CoinSpend {
        coin: launcher_coin,
        puzzle_reveal: (*SINGLETON_LAUNCHER).clone().into(),
        solution: launcher_solution.into(),
    };
    Ok((conditions, launcher_coin_spend))
}