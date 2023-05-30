use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use blst::min_pk::SecretKey;
use log::{debug, error, info, warn};
use dg_xch_clients::api::pool::{DefaultPoolClient, PoolClient};
use dg_xch_clients::protocols::pool::{AuthenticationPayload, get_current_authentication_token, GetFarmerRequest, GetFarmerResponse, GetPoolInfoResponse, PoolError, PoolErrorCode, PostFarmerPayload, PostFarmerRequest, PostFarmerResponse, PutFarmerPayload, PutFarmerRequest, PutFarmerResponse};
use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes48};
use dg_xch_core::clvm::bls_bindings::sign;
use dg_xch_serialize::{ChiaSerialize, hash_256};
use crate::models::config::{Config, PoolWalletConfig};

const UPDATE_POOL_INFO_INTERVAL: u64 = 600;
const UPDATE_POOL_INFO_FAILURE_RETRY_INTERVAL: u64 = 120;
const UPDATE_POOL_FARMER_INFO_INTERVAL: u64 = 300;

pub async fn pool_updater(shutdown_trigger: &AtomicBool, config: Arc<Config>) {
    let mut last_update = Instant::now();
    let mut first = true;
    let pool_client = Arc::new(DefaultPoolClient::new());
    let auth_keys = Default::default();
    let owner_keys = Default::default();
    let mut pool_states = Default::default();
    loop {
        if !shutdown_trigger.load(Ordering::Relaxed) {
            break;
        }
        if first || Instant::now().duration_since(last_update).as_secs() >= 60 {
            first = false;
            info!("Updating Pool State");
            update_pool_state(
                &auth_keys,
                &owner_keys,
                &mut pool_states,
                pool_client.clone(),
                config.clone()
            ).await;
            last_update = Instant::now();
        }
        tokio::time::sleep(Duration::from_secs(1)).await
    }
    info!("Pool Handle Stopped");
}
pub async fn get_pool_info(pool_config: &PoolWalletConfig) -> Option<GetPoolInfoResponse> {
    match reqwest::get(format!("{}/pool_info", pool_config.pool_url)).await {
        Ok(resp) => match resp.status() {
            reqwest::StatusCode::OK => match resp.text().await {
                Ok(body) => match serde_json::from_str(body.as_str()) {
                    Ok(c) => {
                        return Some(c);
                    }
                    Err(e) => {
                        warn!("Failed to load Pool Info, Invalid Json: {:?}, {}", e, body);
                    }
                },
                Err(e) => {
                    warn!("Failed to load Pool Info, Invalid Body: {:?}", e);
                }
            },
            _ => {
                warn!(
                    "Failed to load Pool Info, Bad Status Code: {:?}, {}",
                    resp.status(),
                    resp.text().await.unwrap_or_default()
                );
            }
        },
        Err(e) => {
            warn!("Failed to load Pool Info: {:?}", e);
        }
    }
    None
}

pub async fn get_farmer<T: PoolClient + Sized + Sync + Send>(
    pool_config: &PoolWalletConfig,
    authentication_token_timeout: u8,
    authentication_sk: &SecretKey,
    client: Arc<T>,
) -> Result<GetFarmerResponse, PoolError> {
    let authentication_token = get_current_authentication_token(authentication_token_timeout);
    let signature = sign(
        authentication_sk,
        hash_256(
            AuthenticationPayload {
                method_name: "get_farmer".to_string(),
                launcher_id: pool_config.launcher_id.clone(),
                target_puzzle_hash: pool_config.target_puzzle_hash.clone(),
                authentication_token,
            }
                .to_bytes(),
        )
            .as_slice(),
    );
    client
        .get_farmer(
            &pool_config.pool_url,
            GetFarmerRequest {
                launcher_id: pool_config.launcher_id.clone(),
                authentication_token,
                signature: signature.to_bytes().into(),
            },
        )
        .await
}

async fn do_auth(
    auth_keys: &HashMap<Bytes32, SecretKey>,
    pool_config: &PoolWalletConfig,
    owner_sk: &SecretKey,
) -> Result<Bytes48, PoolError> {
    if owner_sk.sk_to_pk().to_bytes() != pool_config.owner_public_key.to_sized_bytes() {
        return Err(PoolError {
            error_code: PoolErrorCode::ServerException as u8,
            error_message: "Owner Keys Mismatch".to_string(),
        });
    }
    if let Some(s) = auth_keys.get(&pool_config.p2_singleton_puzzle_hash) {
        Ok(s.sk_to_pk().to_bytes().into())
    } else {
        Err(PoolError {
            error_code: PoolErrorCode::ServerException as u8,
            error_message: "Authentication Public Key Not Found".to_string(),
        })
    }
}

pub async fn post_farmer<T: PoolClient + Sized + Sync + Send>(
    auth_keys: &HashMap<Bytes32, SecretKey>,
    pool_config: &PoolWalletConfig,
    authentication_token_timeout: u8,
    owner_sk: &SecretKey,
    client: Arc<T>,
) -> Result<PostFarmerResponse, PoolError> {
    let payload = PostFarmerPayload {
        launcher_id: pool_config.launcher_id.clone(),
        authentication_token: get_current_authentication_token(authentication_token_timeout),
        authentication_public_key: do_auth(auth_keys, pool_config, owner_sk).await?,
        payout_instructions: pool_config.payout_instructions.clone(),
        suggested_difficulty: None,
    };
    let signature = sign(owner_sk, &hash_256(payload.to_bytes()));
    client
        .post_farmer(
            &pool_config.pool_url,
            PostFarmerRequest {
                payload,
                signature: signature.to_bytes().into(),
            },
        )
        .await
}

pub async fn put_farmer<T: PoolClient + Sized + Sync + Send>(
    auth_keys: &HashMap<Bytes32, SecretKey>,
    pool_config: &PoolWalletConfig,
    authentication_token_timeout: u8,
    owner_sk: &SecretKey,
    client: Arc<T>,
) -> Result<PutFarmerResponse, PoolError> {
    let authentication_public_key = do_auth(auth_keys, pool_config, owner_sk).await?;
    let payload = PutFarmerPayload {
        launcher_id: pool_config.launcher_id.clone(),
        authentication_token: get_current_authentication_token(authentication_token_timeout),
        authentication_public_key: Some(authentication_public_key),
        payout_instructions: Some(pool_config.payout_instructions.clone()),
        suggested_difficulty: None,
    };
    let signature = sign(owner_sk, &hash_256(payload.to_bytes()));
    let request = PutFarmerRequest {
        payload,
        signature: signature.to_bytes().into(),
    };
    client.put_farmer(&pool_config.pool_url, request).await
}

pub async fn update_pool_farmer_info<T: PoolClient + Sized + Sync + Send>(
    pool_state: &mut FarmerPoolState,
    pool_config: &PoolWalletConfig,
    authentication_token_timeout: u8,
    authentication_sk: &SecretKey,
    client: Arc<T>,
) -> Result<GetFarmerResponse, PoolError> {
    let response = get_farmer(
        pool_config,
        authentication_token_timeout,
        authentication_sk,
        client,
    ).await?;
    pool_state.current_difficulty = Some(response.current_difficulty);
    pool_state.current_points = response.current_points;
    info!(
        "Updating Pool Difficulty: {:?} ",
        pool_state.current_difficulty
    );
    info!("Updating Current Points: {:?} ", pool_state.current_points);
    Ok(response)
}

pub async fn update_pool_state<'a, T: 'a + PoolClient + Sized + Sync + Send>(
    auth_keys: &HashMap<Bytes32, SecretKey>,
    owner_keys: &HashMap<Bytes48, SecretKey>,
    pool_states: &mut HashMap<Bytes32, FarmerPoolState>,
    pool_client: Arc<T>,
    config: Arc<Config>,
) {
    for pool_config in &config.pool_info {
        if let Some(auth_secret_key) = auth_keys.get(&pool_config.p2_singleton_puzzle_hash) {
            let mut pool_state = match pool_states.get(&pool_config.p2_singleton_puzzle_hash) {
                Some(s)  => s,
                None => {
                    let s = FarmerPoolState {
                        points_found_since_start: 0,
                        points_found_24h: vec![],
                        points_acknowledged_since_start: 0,
                        points_acknowledged_24h: vec![],
                        next_farmer_update: Instant::now(),
                        next_pool_info_update: Instant::now(),
                        current_points: 0,
                        current_difficulty: None,
                        pool_errors_24h: vec![],
                        authentication_token_timeout: None,
                    };
                    pool_states.insert(
                        pool_config.p2_singleton_puzzle_hash.clone(),
                        s.clone(),
                    );
                    info!("Added pool: {:?}", pool_config);
                    s
                }
            };
            if pool_config.pool_url.is_empty() {
                continue;
            }
            if config.selected_network == "mainnet" && !pool_config.pool_url.starts_with("https") {
                error!(
                    "Pool URLs must be HTTPS on mainnet {}",
                    pool_config.pool_url
                );
                continue;
            }
            if Instant::now() >= pool_state.next_pool_info_update {
                pool_state.next_pool_info_update =
                    Instant::now() + Duration::from_secs(UPDATE_POOL_INFO_INTERVAL);
                //Makes a GET request to the pool to get the updated information
                let pool_info = get_pool_info(pool_config).await;
                if let Some(pool_info) = pool_info {
                    pool_state.authentication_token_timeout =
                        Some(pool_info.authentication_token_timeout);
                    // Only update the first time from GET /pool_info, gets updated from GET /farmer later
                    if pool_state.current_difficulty.is_none() {
                        pool_state.current_difficulty = Some(pool_info.minimum_difficulty);
                    }
                } else {
                    pool_state.next_pool_info_update = Instant::now()
                        + Duration::from_secs(UPDATE_POOL_INFO_FAILURE_RETRY_INTERVAL);
                    error!("Update Pool Info Error");
                }
            } else {
                debug!("Not Ready for Update");
            }
            if Instant::now() >= pool_state.next_farmer_update {
                pool_state.next_farmer_update =
                    Instant::now() + Duration::from_secs(UPDATE_POOL_FARMER_INFO_INTERVAL);
                if let Some(authentication_token_timeout) = pool_state.authentication_token_timeout
                {
                    let farmer_info = match update_pool_farmer_info(
                            &mut pool_state,
                            pool_config,
                            authentication_token_timeout,
                            &auth_secret_key,
                            pool_client.clone(),
                        )
                        .await
                    {
                        Ok(resp) => Some(resp),
                        Err(e) => {
                            if e.error_code == PoolErrorCode::FarmerNotKnown as u8 {
                                match &owner_keys.get(&pool_config.owner_public_key.to_sized_bytes().into()) {
                                    None => {
                                        error!(
                                            "Could not find Owner SK for {}",
                                            &pool_config.owner_public_key
                                        );
                                        continue;
                                    }
                                    Some(sk) => {
                                        match post_farmer(
                                                auth_keys,
                                                pool_config,
                                                authentication_token_timeout,
                                                sk,
                                                pool_client.clone(),
                                            )
                                            .await
                                        {
                                            Ok(resp) => {
                                                info!(
                                                    "Welcome message from {} : {}",
                                                    pool_config.pool_url, resp.welcome_message
                                                );
                                            }
                                            Err(e) => {
                                                error!("Failed POST farmer info. {:?}", e);
                                            }
                                        }
                                        match update_pool_farmer_info(
                                                &mut pool_state,
                                                pool_config,
                                                authentication_token_timeout,
                                                &auth_secret_key,
                                                pool_client.clone(),
                                            )
                                            .await
                                        {
                                            Ok(resp) => Some(resp),
                                            Err(e) => {
                                                error!("Failed to update farmer info after POST /farmer. {:?}", e);
                                                None
                                            }
                                        }
                                    }
                                }
                            } else if e.error_code == PoolErrorCode::InvalidSignature as u8 {
                                match &owner_keys.get(&pool_config.owner_public_key.to_sized_bytes().into()) {
                                    None => {
                                        error!(
                                            "Could not find Owner SK for {}",
                                            &pool_config.owner_public_key
                                        );
                                        continue;
                                    }
                                    Some(sk) => {
                                        let _ = put_farmer(
                                                auth_keys,
                                                pool_config,
                                                authentication_token_timeout,
                                                sk,
                                                pool_client.clone(),
                                            )
                                            .await; //Todo maybe add logging here
                                    }
                                }
                                update_pool_farmer_info(
                                    &mut pool_state,
                                    pool_config,
                                    authentication_token_timeout,
                                    &auth_secret_key,
                                    pool_client.clone(),
                                )
                                .await
                                .ok()
                            } else {
                                None
                            }
                        }
                    };
                    let payout_instructions_update_required = if let Some(info) = farmer_info {
                        pool_config.payout_instructions.to_ascii_lowercase()
                            != info.payout_instructions.to_ascii_lowercase()
                    } else {
                        false
                    };
                    if payout_instructions_update_required {
                        match &owner_keys.get(&pool_config.owner_public_key.to_sized_bytes().into()) {
                            None => {
                                error!(
                                    "Could not find Owner SK for {}",
                                    &pool_config.owner_public_key
                                );
                                continue;
                            }
                            Some(sk) => {
                                let _ = put_farmer(
                                    auth_keys,
                                    pool_config,
                                    authentication_token_timeout,
                                    sk,
                                    pool_client.clone(),
                                ).await; //Todo maybe add logging here
                            }
                        }
                    }
                } else {
                    warn!("No pool specific authentication_token_timeout has been set for {}, check communication with the pool.", &pool_config.p2_singleton_puzzle_hash);
                }
                //Update map
                pool_states.insert(pool_config.p2_singleton_puzzle_hash.clone(), pool_state);
            }
        } else {
            warn!(
                "Could not find authentication sk for: {:?}",
                &pool_config.p2_singleton_puzzle_hash
            );
        }
    }
}