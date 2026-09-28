use crate::singleton_launcher::SINGLETON_LAUNCHER_TREE_HASH;
use crate::singleton_top_layer::{SINGLETON_TOP_LAYER_PROGRAM, SINGLETON_TOP_LAYER_TREE_HASH};
use crate::singleton_top_layer_v1_1::{
    SINGLETON_TOP_LAYER_V1_1_PROGRAM, SINGLETON_TOP_LAYER_V1_1_TREE_HASH,
};
use dg_parser_macro::parse_program_hex;
use dg_xch_core::blockchain::coin::Coin;
use dg_xch_core::blockchain::coin_spend::CoinSpend;
use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes48};
use dg_xch_core::clvm::program::Program;
use dg_xch_core::clvm::sexp::{AtomBuf, SExp};
use dg_xch_core::consensus::block_rewards::calculate_pool_reward;
use dg_xch_core::consensus::coinbase::pool_parent_id;
use dg_xch_core::errors::ClvmError;
use dg_xch_core::formatting::number_from_slice;
use dg_xch_core::plots::PlotNftExtraData;
use dg_xch_core::pool::PoolState;
use dg_xch_core::traits::SizedBytes;
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use log::debug;
use num_traits::{ToPrimitive, Zero};
use std::io::{Cursor, Error, ErrorKind};

parse_program_hex!(
    POOL_WAITING_ROOM,
    "ff02ffff01ff02ffff03ff82017fffff01ff04ffff04ff1cffff04ff5fff808080ffff04ffff04ff12ffff04ff8205ffffff04ff8206bfff80808080ffff04ffff04ff08ffff04ff17ffff04ffff02ff1effff04ff02ffff04ffff04ff8205ffffff04ff8202ffff808080ff80808080ff80808080ff80808080ffff01ff02ff16ffff04ff02ffff04ff05ffff04ff8204bfffff04ff8206bfffff04ff8202ffffff04ffff0bffff19ff2fffff18ffff019100ffffffffffffffffffffffffffffffffff8205ff8080ff0bff8202ff80ff808080808080808080ff0180ffff04ffff01ffff32ff3d52ffff333effff04ffff04ff12ffff04ff0bffff04ff17ff80808080ffff04ffff04ff12ffff04ff05ffff04ff2fff80808080ffff04ffff04ff1affff04ff5fff808080ffff04ffff04ff14ffff04ffff0bff5fffff012480ff808080ff8080808080ff02ffff03ffff07ff0580ffff01ff0bffff0102ffff02ff1effff04ff02ffff04ff09ff80808080ffff02ff1effff04ff02ffff04ff0dff8080808080ffff01ff0bffff0101ff058080ff0180ff018080"
);
parse_program_hex!(
    POOL_MEMBER,
    "ff02ffff01ff02ffff03ff8202ffffff01ff02ff16ffff04ff02ffff04ff05ffff04ff8204bfffff04ff8206bfffff04ff82017fffff04ffff0bffff19ff2fffff18ffff019100ffffffffffffffffffffffffffffffffff8202ff8080ff0bff82017f80ff8080808080808080ffff01ff04ffff04ff08ffff04ff17ffff04ffff02ff1effff04ff02ffff04ff82017fff80808080ff80808080ffff04ffff04ff1cffff04ff5fffff04ff8206bfff80808080ff80808080ff0180ffff04ffff01ffff32ff3d33ff3effff04ffff04ff1cffff04ff0bffff04ff17ff80808080ffff04ffff04ff1cffff04ff05ffff04ff2fff80808080ffff04ffff04ff0affff04ff5fff808080ffff04ffff04ff14ffff04ffff0bff5fffff012480ff808080ff8080808080ff02ffff03ffff07ff0580ffff01ff0bffff0102ffff02ff1effff04ff02ffff04ff09ff80808080ffff02ff1effff04ff02ffff04ff0dff8080808080ffff01ff0bffff0101ff058080ff0180ff018080"
);
parse_program_hex!(
    P2_SINGLETON_OR_DELAYED,
    "ff02ffff01ff02ffff03ff82017fffff01ff04ffff04ff38ffff04ffff0bffff02ff2effff04ff02ffff04ff05ffff04ff81bfffff04ffff02ff3effff04ff02ffff04ffff04ff05ffff04ff0bff178080ff80808080ff808080808080ff82017f80ff808080ffff04ffff04ff3cffff01ff248080ffff04ffff04ff28ffff04ff82017fff808080ff80808080ffff01ff04ffff04ff24ffff04ff2fff808080ffff04ffff04ff2cffff04ff5fffff04ff81bfff80808080ffff04ffff04ff10ffff04ff81bfff808080ff8080808080ff0180ffff04ffff01ffffff49ff463fffff5002ff333cffff04ff0101ffff02ff02ffff03ff05ffff01ff02ff36ffff04ff02ffff04ff0dffff04ffff0bff26ffff0bff2aff1280ffff0bff26ffff0bff26ffff0bff2aff3a80ff0980ffff0bff26ff0bffff0bff2aff8080808080ff8080808080ffff010b80ff0180ffff0bff26ffff0bff2aff3480ffff0bff26ffff0bff26ffff0bff2aff3a80ff0580ffff0bff26ffff02ff36ffff04ff02ffff04ff07ffff04ffff0bff2aff2a80ff8080808080ffff0bff2aff8080808080ff02ffff03ffff07ff0580ffff01ff0bffff0102ffff02ff3effff04ff02ffff04ff09ff80808080ffff02ff3effff04ff02ffff04ff0dff8080808080ffff01ff0bffff0101ff058080ff0180ff018080"
);
#[test]
pub fn test_hashes() {
    assert_eq!(
        Bytes32::const_hex("a317541a765bf8375e1c6e7c13503d0d2cbf56cacad5182befe947e78e2c0307"),
        POOL_WAITING_ROOM_TREE_HASH
    );
    assert_eq!(
        Bytes32::const_hex("a8490702e333ddd831a3ac9c22d0fa26d2bfeaf2d33608deb22f0e0123eb0494"),
        POOL_MEMBER_TREE_HASH
    );
    assert_eq!(
        Bytes32::const_hex("adb656e0211e2ab4f42069a4c5efc80dc907e7062be08bf1628c8e5b6d94d25b"),
        P2_SINGLETON_OR_DELAYED_TREE_HASH
    );
}
pub fn launcher_coin_spend_to_extra_data(
    coin_spend: &CoinSpend,
) -> Result<PlotNftExtraData, ClvmError> {
    if coin_spend.coin.puzzle_hash != SINGLETON_LAUNCHER_TREE_HASH {
        return Err(ClvmError::InvalidSyntax(
            "Provided coin spend is not launcher coin spend".to_string(),
        ));
    }
    let solution = coin_spend.solution.to_program()?;
    PlotNftExtraData::from_program(&solution.rest()?.rest()?.first()?)
}

pub fn puzzle_for_singleton(
    launcher_id: Bytes32,
    inner_puz: &'_ Program,
) -> Result<Program<'static>, ClvmError> {
    let top_layer: SExp<'_> = SINGLETON_TOP_LAYER_TREE_HASH.into();
    let launcher: SExp<'_> = launcher_id.into();
    let singleton_hash: SExp<'_> = SINGLETON_LAUNCHER_TREE_HASH.into();
    let rest_prog: SExp<'_> = (launcher, singleton_hash).into();
    let pair_prog: SExp<'_> = (top_layer, rest_prog).into();
    let args = vec![Program::new(pair_prog), inner_puz.to_owned()];
    Ok(SINGLETON_TOP_LAYER_PROGRAM.curry(&args).to_owned())
}

pub fn puzzle_for_singleton_v1_1(
    launcher_id: Bytes32,
    inner_puz: &Program,
) -> Result<Program<'static>, ClvmError> {
    let top_layer: SExp<'_> = SINGLETON_TOP_LAYER_V1_1_TREE_HASH.into();
    let launcher: SExp<'_> = launcher_id.into();
    let singleton_hash: SExp<'_> = SINGLETON_LAUNCHER_TREE_HASH.into();
    let rest_prog: SExp<'_> = (launcher, singleton_hash).into();
    let pair_prog: SExp<'_> = (top_layer, rest_prog).into();
    let args = vec![Program::new(pair_prog), inner_puz.to_owned()];
    Ok(SINGLETON_TOP_LAYER_V1_1_PROGRAM.curry(&args).to_owned())
}

pub fn create_waiting_room_inner_puzzle(
    target_puzzle_hash: Bytes32,
    relative_lock_height: u32,
    owner_pubkey: Bytes48,
    launcher_id: Bytes32,
    genesis_challenge: Bytes32,
    delay_time: u64,
    delay_ph: Bytes32,
) -> Result<Program<'static>, ClvmError> {
    let mut genesis_bytes = genesis_challenge.bytes()[0..16].to_vec();
    genesis_bytes.append(&mut b"\x00".repeat(16));
    let pool_reward_prefix: Bytes32 = Bytes32::parse(&genesis_bytes)?;
    let p2_singleton_puzzle_hash: Bytes32 =
        launcher_id_to_p2_puzzle_hash(launcher_id, delay_time, delay_ph)?;
    let args: Vec<Program> = vec![
        target_puzzle_hash.into(),
        p2_singleton_puzzle_hash.into(),
        owner_pubkey.into(),
        pool_reward_prefix.into(),
        relative_lock_height.try_into()?,
    ];
    Ok(POOL_WAITING_ROOM_PROGRAM.curry(&args).to_owned())
}

pub fn create_pooling_inner_puzzle(
    target_puzzle_hash: Bytes32,
    pool_waiting_room_inner_hash: Bytes32,
    owner_pubkey: Bytes48,
    launcher_id: Bytes32,
    genesis_challenge: Bytes32,
    delay_time: u64,
    delay_ph: Bytes32,
) -> Result<Program<'static>, ClvmError> {
    let mut genesis_bytes = genesis_challenge.bytes()[..16].to_vec();
    genesis_bytes.append(&mut b"\x00".repeat(16));
    let pool_reward_prefix: Bytes32 = Bytes32::parse(&genesis_bytes)?;
    let p2_singleton_puzzle_hash: Bytes32 =
        launcher_id_to_p2_puzzle_hash(launcher_id, delay_time, delay_ph)?;
    let args: Vec<Program> = vec![
        target_puzzle_hash.into(),
        p2_singleton_puzzle_hash.into(),
        owner_pubkey.into(),
        pool_reward_prefix.into(),
        pool_waiting_room_inner_hash.into(),
    ];
    Ok(POOL_MEMBER_PROGRAM.curry(&args).to_owned())
}

pub fn create_full_puzzle(
    inner_puzzle: &Program,
    launcher_id: Bytes32,
) -> Result<Program<'static>, ClvmError> {
    puzzle_for_singleton(launcher_id, inner_puzzle)
}

pub fn create_p2_singleton_puzzle(
    singleton_mod_hash: Bytes32,
    launcher_id: Bytes32,
    seconds_delay: u64,
    delayed_puzzle_hash: Bytes32,
) -> Result<Program<'static>, ClvmError> {
    let args: Vec<Program> = vec![
        singleton_mod_hash.into(),
        launcher_id.into(),
        SINGLETON_LAUNCHER_TREE_HASH.into(),
        seconds_delay.try_into()?,
        delayed_puzzle_hash.into(),
    ];
    Ok(P2_SINGLETON_OR_DELAYED_PROGRAM.curry(&args).to_owned())
}

pub fn launcher_id_to_p2_puzzle_hash(
    launcher_id: Bytes32,
    seconds_delay: u64,
    delayed_puzzle_hash: Bytes32,
) -> Result<Bytes32, Error> {
    let as_prog = create_p2_singleton_puzzle(
        SINGLETON_TOP_LAYER_TREE_HASH,
        launcher_id,
        seconds_delay,
        delayed_puzzle_hash,
    )?;
    Ok(as_prog.tree_hash())
}

pub fn get_delay_puzzle_info_from_launcher_spend(
    coin_spend: &CoinSpend,
) -> Result<(u64, Bytes32), Error> {
    let solution = coin_spend.solution.to_program()?;
    let rest = solution.rest()?;
    let rest = rest.rest()?;
    let extra_data = rest.first()?;
    let as_map = extra_data.to_map()?;
    let seconds_vec = as_map.get(&Program::to("t")).ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidInput,
            "PlotNFT launcher has no delay time",
        )
    })?;
    let hash_vec = as_map.get(&Program::to("h")).ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidInput,
            "PlotNFT launcher has no delayed puzzle hash",
        )
    })?;
    let seconds = seconds_vec
        .as_vec()
        .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "PlotNFT delay time is not an atom"))?;
    Ok((
        number_from_slice(&seconds)
            .to_u64()
            .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "Failed to convert Atom to Int"))?,
        hash_vec.try_into()?,
    ))
}

pub fn get_template_singleton_inner_puzzle<'a>(
    inner_puzzle: &'a Program,
) -> Result<Program<'a>, Error> {
    Ok(inner_puzzle.uncurry()?.0)
}

pub fn get_seconds_and_delayed_puzhash_from_p2_singleton_puzzle(
    puzzle: &Program,
) -> Result<(u64, Bytes32), ClvmError> {
    let (_, args) = puzzle.uncurry()?;
    let as_list = args.as_list();
    if as_list.len() < 5 {
        return Err(ClvmError::InvalidInput(
            "Failed to unpack inner puzzle".to_string(),
        ));
    }
    let seconds_delay = as_list[3].to_owned();
    let delayed_puzzle_hash = as_list[4].to_owned();
    let seconds_delay_int: u64 = seconds_delay.try_into()?;
    Ok((
        seconds_delay_int,
        Bytes32::parse(
            delayed_puzzle_hash
                .as_atom()
                .unwrap_or_default()
                .serialized()?
                .as_ref(),
        )?,
    ))
}

pub fn is_pool_singleton_inner_puzzle(inner_puzzle: &Program) -> Result<bool, Error> {
    let inner_f = get_template_singleton_inner_puzzle(inner_puzzle)?;
    Ok([&POOL_WAITING_ROOM_PROGRAM, &POOL_MEMBER_PROGRAM].contains(&&inner_f))
}

pub fn is_pool_waitingroom_inner_puzzle(inner_puzzle: &Program) -> Result<bool, Error> {
    let inner_f = get_template_singleton_inner_puzzle(inner_puzzle)?;
    Ok(POOL_WAITING_ROOM_PROGRAM == inner_f)
}

pub fn is_pool_member_inner_puzzle(inner_puzzle: &Program) -> Result<bool, Error> {
    let inner_f = get_template_singleton_inner_puzzle(inner_puzzle)?;
    Ok(POOL_MEMBER_PROGRAM == inner_f)
}

pub fn create_absorb_spend(
    last_coin_spend: &CoinSpend,
    current: &PoolState,
    launcher_coin: Coin,
    height: u32,
    genesis_challenge: Bytes32,
    delay_time: u64,
    delay_ph: Bytes32,
) -> Result<Vec<CoinSpend>, Error> {
    create_absorb_spend_with_reward(
        last_coin_spend,
        current,
        launcher_coin,
        height,
        genesis_challenge,
        delay_time,
        delay_ph,
        calculate_pool_reward(height),
    )
}

#[allow(clippy::too_many_arguments)]
pub fn create_absorb_spend_with_reward(
    last_coin_spend: &CoinSpend,
    current: &PoolState,
    launcher_coin: Coin,
    height: u32,
    genesis_challenge: Bytes32,
    delay_time: u64,
    delay_ph: Bytes32,
    reward_amount: u64,
) -> Result<Vec<CoinSpend>, Error> {
    if reward_amount == 0 {
        return Err(Error::other("pool reward must be positive"));
    }
    let inner_puzzle: Program = pool_state_to_inner_puzzle(
        current,
        launcher_coin.name(),
        genesis_challenge,
        delay_time,
        delay_ph,
    )?;
    let inner_sol = if is_pool_member_inner_puzzle(&inner_puzzle)? {
        //inner sol is (spend_type, pool_reward_amount, pool_reward_height, extra_data)
        Program::to(&[SExp::from(reward_amount), SExp::from(height)])
    } else if is_pool_waitingroom_inner_puzzle(&inner_puzzle)? {
        //inner sol is (spend_type, destination_puzhash, pool_reward_amount, pool_reward_height, extra_data)
        Program::to(&[SExp::from(0), SExp::from(reward_amount), SExp::from(height)])
    } else {
        return Err(Error::new(ErrorKind::InvalidInput, ""));
    };
    //full sol = (parent_info, my_amount, inner_solution)
    if let Some(coin) = get_most_recent_singleton_coin_from_coin_spend(last_coin_spend)? {
        let parent_info = if coin.parent_coin_info == launcher_coin.name() {
            Program::to(&[
                SExp::from(launcher_coin.parent_coin_info),
                SExp::from(launcher_coin.amount),
            ])
        } else if let Some(last_coin_spend_inner_puzzle) =
            get_inner_puzzle_from_puzzle(&last_coin_spend.puzzle_reveal.to_program()?)?
        {
            Program::to(&[
                SExp::from(last_coin_spend.coin.parent_coin_info),
                SExp::from(last_coin_spend_inner_puzzle.tree_hash()),
                SExp::from(last_coin_spend.coin.amount),
            ])
        } else {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "Invalid Inner Puzzle when calculating parent info",
            ));
        };
        let coin_amount = SExp::from(last_coin_spend.coin.amount);
        let full_solution: Program =
            Program::to([parent_info.sexp(), &coin_amount, inner_sol.sexp()]);
        let full_puzzle_program = create_full_puzzle(&inner_puzzle, launcher_coin.name())?;
        if coin.puzzle_hash != full_puzzle_program.tree_hash() {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "Coin puzzleHash and Puzzle Treehash are not Equal",
            ));
        }
        let reward_parent = pool_parent_id(height, genesis_challenge);
        let p2_singleton_puzzle_program = create_p2_singleton_puzzle(
            SINGLETON_TOP_LAYER_TREE_HASH,
            launcher_coin.name(),
            delay_time,
            delay_ph,
        )?;
        let p2_singleton_puzzle_tree_hash = p2_singleton_puzzle_program.tree_hash();
        let reward_coin = Coin {
            parent_coin_info: reward_parent,
            puzzle_hash: p2_singleton_puzzle_tree_hash,
            amount: reward_amount,
        };
        let p2_singleton_solution: Program =
            Program::to(&[inner_puzzle.tree_hash(), reward_coin.name()]);

        if reward_coin.puzzle_hash != p2_singleton_puzzle_tree_hash {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "Coin puzzleHash and Puzzle Treehash are not Equal",
            ));
        }
        if get_inner_puzzle_from_puzzle(&full_puzzle_program)?.is_none() {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "Failed to get inner puzzle from full puzzle",
            ));
        }
        let coin_spends = vec![
            CoinSpend {
                coin,
                puzzle_reveal: full_puzzle_program.serialized()?.to_owned(),
                solution: full_solution.serialized()?.to_owned(),
            },
            CoinSpend {
                coin: reward_coin,
                puzzle_reveal: p2_singleton_puzzle_program.serialized()?.to_owned(),
                solution: p2_singleton_solution.serialized()?.to_owned(),
            },
        ];
        Ok(coin_spends)
    } else {
        Err(Error::new(
            ErrorKind::InvalidInput,
            "Failed to find most recent singleton coin from coin spend",
        ))
    }
}

pub fn create_travel_spend(
    last_coin_spend: &CoinSpend,
    launcher_coin: Coin,
    current: &PoolState,
    target: &PoolState,
    genesis_challenge: Bytes32,
    delay_time: u64,
    delay_ph: Bytes32,
) -> Result<(CoinSpend, Program<'static>), Error> {
    let inner_puzzle = pool_state_to_inner_puzzle(
        current,
        launcher_coin.name(),
        genesis_challenge,
        delay_time,
        delay_ph,
    )?;
    let inner_solution = if is_pool_member_inner_puzzle(&inner_puzzle)? {
        Program::to(&[
            (&[SExp::from("p").cons(SExp::Atom(AtomBuf::new(
                target.to_bytes(ChiaProtocolVersion::default())?,
            )))])
                .into(),
            SExp::from(0),
        ])
    } else if is_pool_waitingroom_inner_puzzle(&inner_puzzle)? {
        let destination_inner = pool_state_to_inner_puzzle(
            target,
            launcher_coin.name(),
            genesis_challenge,
            delay_time,
            delay_ph,
        )?;
        debug!(
            "create_travel_spend: waitingroom: target PoolState bytes:\n{:?}\nhash:{}",
            target,
            Program::to(target.to_bytes(ChiaProtocolVersion::default())?).tree_hash()
        );
        Program::to(&[
            SExp::from(1),
            (&[SExp::from("p").cons(SExp::Atom(AtomBuf::new(
                target.to_bytes(ChiaProtocolVersion::default())?,
            )))])
                .into(),
            SExp::from(destination_inner.tree_hash()),
        ]) // current or target
    } else {
        return Err(Error::new(ErrorKind::InvalidInput, "Invalid Inner Puzzle"));
    };
    let current_singleton = get_most_recent_singleton_coin_from_coin_spend(last_coin_spend)?
        .ok_or(Error::new(
            ErrorKind::InvalidInput,
            "Failed to find singleton",
        ))?;
    let parent_info_list = if current_singleton.parent_coin_info == launcher_coin.name() {
        Program::to(&[
            SExp::from(launcher_coin.parent_coin_info),
            SExp::from(launcher_coin.amount),
        ])
    } else {
        let puzzle_reveal = last_coin_spend.puzzle_reveal.to_program()?;
        let last_coin_spend_inner_puzzle =
            get_inner_puzzle_from_puzzle(&puzzle_reveal)?.ok_or(Error::new(
                ErrorKind::InvalidInput,
                "Failed to get inner puzzle for last_coin_spend_inner_puzzle",
            ))?;
        Program::to(&[
            SExp::from(last_coin_spend.coin.parent_coin_info),
            SExp::from(last_coin_spend_inner_puzzle.tree_hash()),
            SExp::from(last_coin_spend.coin.amount),
        ])
    };
    let singleton_amount = SExp::from(current_singleton.amount);
    let full_solution = Program::to([
        parent_info_list.sexp(),
        &singleton_amount,
        inner_solution.sexp(),
    ]);
    let full_puzzle = create_full_puzzle(&inner_puzzle, launcher_coin.name())?;
    Ok((
        CoinSpend {
            coin: current_singleton,
            puzzle_reveal: full_puzzle.serialized()?.to_owned(),
            solution: full_solution.serialized()?.to_owned(),
        },
        inner_puzzle,
    ))
}

pub fn get_most_recent_singleton_coin_from_coin_spend(
    coin_solution: &CoinSpend,
) -> Result<Option<Coin>, Error> {
    for coin in coin_solution.additions()? {
        if coin.amount % 2 == 1 {
            return Ok(Some(coin));
        }
    }
    Ok(None)
}

pub fn get_pubkey_from_member_inner_puzzle(inner_puzzle: &Program) -> Result<Bytes48, Error> {
    match uncurry_pool_member_inner_puzzle(inner_puzzle) {
        Ok((_, _, _, pubkey_program, _, _)) => {
            Ok(pubkey_program.as_atom().unwrap_or_default().try_into()?)
        }
        Err(_) => Err(Error::other("Unable to extract pubkey")),
    }
}

pub fn uncurry_pool_member_inner_puzzle(
    inner_puzzle: &Program,
) -> Result<
    (
        Program<'static>,
        Program<'static>,
        Program<'static>,
        Program<'static>,
        Program<'static>,
        Program<'static>,
    ),
    Error,
> {
    if is_pool_member_inner_puzzle(inner_puzzle)? {
        match inner_puzzle.uncurry() {
            Ok((inner_f, args)) => {
                let args_sexp = args.sexp();
                let as_list = args_sexp.owned_list(true);
                let mut as_list: Vec<SExp> = as_list.into_iter().take(5).collect();
                if as_list.len() < 5 {
                    return Err(Error::other("Failed to unpack inner puzzle"));
                }
                let escape_puzzlehash = Program::new(as_list.remove(4)).to_owned();
                let pool_reward_prefix = Program::new(as_list.remove(3).to_owned());
                let owner_pubkey = Program::new(as_list.remove(2).to_owned());
                let p2_singleton_hash = Program::new(as_list.remove(1).to_owned());
                let target_puzzle_hash = Program::new(as_list.remove(0).to_owned());
                Ok((
                    inner_f.to_owned(),
                    target_puzzle_hash,
                    p2_singleton_hash,
                    owner_pubkey,
                    pool_reward_prefix,
                    escape_puzzlehash,
                ))
            }
            Err(_) => Err(Error::other("Failed to unpack inner puzzle")),
        }
    } else {
        Err(Error::other(
            "Attempting to unpack a non-waitingroom inner puzzle",
        ))
    }
}

pub fn uncurry_pool_waitingroom_inner_puzzle(
    inner_puzzle: &Program,
) -> Result<
    (
        Program<'static>,
        Program<'static>,
        Program<'static>,
        Program<'static>,
    ),
    Error,
> {
    if is_pool_waitingroom_inner_puzzle(inner_puzzle)? {
        match inner_puzzle.uncurry() {
            Ok((_, args)) => {
                let as_list = args.as_list();
                if as_list.len() < 5 {
                    return Err(Error::other("Failed to unpack inner puzzle"));
                }
                let target_puzzle_hash = as_list[0].to_owned();
                let p2_singleton_hash = as_list[1].to_owned();
                let owner_pubkey = as_list[2].to_owned();
                let relative_lock_height = as_list[4].to_owned();
                Ok((
                    target_puzzle_hash,
                    relative_lock_height,
                    owner_pubkey,
                    p2_singleton_hash,
                ))
            }
            Err(e) => Err(Error::other(format!(
                "Failed to unpack inner puzzle: {e:?}"
            ))),
        }
    } else {
        Err(Error::other(
            "Attempting to unpack a non-waitingroom inner puzzle",
        ))
    }
}

pub fn get_inner_puzzle_from_puzzle(
    full_puzzle: &Program,
) -> Result<Option<Program<'static>>, Error> {
    let (_, args) = full_puzzle.uncurry()?;
    let list: Vec<Program> = args.as_list();
    if list.len() < 2 {
        return Ok(None);
    }
    if !is_pool_singleton_inner_puzzle(&list[1])? {
        return Ok(None);
    }
    Ok(Some(list[1].to_owned()))
}

pub fn pool_state_from_extra_data(extra_data: Program) -> Result<Option<PoolState>, ClvmError> {
    let mut state_bytes: Option<Vec<u8>> = None;
    let extra_data = extra_data.to_map()?;
    for (key, value) in extra_data {
        let key_vec = key.as_vec().unwrap_or_default();
        if key_vec.len() == 1 && key_vec == b"p".to_vec() {
            state_bytes = Some(value.as_vec().unwrap_or_default());
            break;
        }
    }
    match state_bytes {
        Some(byte_data) => {
            let mut cursor = Cursor::new(byte_data.as_slice());
            Ok(Some(PoolState::from_bytes(
                &mut cursor,
                ChiaProtocolVersion::default(),
            )?))
        }
        None => Ok(None),
    }
}

pub fn solution_to_pool_state(coin_solution: &CoinSpend) -> Result<Option<PoolState>, ClvmError> {
    if coin_solution.coin.puzzle_hash == SINGLETON_LAUNCHER_TREE_HASH {
        //Launcher spend
        let as_program = coin_solution.solution.to_program()?;
        let rest = as_program.rest()?;
        let rest = rest.rest()?;
        let extra_data = rest.first()?;
        return pool_state_from_extra_data(extra_data);
    }
    // Not launcher spend
    let as_program = coin_solution.solution.to_program()?;
    let rest = as_program.rest()?;
    let rest = rest.rest()?;
    let inner_solution = rest.first()?.to_owned();
    // Spend which is not absorb, and is not the launcher
    let inner_map = inner_solution.to_map()?;
    let num_args = inner_map.len();
    if num_args == 2 {
        if inner_solution.rest()?.first()?.as_int()? != Zero::zero() {
            // pool member
            return Ok(None);
        }
        let extra_data = inner_solution.first()?;
        if extra_data.is_atom() {
            // Absorbing
            return Ok(None);
        }
        pool_state_from_extra_data(extra_data)
    } else if num_args == 3 {
        let first = inner_solution.first()?;
        let rest = inner_solution.rest()?;
        if first.as_int()? == Zero::zero() {
            // pool waitingroom
            return Ok(None);
        }
        let extra_data = rest.first()?;
        pool_state_from_extra_data(extra_data)
    } else {
        Err(ClvmError::InvalidArgCount(format!(
            "Invalid Arg Length {num_args}, expected 2 or 3"
        )))
    }
}

pub fn pool_state_to_inner_puzzle(
    pool_state: &PoolState,
    launcher_id: Bytes32,
    genesis_challenge: Bytes32,
    delay_time: u64,
    delay_ph: Bytes32,
) -> Result<Program<'static>, ClvmError> {
    let escaping_inner_puzzle: Program = create_waiting_room_inner_puzzle(
        pool_state.target_puzzle_hash,
        pool_state.relative_lock_height,
        pool_state.owner_pubkey,
        launcher_id,
        genesis_challenge,
        delay_time,
        delay_ph,
    )?;
    match pool_state.state {
        //Self Pooling = 1 Leaving Pool = 2
        1 | 2 => Ok(escaping_inner_puzzle),
        //Pooling
        _ => create_pooling_inner_puzzle(
            pool_state.target_puzzle_hash,
            escaping_inner_puzzle.tree_hash(),
            pool_state.owner_pubkey,
            launcher_id,
            genesis_challenge,
            delay_time,
            delay_ph,
        ),
    }
}

pub fn validate_puzzle_hash(
    launcher_id: Bytes32,
    delay_ph: Bytes32,
    delay_time: u64,
    pool_state: &PoolState,
    outer_puzzle_hash: Bytes32,
    genesis_challenge: Bytes32,
) -> Result<bool, Error> {
    let inner_puzzle: Program = pool_state_to_inner_puzzle(
        pool_state,
        launcher_id,
        genesis_challenge,
        delay_time,
        delay_ph,
    )?;
    let new_full_puzzle: Program = create_full_puzzle(&inner_puzzle, launcher_id)?;
    Ok(new_full_puzzle.tree_hash() == outer_puzzle_hash)
}
