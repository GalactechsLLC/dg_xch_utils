use dg_xch_core::blockchain::coin::Coin;
use dg_xch_core::blockchain::coin_spend::CoinSpend;
use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes48, SizedBytes};
use dg_xch_core::blockchain::utils::atom_to_int;
use dg_xch_core::clvm::program::{Program, SerializedProgram};
use dg_xch_core::clvm::sexp::{AtomBuf, IntoSExp, SExp};
use dg_xch_core::plots::PlotNftExtraData;
use dg_xch_core::pool::PoolState;
use dg_xch_serialize::ChiaSerialize;
use lazy_static::lazy_static;
use log::{debug, info};
use num_traits::{ToPrimitive, Zero};
use std::io::{Cursor, Error, ErrorKind};

const SINGLETON_LAUNCHER_HEX: &str = "ff02ffff01ff04ffff04ff04ffff04ff05ffff04ff0bff80808080ffff04ffff04ff0affff04ffff02ff0effff04ff02ffff04ffff04ff05ffff04ff0bffff04ff17ff80808080ff80808080ff808080ff808080ffff04ffff01ff33ff3cff02ffff03ffff07ff0580ffff01ff0bffff0102ffff02ff0effff04ff02ffff04ff09ff80808080ffff02ff0effff04ff02ffff04ff0dff8080808080ffff01ff0bffff0101ff058080ff0180ff018080";
const SINGLETON_MOD_HEX: &str = "ff02ffff01ff02ffff03ffff18ff2fffff010180ffff01ff02ff36ffff04ff02ffff04ff05ffff04ff17ffff04ffff02ff26ffff04ff02ffff04ff0bff80808080ffff04ff2fffff04ff0bffff04ff5fff808080808080808080ffff01ff088080ff0180ffff04ffff01ffffffff4602ff3304ffff0101ff02ffff02ffff03ff05ffff01ff02ff5cffff04ff02ffff04ff0dffff04ffff0bff2cffff0bff24ff3880ffff0bff2cffff0bff2cffff0bff24ff3480ff0980ffff0bff2cff0bffff0bff24ff8080808080ff8080808080ffff010b80ff0180ff02ffff03ff0bffff01ff02ff32ffff04ff02ffff04ff05ffff04ff0bffff04ff17ffff04ffff02ff2affff04ff02ffff04ffff02ffff03ffff09ff23ff2880ffff0181b3ff8080ff0180ff80808080ff80808080808080ffff01ff02ffff03ff17ff80ffff01ff088080ff018080ff0180ffffffff0bffff0bff17ffff02ff3affff04ff02ffff04ff09ffff04ff2fffff04ffff02ff26ffff04ff02ffff04ff05ff80808080ff808080808080ff5f80ff0bff81bf80ff02ffff03ffff20ffff22ff4fff178080ffff01ff02ff7effff04ff02ffff04ff6fffff04ffff04ffff02ffff03ff4fffff01ff04ff23ffff04ffff02ff3affff04ff02ffff04ff09ffff04ff53ffff04ffff02ff26ffff04ff02ffff04ff05ff80808080ff808080808080ffff04ff81b3ff80808080ffff011380ff0180ffff02ff7cffff04ff02ffff04ff05ffff04ff1bffff04ffff21ff4fff1780ff80808080808080ff8080808080ffff01ff088080ff0180ffff04ffff09ffff18ff05ffff010180ffff010180ffff09ff05ffff01818f8080ff0bff2cffff0bff24ff3080ffff0bff2cffff0bff2cffff0bff24ff3480ff0580ffff0bff2cffff02ff5cffff04ff02ffff04ff07ffff04ffff0bff24ff2480ff8080808080ffff0bff24ff8080808080ffffff02ffff03ffff07ff0580ffff01ff0bffff0102ffff02ff26ffff04ff02ffff04ff09ff80808080ffff02ff26ffff04ff02ffff04ff0dff8080808080ffff01ff0bffff0101ff058080ff0180ff02ff5effff04ff02ffff04ff05ffff04ff0bffff04ffff02ff3affff04ff02ffff04ff09ffff04ff17ffff04ffff02ff26ffff04ff02ffff04ff05ff80808080ff808080808080ffff04ff17ffff04ff2fffff04ff5fffff04ff81bfff80808080808080808080ffff04ffff04ff20ffff04ff17ff808080ffff02ff7cffff04ff02ffff04ff05ffff04ffff02ff82017fffff04ffff04ffff04ff17ff2f80ffff04ffff04ff5fff81bf80ffff04ff0bff05808080ff8202ff8080ffff01ff80808080808080ffff02ff2effff04ff02ffff04ff05ffff04ff0bffff04ffff02ffff03ff3bffff01ff02ff22ffff04ff02ffff04ff05ffff04ff17ffff04ff13ffff04ff2bffff04ff5bffff04ff5fff808080808080808080ffff01ff02ffff03ffff09ff15ffff0bff13ff1dff2b8080ffff01ff0bff15ff17ff5f80ffff01ff088080ff018080ff0180ffff04ff17ffff04ff2fffff04ff5fffff04ff81bfffff04ff82017fff8080808080808080808080ff02ffff03ff05ffff011bffff010b80ff0180ff018080";
const POOL_WAITING_ROOM_MOD_HEX: &str = "ff02ffff01ff02ffff03ff82017fffff01ff04ffff04ff1cffff04ff5fff808080ffff04ffff04ff12ffff04ff8205ffffff04ff8206bfff80808080ffff04ffff04ff08ffff04ff17ffff04ffff02ff1effff04ff02ffff04ffff04ff8205ffffff04ff8202ffff808080ff80808080ff80808080ff80808080ffff01ff02ff16ffff04ff02ffff04ff05ffff04ff8204bfffff04ff8206bfffff04ff8202ffffff04ffff0bffff19ff2fffff18ffff019100ffffffffffffffffffffffffffffffffff8205ff8080ff0bff8202ff80ff808080808080808080ff0180ffff04ffff01ffff32ff3d52ffff333effff04ffff04ff12ffff04ff0bffff04ff17ff80808080ffff04ffff04ff12ffff04ff05ffff04ff2fff80808080ffff04ffff04ff1affff04ff5fff808080ffff04ffff04ff14ffff04ffff0bff5fffff012480ff808080ff8080808080ff02ffff03ffff07ff0580ffff01ff0bffff0102ffff02ff1effff04ff02ffff04ff09ff80808080ffff02ff1effff04ff02ffff04ff0dff8080808080ffff01ff0bffff0101ff058080ff0180ff018080";
const POOL_MEMBER_MOD_HEX: &str = "ff02ffff01ff02ffff03ff8202ffffff01ff02ff16ffff04ff02ffff04ff05ffff04ff8204bfffff04ff8206bfffff04ff82017fffff04ffff0bffff19ff2fffff18ffff019100ffffffffffffffffffffffffffffffffff8202ff8080ff0bff82017f80ff8080808080808080ffff01ff04ffff04ff08ffff04ff17ffff04ffff02ff1effff04ff02ffff04ff82017fff80808080ff80808080ffff04ffff04ff1cffff04ff5fffff04ff8206bfff80808080ff80808080ff0180ffff04ffff01ffff32ff3d33ff3effff04ffff04ff1cffff04ff0bffff04ff17ff80808080ffff04ffff04ff1cffff04ff05ffff04ff2fff80808080ffff04ffff04ff0affff04ff5fff808080ffff04ffff04ff14ffff04ffff0bff5fffff012480ff808080ff8080808080ff02ffff03ffff07ff0580ffff01ff0bffff0102ffff02ff1effff04ff02ffff04ff09ff80808080ffff02ff1effff04ff02ffff04ff0dff8080808080ffff01ff0bffff0101ff058080ff0180ff018080";
const P2_SINGLETON_OR_DELAYED_MOD_HEX: &str = "ff02ffff01ff02ffff03ff82017fffff01ff04ffff04ff38ffff04ffff0bffff02ff2effff04ff02ffff04ff05ffff04ff81bfffff04ffff02ff3effff04ff02ffff04ffff04ff05ffff04ff0bff178080ff80808080ff808080808080ff82017f80ff808080ffff04ffff04ff3cffff01ff248080ffff04ffff04ff28ffff04ff82017fff808080ff80808080ffff01ff04ffff04ff24ffff04ff2fff808080ffff04ffff04ff2cffff04ff5fffff04ff81bfff80808080ffff04ffff04ff10ffff04ff81bfff808080ff8080808080ff0180ffff04ffff01ffffff49ff463fffff5002ff333cffff04ff0101ffff02ff02ffff03ff05ffff01ff02ff36ffff04ff02ffff04ff0dffff04ffff0bff26ffff0bff2aff1280ffff0bff26ffff0bff26ffff0bff2aff3a80ff0980ffff0bff26ff0bffff0bff2aff8080808080ff8080808080ffff010b80ff0180ffff0bff26ffff0bff2aff3480ffff0bff26ffff0bff26ffff0bff2aff3a80ff0580ffff0bff26ffff02ff36ffff04ff02ffff04ff07ffff04ffff0bff2aff2a80ff8080808080ffff0bff2aff8080808080ff02ffff03ffff07ff0580ffff01ff0bffff0102ffff02ff3effff04ff02ffff04ff09ff80808080ffff02ff3effff04ff02ffff04ff0dff8080808080ffff01ff0bffff0101ff058080ff0180ff018080";
lazy_static! {
    pub static ref SINGLETON_LAUNCHER: Program =
        SerializedProgram::from_hex(SINGLETON_LAUNCHER_HEX)
            .unwrap()
            .to_program()
            .unwrap();
    pub static ref SINGLETON_LAUNCHER_HASH: Bytes32 = SINGLETON_LAUNCHER.tree_hash();
    pub static ref SINGLETON_MOD: Program = SerializedProgram::from_hex(SINGLETON_MOD_HEX)
        .unwrap()
        .to_program()
        .unwrap();
    pub static ref SINGLETON_MOD_HASH: Bytes32 = SINGLETON_MOD.tree_hash();
    pub static ref POOL_WAITING_ROOM_MOD: Program =
        SerializedProgram::from_hex(POOL_WAITING_ROOM_MOD_HEX)
            .unwrap()
            .to_program()
            .unwrap();
    pub static ref POOL_MEMBER_MOD: Program = SerializedProgram::from_hex(POOL_MEMBER_MOD_HEX)
        .unwrap()
        .to_program()
        .unwrap();
    pub static ref P2_SINGLETON_OR_DELAYED_MOD: Program =
        SerializedProgram::from_hex(P2_SINGLETON_OR_DELAYED_MOD_HEX)
            .unwrap()
            .to_program()
            .unwrap();
}

pub fn launcher_coin_spend_to_extra_data(
    coin_spend: &CoinSpend,
) -> Result<PlotNftExtraData, Error> {
    if coin_spend.coin.puzzle_hash != *SINGLETON_LAUNCHER_HASH {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "Provided coin spend is not launcher coin spend",
        ));
    }
    PlotNftExtraData::from_program(coin_spend.solution.to_program()?.rest()?.rest()?.first()?)
}

pub fn puzzle_for_singleton(launcher_id: &Bytes32, inner_puz: &Program) -> Result<Program, Error> {
    let args = vec![
        (
            (*SINGLETON_MOD_HASH).try_into()?,
            (
                launcher_id.try_into()?,
                (*SINGLETON_LAUNCHER_HASH).try_into()?,
            )
                .try_into()?,
        )
            .try_into()?,
        inner_puz.clone(),
    ];
    SINGLETON_MOD.curry(&args)
}

pub fn create_waiting_room_inner_puzzle(
    target_puzzle_hash: &Bytes32,
    relative_lock_height: u32,
    owner_pubkey: &Bytes48,
    launcher_id: &Bytes32,
    genesis_challenge: &Bytes32,
    delay_time: u64,
    delay_ph: &Bytes32,
) -> Result<Program, Error> {
    let mut genesis_bytes = genesis_challenge.as_slice()[0..16].to_vec();
    genesis_bytes.append(&mut b"\x00".repeat(16));
    let pool_reward_prefix: Bytes32 = Bytes32::new(&genesis_bytes);
    let p2_singleton_puzzle_hash: Bytes32 =
        launcher_id_to_p2_puzzle_hash(launcher_id, delay_time, delay_ph)?;
    let args: Vec<Program> = vec![
        target_puzzle_hash.try_into()?,
        p2_singleton_puzzle_hash.try_into()?,
        owner_pubkey.try_into()?,
        pool_reward_prefix.try_into()?,
        relative_lock_height.try_into()?,
    ];
    POOL_WAITING_ROOM_MOD.curry(&args)
}

pub fn create_pooling_inner_puzzle(
    target_puzzle_hash: &Bytes32,
    pool_waiting_room_inner_hash: &Bytes32,
    owner_pubkey: &Bytes48,
    launcher_id: &Bytes32,
    genesis_challenge: &Bytes32,
    delay_time: u64,
    delay_ph: &Bytes32,
) -> Result<Program, Error> {
    let mut genesis_bytes = genesis_challenge.as_slice()[..16].to_vec();
    genesis_bytes.append(&mut b"\x00".repeat(16));
    let pool_reward_prefix: Bytes32 = Bytes32::new(&genesis_bytes);
    let p2_singleton_puzzle_hash: Bytes32 =
        launcher_id_to_p2_puzzle_hash(launcher_id, delay_time, delay_ph)?;
    let args: Vec<Program> = vec![
        target_puzzle_hash.try_into()?,
        p2_singleton_puzzle_hash.try_into()?,
        owner_pubkey.try_into()?,
        pool_reward_prefix.try_into()?,
        pool_waiting_room_inner_hash.try_into()?,
    ];
    POOL_MEMBER_MOD.curry(&args)
}

pub fn create_full_puzzle(inner_puzzle: &Program, launcher_id: &Bytes32) -> Result<Program, Error> {
    puzzle_for_singleton(launcher_id, inner_puzzle)
}

pub fn create_p2_singleton_puzzle(
    singleton_mod_hash: &Bytes32,
    launcher_id: &Bytes32,
    seconds_delay: u64,
    delayed_puzzle_hash: &Bytes32,
) -> Result<Program, Error> {
    let args: Vec<Program> = vec![
        singleton_mod_hash.try_into()?,
        launcher_id.try_into()?,
        (*SINGLETON_LAUNCHER_HASH).try_into()?,
        seconds_delay.try_into()?,
        delayed_puzzle_hash.try_into()?,
    ];
    let curried = P2_SINGLETON_OR_DELAYED_MOD.curry(&args)?;
    Ok(curried)
}

pub fn launcher_id_to_p2_puzzle_hash(
    launcher_id: &Bytes32,
    seconds_delay: u64,
    delayed_puzzle_hash: &Bytes32,
) -> Result<Bytes32, Error> {
    let as_prog = create_p2_singleton_puzzle(
        &SINGLETON_MOD_HASH,
        launcher_id,
        seconds_delay,
        delayed_puzzle_hash,
    )?;

    Ok(as_prog.tree_hash())
}

pub fn get_delay_puzzle_info_from_launcher_spend(
    coin_solution: &CoinSpend,
) -> Result<(u64, Bytes32), Error> {
    let program = Program::new(coin_solution.solution.to_bytes());
    let extra_data = program.rest()?.rest()?.first()?;
    let as_map = extra_data.to_map()?;
    let seconds_vec = as_map.get(&Program::new("t".as_bytes().to_vec())).unwrap();
    let hash_vec = as_map.get(&Program::new("h".as_bytes().to_vec())).unwrap();
    Ok((
        atom_to_int(&seconds_vec.as_vec().unwrap())
            .to_u64()
            .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "Failed to convert Atom to Int"))?,
        hash_vec.try_into()?,
    ))
}

pub fn get_template_singleton_inner_puzzle(inner_puzzle: &Program) -> Result<Program, Error> {
    Ok(inner_puzzle.uncurry()?.0)
}

pub fn get_seconds_and_delayed_puzhash_from_p2_singleton_puzzle(
    puzzle: Program,
) -> Result<(u64, Bytes32), Error> {
    match puzzle.uncurry() {
        Ok((_, args)) => {
            let as_list = args.as_list();
            if as_list.len() < 5 {
                return Err(Error::new(
                    ErrorKind::Other,
                    "Failed to unpack inner puzzle",
                ));
            }
            let seconds_delay = as_list[3].clone();
            let delayed_puzzle_hash = as_list[4].clone();
            let seconds_delay_int: u64 = seconds_delay.try_into()?;
            Ok((
                seconds_delay_int,
                Bytes32::new(
                    &delayed_puzzle_hash
                        .as_atom()
                        .unwrap_or_else(|| Program::new(Vec::new()))
                        .serialized,
                ),
            ))
        }
        Err(error) => Err(error),
    }
}

pub fn is_pool_singleton_inner_puzzle(inner_puzzle: &Program) -> Result<bool, Error> {
    let inner_f = get_template_singleton_inner_puzzle(inner_puzzle)?;
    Ok([POOL_WAITING_ROOM_MOD.clone(), POOL_MEMBER_MOD.clone()].contains(&inner_f))
}

pub fn is_pool_waitingroom_inner_puzzle(inner_puzzle: &Program) -> Result<bool, Error> {
    let inner_f = get_template_singleton_inner_puzzle(inner_puzzle)?;
    Ok(*POOL_WAITING_ROOM_MOD == inner_f)
}

pub fn is_pool_member_inner_puzzle(inner_puzzle: &Program) -> Result<bool, Error> {
    let inner_f = get_template_singleton_inner_puzzle(inner_puzzle)?;
    Ok(POOL_MEMBER_MOD.clone() == inner_f)
}

pub fn create_travel_spend(
    last_coin_spend: &CoinSpend,
    launcher_coin: &Coin,
    current: &PoolState,
    target: &PoolState,
    genesis_challenge: &Bytes32,
    delay_time: u64,
    delay_ph: &Bytes32,
) -> Result<(CoinSpend, Program), Error> {
    let inner_puzzle = pool_state_to_inner_puzzle(
        current,
        &launcher_coin.name(),
        genesis_challenge,
        delay_time,
        delay_ph,
    )?;
    let inner_solution = if is_pool_member_inner_puzzle(&inner_puzzle)? {
        Program::to(vec![
            vec![("p".to_sexp(), SExp::Atom(AtomBuf::new(target.to_bytes())))].to_sexp(),
            0.to_sexp(),
        ])
    } else if is_pool_waitingroom_inner_puzzle(&inner_puzzle)? {
        let destination_inner = pool_state_to_inner_puzzle(
            target,
            &launcher_coin.name(),
            genesis_challenge,
            delay_time,
            delay_ph,
        )?;
        debug!(
            "create_travel_spend: waitingroom: target PoolState bytes:\n{:?}\nhash:{}",
            target,
            Program::to(target.to_bytes()).tree_hash()
        );
        Program::to(vec![
            1.to_sexp(),
            vec![("p".to_sexp(), target.to_bytes().to_sexp())].to_sexp(),
            destination_inner.tree_hash().to_sexp(),
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
        Program::to(vec![
            launcher_coin.parent_coin_info.to_sexp(),
            launcher_coin.amount.to_sexp(),
        ])
    } else {
        let p = last_coin_spend.puzzle_reveal.to_program()?;
        let last_coin_spend_inner_puzzle = get_inner_puzzle_from_puzzle(&p)?.ok_or(Error::new(
            ErrorKind::InvalidInput,
            "Failed to get inner puzzle for last_coin_spend_inner_puzzle",
        ))?;
        Program::to(vec![
            last_coin_spend.coin.parent_coin_info.to_sexp(),
            last_coin_spend_inner_puzzle.tree_hash().to_sexp(),
            last_coin_spend.coin.amount.to_sexp(),
        ])
    };
    let full_solution = Program::to(vec![
        parent_info_list.to_sexp(),
        current_singleton.amount.to_sexp(),
        inner_solution.to_sexp(),
    ]);
    let full_puzzle = create_full_puzzle(&inner_puzzle, &launcher_coin.name())?;
    Ok((
        CoinSpend {
            coin: current_singleton,
            puzzle_reveal: SerializedProgram::from_bytes(&full_puzzle.serialized),
            solution: SerializedProgram::from_bytes(&full_solution.serialized),
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
        Ok((_, _, _, pubkey_program, _, _)) => Ok(pubkey_program
            .as_atom()
            .unwrap_or_else(|| Program::new(Vec::new()))
            .try_into()?),
        Err(_) => Err(Error::new(ErrorKind::Other, "Unable to extract pubkey")),
    }
}

pub fn uncurry_pool_member_inner_puzzle(
    inner_puzzle: &Program,
) -> Result<(Program, Program, Program, Program, Program, Program), Error> {
    match is_pool_member_inner_puzzle(inner_puzzle)? {
        true => match inner_puzzle.uncurry() {
            Ok((inner_f, args)) => {
                let mut as_list: Vec<Program> = args.as_list().into_iter().take(5).collect();
                if as_list.len() < 5 {
                    return Err(Error::new(
                        ErrorKind::Other,
                        "Failed to unpack inner puzzle",
                    ));
                }
                let escape_puzzlehash = as_list.remove(4);
                let pool_reward_prefix = as_list.remove(3);
                let owner_pubkey = as_list.remove(2);
                let p2_singleton_hash = as_list.remove(1);
                let target_puzzle_hash = as_list.remove(0);
                Ok((
                    inner_f,
                    target_puzzle_hash,
                    p2_singleton_hash,
                    owner_pubkey,
                    pool_reward_prefix,
                    escape_puzzlehash,
                ))
            }
            Err(_) => Err(Error::new(
                ErrorKind::Other,
                "Failed to unpack inner puzzle",
            )),
        },
        false => Err(Error::new(
            ErrorKind::Other,
            "Attempting to unpack a non-waitingroom inner puzzle",
        )),
    }
}

pub fn uncurry_pool_waitingroom_inner_puzzle(
    inner_puzzle: &Program,
) -> Result<(Program, Program, Program, Program), Error> {
    match is_pool_waitingroom_inner_puzzle(inner_puzzle)? {
        false => Err(Error::new(
            ErrorKind::Other,
            "Attempting to unpack a non-waitingroom inner puzzle",
        )),
        true => match inner_puzzle.uncurry() {
            Ok((_, args)) => {
                let as_list = args.as_list();
                if as_list.len() < 5 {
                    return Err(Error::new(
                        ErrorKind::Other,
                        "Failed to unpack inner puzzle",
                    ));
                }
                let target_puzzle_hash = as_list[0].clone();
                let p2_singleton_hash = as_list[1].clone();
                let owner_pubkey = as_list[2].clone();
                let relative_lock_height = as_list[4].clone();
                Ok((
                    target_puzzle_hash,
                    relative_lock_height,
                    owner_pubkey,
                    p2_singleton_hash,
                ))
            }
            Err(e) => Err(Error::new(
                ErrorKind::Other,
                format!("Failed to unpack inner puzzle: {:?}", e),
            )),
        },
    }
}

pub fn get_inner_puzzle_from_puzzle(full_puzzle: &Program) -> Result<Option<Program>, Error> {
    info!("Full Puz: {}", hex::encode(&full_puzzle.serialized));
    match full_puzzle.uncurry() {
        Ok((_, args)) => {
            let list: Vec<Program> = args.as_list();
            if list.len() < 2 {
                return Ok(None);
            }
            if !is_pool_singleton_inner_puzzle(&list[1])? {
                return Ok(None);
            }
            Ok(Some(list[1].clone()))
        }
        Err(error) => Err(error),
    }
}

pub fn pool_state_from_extra_data(extra_data: Program) -> Result<Option<PoolState>, Error> {
    let mut state_bytes: Option<Vec<u8>> = None;
    match extra_data.to_map() {
        Ok(extra_data) => {
            for (key, value) in extra_data {
                let key_vec = key.as_vec().unwrap_or_default();
                if key_vec.len() == 1 && key_vec == b"p".to_vec() {
                    state_bytes = Some(value.as_vec().unwrap_or_default());
                    break;
                }
            }
            match state_bytes {
                Some(byte_data) => {
                    let mut cursor = Cursor::new(byte_data);
                    Ok(Some(PoolState::from_bytes(&mut cursor)?))
                }
                None => Ok(None),
            }
        }
        Err(error) => Err(error),
    }
}

pub fn solution_to_pool_state(coin_solution: &CoinSpend) -> Result<Option<PoolState>, Error> {
    let full_solution = Program::new(coin_solution.solution.to_bytes());
    let extra_data: Program;
    if coin_solution.coin.puzzle_hash == *SINGLETON_LAUNCHER_HASH {
        //Launcher spend
        extra_data = full_solution.rest()?.rest()?.first()?;
        return pool_state_from_extra_data(extra_data);
    }
    // Not launcher spend
    let inner_solution: Program = full_solution.rest()?.rest()?.first()?;
    // Spend which is not absorb, and is not the launcher
    let inner_map = inner_solution.clone().to_map()?;
    let num_args = inner_map.len();
    //TODO assert num_args in (2, 3); //Check arg length
    if num_args == 2 {
        if inner_solution.rest()?.first()?.as_int()? != Zero::zero() {
            // pool member
            return Ok(None);
        }
        extra_data = inner_solution.first()?;
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
        extra_data = rest.first()?;
        pool_state_from_extra_data(extra_data)
    } else {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            format!("Invalid Arg Length {num_args}, expected 2 or 3"),
        ));
    }
}

pub fn pool_state_to_inner_puzzle(
    pool_state: &PoolState,
    launcher_id: &Bytes32,
    genesis_challenge: &Bytes32,
    delay_time: u64,
    delay_ph: &Bytes32,
) -> Result<Program, Error> {
    let escaping_inner_puzzle: Program = create_waiting_room_inner_puzzle(
        &pool_state.target_puzzle_hash,
        pool_state.relative_lock_height,
        &pool_state.owner_pubkey,
        launcher_id,
        genesis_challenge,
        delay_time,
        delay_ph,
    )?;
    match pool_state.state {
        //Self Pooling
        1 => Ok(escaping_inner_puzzle),
        //Leaving Pool
        2 => Ok(escaping_inner_puzzle),
        //Pooling
        _ => create_pooling_inner_puzzle(
            &pool_state.target_puzzle_hash,
            &escaping_inner_puzzle.tree_hash(),
            &pool_state.owner_pubkey,
            launcher_id,
            genesis_challenge,
            delay_time,
            delay_ph,
        ),
    }
}

pub fn validate_puzzle_hash(
    launcher_id: &Bytes32,
    delay_ph: &Bytes32,
    delay_time: u64,
    pool_state: &PoolState,
    outer_puzzle_hash: &Bytes32,
    genesis_challenge: &Bytes32,
) -> Result<bool, Error> {
    let inner_puzzle: Program = pool_state_to_inner_puzzle(
        pool_state,
        launcher_id,
        genesis_challenge,
        delay_time,
        delay_ph,
    )?;
    let new_full_puzzle: Program = create_full_puzzle(&inner_puzzle, launcher_id)?;
    let tree_hash = new_full_puzzle.tree_hash();
    Ok(tree_hash == *outer_puzzle_hash)
}
