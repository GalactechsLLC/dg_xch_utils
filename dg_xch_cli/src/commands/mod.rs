use dg_xch_clients::api::full_node::FullnodeAPI;
use dg_xch_clients::rpc::full_node::FullnodeClient;
use dg_xch_core::blockchain::coin_record::CoinRecord;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::plots::PlotNft;
use dg_xch_core::puzzles::clvm_puzzles::{
    get_most_recent_singleton_coin_from_coin_spend, launcher_coin_spend_to_extra_data,
    solution_to_pool_state, SINGLETON_LAUNCHER_HASH,
};
use std::io::Error;

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
            if child.puzzle_hash == *SINGLETON_LAUNCHER_HASH {
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
                    let next_coin = get_most_recent_singleton_coin_from_coin_spend(&last_spend)?;
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
