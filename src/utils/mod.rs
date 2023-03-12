pub mod clvm_puzzles;
pub mod wallet;

use crate::clients::api::full_node::FullnodeAPI;
use crate::clients::rpc::full_node::FullnodeClient;
use crate::clvm::program::{Program, SerializedProgram};
use crate::types::blockchain::coin_record::CoinRecord;
use crate::types::blockchain::sized_bytes::Bytes32;
use crate::utils::clvm_puzzles::{
    get_most_recent_singleton_coin_from_coin_spend, solution_to_pool_state,
};
use crate::utils::wallet::{launcher_coin_spend_to_extra_data, PlotNft};
use lazy_static::lazy_static;
use std::io::Error;
use tokio::select;
use tokio::signal::unix::{signal, SignalKind};

pub async fn await_termination() -> Result<(), Error> {
    let mut term_signal = signal(SignalKind::terminate())?;
    let mut int_signal = signal(SignalKind::interrupt())?;
    let mut quit_signal = signal(SignalKind::quit())?;
    let mut alarm_signal = signal(SignalKind::alarm())?;
    let mut hup_signal = signal(SignalKind::hangup())?;
    select! {
        _ = term_signal.recv() => (),
        _ = int_signal.recv() => (),
        _ = quit_signal.recv() => (),
        _ = alarm_signal.recv() => (),
        _ = hup_signal.recv() => ()
    }
    Ok(())
}
const SINGLETON_LAUNCHER_HEX: &str = "ff02ffff01ff04ffff04ff04ffff04ff05ffff04ff0bff80808080ffff04ffff04ff0affff04ffff02ff0effff04ff02ffff04ffff04ff05ffff04ff0bffff04ff17ff80808080ff80808080ff808080ff808080ffff04ffff01ff33ff3cff02ffff03ffff07ff0580ffff01ff0bffff0102ffff02ff0effff04ff02ffff04ff09ff80808080ffff02ff0effff04ff02ffff04ff0dff8080808080ffff01ff0bffff0101ff058080ff0180ff018080";
lazy_static! {
    pub static ref SINGLETON_LAUNCHER_PUZZLE: Program =
        SerializedProgram::from_hex(SINGLETON_LAUNCHER_HEX)
            .unwrap()
            .to_program()
            .unwrap();
    pub static ref SINGLETON_LAUNCHER_PUZZLE_HASH: Bytes32 = SINGLETON_LAUNCHER_PUZZLE.tree_hash();
}

pub async fn scrounge_for_plotnfts(
    client: &FullnodeClient,
    puzzle_hashes: &[Bytes32],
) -> Result<Vec<PlotNft>, Error> {
    let hashes = client
        .get_coin_records_by_puzzle_hashes(puzzle_hashes, Some(true), None, None)
        .await?;
    let spent: Vec<CoinRecord> = hashes.into_iter().filter(|c| c.spent).collect();
    let mut plotnfts = vec![];
    for spent_coin in spent {
        let coin_spend = client.get_coin_spend(&spent_coin).await?;
        for child in coin_spend.additions()? {
            if child.puzzle_hash == *SINGLETON_LAUNCHER_PUZZLE_HASH {
                let launcher_id = child.name();
                if let Some(plotnft) = get_plotnft_by_launcher_id(client, &launcher_id).await? {
                    plotnfts.push(plotnft);
                }
            }
        }
    }
    Ok(plotnfts)
}

pub async fn get_plotnft_by_launcher_id(
    client: &FullnodeClient,
    launcher_id: &Bytes32,
) -> Result<Option<PlotNft>, Error> {
    let launcher_coin = client.get_coin_record_by_name(launcher_id).await?;
    if let Some(launcher_coin) = launcher_coin {
        let spend = client.get_coin_spend(&launcher_coin).await?;
        let initial_extra_data = launcher_coin_spend_to_extra_data(&spend)?;
        let first_coin = get_most_recent_singleton_coin_from_coin_spend(&spend)?;
        if let Some(coin) = first_coin {
            let mut last_not_null_state = initial_extra_data.pool_state.clone();
            let mut singleton_coin = client.get_coin_record_by_name(&coin.name()).await?;
            while let Some(sc) = &singleton_coin {
                if sc.spent {
                    let last_spend = client.get_coin_spend(sc).await?;
                    let next_coin = get_most_recent_singleton_coin_from_coin_spend(&spend)?;
                    if let Some(pool_state) = solution_to_pool_state(&last_spend)? {
                        last_not_null_state = pool_state;
                    }
                    if let Some(nc) = next_coin {
                        singleton_coin = client.get_coin_record_by_name(&nc.name()).await?;
                    } else {
                        break; //Error?
                    }
                } else {
                    break;
                }
            }
            if let Some(singleton_coin) = singleton_coin {
                Ok(Some(PlotNft {
                    launcher_id: launcher_id.clone(),
                    singleton_coin,
                    pool_state: last_not_null_state,
                    delay_time: initial_extra_data.delay_time,
                    delay_puzzle_hash: initial_extra_data.delay_puzzle_hash,
                }))
            } else {
                Ok(None)
            }
        } else {
            Ok(None)
        }
    } else {
        Ok(None)
    }
}
