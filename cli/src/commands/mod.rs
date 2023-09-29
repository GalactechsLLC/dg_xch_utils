use dg_xch_clients::api::full_node::FullnodeAPI;
use dg_xch_clients::rpc::full_node::FullnodeClient;
use dg_xch_core::blockchain::coin_record::CoinRecord;
use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes48, Bytes96, SizedBytes};
use dg_xch_core::plots::PlotNft;
use dg_xch_puzzles::clvm_puzzles::{get_most_recent_singleton_coin_from_coin_spend, launcher_coin_spend_to_extra_data, solution_to_pool_state, SINGLETON_LAUNCHER_HASH, get_delay_puzzle_info_from_launcher_spend, pool_state_to_inner_puzzle, create_full_puzzle, create_travel_spend};
use std::io::{Error, ErrorKind};
use std::time::{SystemTime, UNIX_EPOCH};
use blst::min_pk::{AggregateSignature, PublicKey, SecretKey, Signature};
use dg_xch_core::blockchain::coin_spend::{CoinSpend, compute_additions_with_cost};
use dg_xch_core::consensus::constants::ConsensusConstants;
use dg_xch_core::pool::PoolState;
use dg_xch_keys::{master_sk_to_wallet_sk_unhardened};
use dg_xch_puzzles::p2_delegated_puzzle_or_hidden_puzzle::puzzle_hash_for_pk;
use num_traits::cast::ToPrimitive;
use dg_xch_core::blockchain::spend_bundle::SpendBundle;
use dg_xch_core::blockchain::transaction_record::{TransactionRecord, TransactionType};
use dg_xch_core::blockchain::utils::pkm_pairs_for_conditions_dict;
use dg_xch_core::clvm::bls_bindings;
use dg_xch_core::clvm::bls_bindings::{aggregate_verify_signature, verify_signature};
use dg_xch_core::clvm::condition_utils::conditions_dict_for_solution;
use crate::wallet_commands::{find_owner_key, generate_fee_transaction};

pub async fn scrounge_for_plotnft_by_key(
    client: &FullnodeClient,
    master_secret_key: &SecretKey
) -> Result<Vec<PlotNft>, Error> {

    let mut page = 0;
    let mut plotnfs = vec![];
    while page < 15 && plotnfs.is_empty() {
        let mut puzzle_hashes = vec![];
        for index in page*50..(page+1)*50 {
            let wallet_sk = master_sk_to_wallet_sk_unhardened(master_secret_key, index).map_err(|e| {
                Error::new(
                    ErrorKind::InvalidInput,
                    format!("Failed to parse Wallet SK: {:?}", e),
                )
            })?;
            let pub_key: Bytes48 = wallet_sk.sk_to_pk().to_bytes().into();
            let ph = puzzle_hash_for_pk(&pub_key)?;
            puzzle_hashes.push(ph);
        }
        plotnfs.extend(scrounge_for_plotnfts(client, &puzzle_hashes).await?);
        page+=1;
    }
    Ok(plotnfs)
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

pub async fn get_pool_state(
    client: &FullnodeClient,
    launcher_id: &Bytes32,
) -> Result<PoolState, Error> {
    if let Some(plotnft) = get_plotnft_by_launcher_id(client, launcher_id).await? {
        Ok(plotnft.pool_state)
    } else {
        Err(Error::new(ErrorKind::NotFound, format!("Failed to find pool state for launcher_id {}", launcher_id)))
    }
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
                    launcher_id: *launcher_id,
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

pub async fn generate_travel_transaction(client: &FullnodeClient, plot_nft: &PlotNft, master_secret_key: &SecretKey, target_pool_state: &PoolState, fee: u64, constants: &ConsensusConstants) -> Result<(TransactionRecord, Option<TransactionRecord>), Error> {
    let launcher_coin = client.get_coin_record_by_name(&plot_nft.singleton_coin.coin.parent_coin_info).await?.ok_or_else(|| {
        Error::new(
            ErrorKind::Other,
            "Failed to load launcher_coin",
        )
    })?;
    let last_record = client.get_coin_record_by_name(&plot_nft.singleton_coin.coin.parent_coin_info).await?.ok_or_else(|| {
        Error::new(
            ErrorKind::Other,
            "Failed to load last spend record:",
        )
    })?;
    let last_coin_spend = client.get_coin_spend(&last_record).await?;
    let (delayed_seconds, delayed_puzhash) = get_delay_puzzle_info_from_launcher_spend(&last_coin_spend)?;
    let new_inner_puzzle = pool_state_to_inner_puzzle(
        target_pool_state,
        &launcher_coin.coin.name(),
        &constants.genesis_challenge,
        delayed_seconds,
        &delayed_puzhash,
    )?;
    let new_full_puzzle = create_full_puzzle(&new_inner_puzzle, &launcher_coin.coin.name())?;
    let (outgoing_coin_spend, inner_puzzle) = create_travel_spend(
        &last_coin_spend,
        &launcher_coin.coin,
        &plot_nft.pool_state,
        target_pool_state,
        &constants.genesis_challenge,
        delayed_seconds,
        &delayed_puzhash,
    )?;
    let (additions, _cost) = compute_additions_with_cost(&last_coin_spend, constants.max_block_cost_clvm.to_u64().unwrap())?;
    let singleton = &additions[0];
    let singleton_id = singleton.name();
    assert_eq!(outgoing_coin_spend.coin.parent_coin_info, last_coin_spend.coin.name());
    assert_eq!(outgoing_coin_spend.coin.name(), singleton_id);
    assert_ne!(new_inner_puzzle, inner_puzzle);
    let mut signed_spend_bundle = sign(outgoing_coin_spend, |_| {
        find_owner_key(master_secret_key, &plot_nft.pool_state.owner_pubkey, 500)
    } , constants).await?;
    assert_eq!(signed_spend_bundle.removals()[0].puzzle_hash, singleton.puzzle_hash);
    assert_eq!(signed_spend_bundle.removals()[0].name(), singleton.name());
    let fee_tx: Option<TransactionRecord> = None;
    if fee > 0 {
        let fee_tx =  generate_fee_transaction(master_secret_key, fee, &Default::default(), None, constants).await?;
        if let Some(fee_bundle) = fee_tx.spend_bundle{
            signed_spend_bundle = SpendBundle::aggregate(vec![signed_spend_bundle, fee_bundle]).map_err(|e| {
                Error::new(
                    ErrorKind::Other,
                    format!("Failed to parse Public key: {:?}", e),
                )
            })?;
        }
    }
    let additions = signed_spend_bundle.additions()?;
    let removals = signed_spend_bundle.removals();
    let name = signed_spend_bundle.name();
    let tx_record = TransactionRecord {
        confirmed_at_height: 0,
        created_at_time: SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs(),
        to_puzzle_hash: new_full_puzzle.tree_hash(),
        amount: 1,
        fee_amount: fee,
        confirmed: false,
        sent: 0,
        spend_bundle: Some(signed_spend_bundle),
        additions,
        removals,
        wallet_id: 1,
        sent_to: vec![],
        trade_id: None,
        memos: vec![],
        transaction_type: TransactionType::OutgoingTx as u32,
        name
    };
    Ok((tx_record, fee_tx))
}

pub async fn sign<F>(coin_spend: CoinSpend, key_fn: F, constants: &ConsensusConstants) -> Result<SpendBundle, Error>
where F: Fn(&Bytes48) -> Result<SecretKey, Error> {
    sign_coin_spends(
        vec![coin_spend],
        key_fn,
        &constants.agg_sig_me_additional_data,
        constants.max_block_cost_clvm.to_u64().unwrap(),
    ).await
}

pub async fn sign_coin_spends<F>(coin_spends: Vec<CoinSpend>, key_fn: F, additional_data: &[u8], max_cost: u64) -> Result<SpendBundle, Error>
where F: Fn(&Bytes48) -> Result<SecretKey, Error> {
    let mut signatures: Vec<Signature> = vec![];
    let mut pk_list: Vec<Bytes48> = vec![];
    let mut msg_list: Vec<Vec<u8>> = vec![];
    for coin_spend in &coin_spends {
        //Get AGG_SIG conditions
        let conditions_dict = conditions_dict_for_solution(&coin_spend.puzzle_reveal, &coin_spend.solution, max_cost)?.0;
        //Create signature
        for (pk_bytes, msg) in pkm_pairs_for_conditions_dict(conditions_dict, coin_spend.coin.name(), additional_data)? {
            let pk = PublicKey::from_bytes(pk_bytes.as_slice()).map_err(|e| {
                Error::new(
                    ErrorKind::Other,
                    format!("Failed to parse Public key: {}, {:?}", hex::encode(pk_bytes), e),
                )
            })?;
            let secret_key = (key_fn)(&pk_bytes)?;
            assert_eq!(&secret_key.sk_to_pk(), &pk);
            let signature = bls_bindings::sign(&secret_key, &msg);
            assert!(verify_signature(&pk, &msg, &signature));
            pk_list.push(pk_bytes);
            msg_list.push(msg);
            signatures.push(signature);
        }
    }
    //Aggregate signatures
    let sig_refs: Vec<&Signature> = signatures.iter().collect();
    let pk_list: Vec<&Bytes48> = pk_list.iter().collect();
    let msg_list: Vec<&[u8]> = msg_list.iter().map(|v| v.as_slice()).collect();
    let aggsig = AggregateSignature::aggregate(&sig_refs, true).map_err(|e| {
        Error::new(
            ErrorKind::Other,
            format!("Failed to aggregate signatures: {:?}", e),
        )
    })?;
    assert!(aggregate_verify_signature(&pk_list, &msg_list, &aggsig.to_signature()));
    Ok(SpendBundle{
        coin_spends,
        aggregated_signature: Bytes96::from(aggsig.to_signature()),
    })
}