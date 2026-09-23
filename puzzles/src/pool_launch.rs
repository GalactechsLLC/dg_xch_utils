use dg_xch_core::blockchain::coin::Coin;
use dg_xch_core::blockchain::coin_spend::CoinSpend;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::clvm::program::Program;
use dg_xch_core::clvm::sexp::SExp;
use dg_xch_core::pool::PoolState;
use dg_xch_core::utils::hash_256;
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use std::io::Error;

pub struct PoolLaunch {
    pub launcher_id: Bytes32,
    pub contract_puzzle_hash: Bytes32,
    pub singleton: Coin,
    pub spends: Vec<CoinSpend>,
    pub funding_conditions: Program<'static>,
}

pub fn launch_v1(
    origin: Coin,
    state: &PoolState,
    genesis: Bytes32,
    delay: u64,
    delay_hash: Bytes32,
) -> Result<PoolLaunch, Error> {
    if origin.amount < 1
        || delay < 3600
        || state.version != 1
        || ![1, 3].contains(&state.state)
        || (state.state == 3
            && (state.relative_lock_height == 0
                || state.relative_lock_height > 1000
                || state
                    .pool_url
                    .as_ref()
                    .is_none_or(|url| !url.starts_with("https://") || url.len() > 2048)))
    {
        return Err(Error::other(
            "invalid initial pool state or delayed-claim configuration",
        ));
    }
    blst::min_pk::PublicKey::key_validate(state.owner_pubkey.as_ref())
        .map_err(|_| Error::other("invalid PlotNFT owner key"))?;
    let launcher = Coin {
        parent_coin_info: origin.name(),
        puzzle_hash: crate::singleton_launcher::SINGLETON_LAUNCHER_TREE_HASH,
        amount: 1,
    };
    let launcher_id = launcher.name();
    let inner = crate::clvm_puzzles::pool_state_to_inner_puzzle(
        state,
        launcher_id,
        genesis,
        delay,
        delay_hash,
    )?;
    let full = crate::clvm_puzzles::create_full_puzzle(&inner, launcher_id)?;
    let extra = Program::to(vec![
        SExp::from(("p", state.to_bytes(ChiaProtocolVersion::Chia0_0_37)?)),
        SExp::from(("t", delay)),
        SExp::from(("h", delay_hash)),
    ]);
    let solution = Program::to(vec![
        SExp::from(full.tree_hash()),
        SExp::from(1u8),
        extra.sexp().to_owned(),
    ]);
    let announcement = hash_256([&launcher_id[0..32], &solution.tree_hash()[0..32]].concat());
    let funding_conditions = Program::to(vec![
        SExp::from(vec![
            SExp::from(51u8),
            SExp::from(launcher.puzzle_hash),
            SExp::from(1u8),
        ]),
        SExp::from(vec![
            SExp::from(61u8),
            SExp::from(Bytes32::from(announcement)),
        ]),
    ]);
    Ok(PoolLaunch {
        launcher_id,
        contract_puzzle_hash: crate::clvm_puzzles::launcher_id_to_p2_puzzle_hash(
            launcher_id,
            delay,
            delay_hash,
        )?,
        singleton: Coin {
            parent_coin_info: launcher_id,
            puzzle_hash: full.tree_hash(),
            amount: 1,
        },
        spends: vec![CoinSpend {
            coin: launcher,
            puzzle_reveal: crate::singleton_launcher::SINGLETON_LAUNCHER_PROGRAM.serialized()?,
            solution: solution.serialized()?,
        }],
        funding_conditions,
    })
}
