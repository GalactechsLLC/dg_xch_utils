use crate::farmer::config::Config;
use crate::{HEADERS, PROTOCOL_VERSION};
use blst::min_pk::SecretKey;
use dg_xch_clients::api::pool::PoolClient;
use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes48};
use dg_xch_core::clvm::bls_bindings::{sign, verify_signature};
use dg_xch_core::config::PoolWalletConfig;
use dg_xch_core::protocols::farmer::{FarmerPoolState, FarmerSharedState};
use dg_xch_core::protocols::pool::{
    AuthenticationPayload, GetFarmerRequest, GetFarmerResponse, PoolError, PoolErrorCode,
    PostFarmerPayload, PostFarmerRequest, PostFarmerResponse, PutFarmerPayload, PutFarmerRequest,
    PutFarmerResponse, get_current_authentication_token,
};
use dg_xch_core::traits::SizedBytes;
use dg_xch_core::utils::hash_256;
use dg_xch_keys::parse_payout_address;
use dg_xch_serialize::ChiaSerialize;
use log::{debug, error, info, warn};
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::io::Error;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant, SystemTime};
use tokio::sync::RwLock;

const UPDATE_POOL_INFO_INTERVAL: u64 = 600;
const UPDATE_POOL_INFO_FAILURE_RETRY_INTERVAL: u64 = 120;
const UPDATE_POOL_FARMER_INFO_INTERVAL: u64 = 300;

pub async fn pool_updater<T, C: Clone>(
    shared_state: Arc<FarmerSharedState<T>>,
    config: Arc<RwLock<Config<C>>>,
) {
    let mut last_update = Instant::now();
    let mut first = true;
    let pool_client = match config.read().await.pool_client() {
        Ok(client) => Arc::new(client),
        Err(error) => {
            error!("Failed to initialize pool client: {error}");
            return;
        }
    };
    loop {
        if !shared_state.signal.load(Ordering::Relaxed) {
            break;
        } else if first
            || shared_state.force_pool_update.load(Ordering::Relaxed)
            || Instant::now().duration_since(last_update).as_secs() >= 60
        {
            debug!("Updating Pool State");
            if let Err(e) = update_pool_state(
                pool_client.clone(),
                &*config.read().await,
                shared_state.clone(),
            )
            .await
            {
                error!("Error updating Pool State: {e}");
                tokio::time::sleep(Duration::from_secs(10)).await;
            } else {
                first = false;
                last_update = Instant::now();
                shared_state.last_pool_update.store(
                    SystemTime::now()
                        .duration_since(SystemTime::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs(),
                    Ordering::Relaxed,
                );
                shared_state
                    .force_pool_update
                    .store(false, Ordering::Relaxed);
            }
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    info!("Pool Handle Stopped");
}

pub async fn get_farmer<T: PoolClient + Sized + Sync + Send>(
    pool_config: &PoolWalletConfig,
    authentication_token_timeout: u8,
    authentication_sk: &SecretKey,
    client: Arc<T>,
    mut headers: HashMap<String, String>,
    chia_version: impl AsyncFn() -> Option<String>,
) -> Result<GetFarmerResponse, PoolError> {
    let authentication_token = get_current_authentication_token(authentication_token_timeout)?;
    let msg = AuthenticationPayload {
        method_name: "get_farmer".to_string(),
        launcher_id: pool_config.launcher_id,
        target_puzzle_hash: pool_config.target_puzzle_hash,
        authentication_token,
    }
    .to_bytes(PROTOCOL_VERSION)
    .map_err(|e| PoolError {
        error_code: PoolErrorCode::RequestFailed as u8,
        error_message: format!("Invalid Payload Format: {e}"),
    })?;
    let to_sign = hash_256(&msg);
    let signature = sign(authentication_sk, &to_sign);
    if !verify_signature(&authentication_sk.sk_to_pk(), &to_sign, &signature) {
        error!("Farmer GET Failed to Validate Signature");
        return Err(PoolError {
            error_code: PoolErrorCode::InvalidSignature as u8,
            error_message: "Local Failed to Validate Signature".to_string(),
        });
    }
    if let Some(v) = chia_version().await {
        headers.insert(String::from("X-chia-version"), v);
    }
    headers.extend(HEADERS.clone());
    client
        .get_farmer(
            &pool_config.pool_url,
            GetFarmerRequest {
                launcher_id: pool_config.launcher_id,
                authentication_token,
                signature: signature.to_bytes().into(),
            },
            &Some(headers),
        )
        .await
}

async fn do_auth(
    pool_config: &PoolWalletConfig,
    owner_sk: &SecretKey,
    auth_keys: &HashMap<Bytes48, SecretKey>,
) -> Result<Bytes48, PoolError> {
    if owner_sk.sk_to_pk().to_bytes() != pool_config.owner_public_key.bytes() {
        Err(PoolError {
            error_code: PoolErrorCode::ServerException as u8,
            error_message: "Owner Keys Mismatch".to_string(),
        })
    } else if let Some(auth_key) = auth_keys.get(&owner_sk.sk_to_pk().to_bytes().into()) {
        Ok(auth_key.sk_to_pk().to_bytes().into())
    } else {
        Err(PoolError {
            error_code: PoolErrorCode::NotFound as u8,
            error_message: "Auth Key Not Found".to_string(),
        })
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn post_farmer<T: PoolClient + Sized + Sync + Send>(
    pool_config: &PoolWalletConfig,
    payout_instructions: &str,
    authentication_token_timeout: u8,
    owner_sk: &SecretKey,
    auth_keys: &HashMap<Bytes48, SecretKey>,
    suggested_difficulty: Option<u64>,
    client: Arc<T>,
    mut headers: HashMap<String, String>,
    chia_version: impl AsyncFn() -> Option<String>,
) -> Result<PostFarmerResponse, PoolError> {
    let payload = PostFarmerPayload {
        launcher_id: pool_config.launcher_id,
        authentication_token: get_current_authentication_token(authentication_token_timeout)?,
        authentication_public_key: do_auth(pool_config, owner_sk, auth_keys).await?,
        payout_instructions: parse_payout_address(payout_instructions).map_err(|e| PoolError {
            error_code: PoolErrorCode::InvalidPayoutInstructions as u8,
            error_message: format!(
                "Failed to Parse Payout Instructions: {payout_instructions}, {e:?}"
            ),
        })?,
        suggested_difficulty,
    };
    let to_sign = hash_256(payload.to_bytes(PROTOCOL_VERSION).map_err(|e| PoolError {
        error_code: PoolErrorCode::RequestFailed as u8,
        error_message: format!("Invalid Payload Format: {e}"),
    })?);
    let signature = sign(owner_sk, &to_sign);
    if !verify_signature(&owner_sk.sk_to_pk(), &to_sign, &signature) {
        error!("Farmer POST Failed to Validate Signature");
        return Err(PoolError {
            error_code: PoolErrorCode::InvalidSignature as u8,
            error_message: "Local Failed to Validate Signature".to_string(),
        });
    }
    if let Some(v) = chia_version().await {
        headers.insert(String::from("X-chia-version"), v);
    }
    headers.extend(HEADERS.clone());
    client
        .post_farmer(
            &pool_config.pool_url,
            PostFarmerRequest {
                payload,
                signature: signature.to_bytes().into(),
            },
            &Some(headers),
        )
        .await
}

#[allow(clippy::too_many_arguments)]
pub async fn put_farmer<T: PoolClient + Sized + Sync + Send>(
    pool_config: &PoolWalletConfig,
    payout_instructions: &str,
    authentication_token_timeout: u8,
    owner_sk: &SecretKey,
    auth_keys: &HashMap<Bytes48, SecretKey>,
    suggested_difficulty: Option<u64>,
    client: Arc<T>,
    mut headers: HashMap<String, String>,
    chia_version: impl AsyncFn() -> Option<String>,
) -> Result<PutFarmerResponse, PoolError> {
    let authentication_public_key = do_auth(pool_config, owner_sk, auth_keys).await?;
    let payload = PutFarmerPayload {
        launcher_id: pool_config.launcher_id,
        authentication_token: get_current_authentication_token(authentication_token_timeout)?,
        authentication_public_key: Some(authentication_public_key),
        payout_instructions: parse_payout_address(payout_instructions).ok(),
        suggested_difficulty,
    };
    let to_sign = hash_256(payload.to_bytes(PROTOCOL_VERSION).map_err(|e| PoolError {
        error_code: PoolErrorCode::RequestFailed as u8,
        error_message: format!("Invalid Payload Format: {e}"),
    })?);
    let signature = sign(owner_sk, &to_sign);
    if !verify_signature(&owner_sk.sk_to_pk(), &to_sign, &signature) {
        error!("Local Failed to Validate Signature");
        return Err(PoolError {
            error_code: PoolErrorCode::InvalidSignature as u8,
            error_message: "Local Failed to Validate Signature".to_string(),
        });
    }
    let request = PutFarmerRequest {
        payload,
        signature: signature.to_bytes().into(),
    };
    if let Some(v) = chia_version().await {
        headers.insert(String::from("X-chia-version"), v);
    }
    headers.extend(HEADERS.clone());
    client
        .put_farmer(&pool_config.pool_url, request, &Some(headers))
        .await
}

pub async fn update_pool_farmer_info<T, P: PoolClient + Sized + Sync + Send>(
    pool_states: Arc<RwLock<HashMap<Bytes32, FarmerPoolState>>>,
    pool_config: &PoolWalletConfig,
    authentication_token_timeout: u8,
    authentication_sk: &SecretKey,
    client: Arc<P>,
    headers: HashMap<String, String>,
    shared_state: Arc<FarmerSharedState<T>>,
) -> Result<GetFarmerResponse, PoolError> {
    let response = get_farmer(
        pool_config,
        authentication_token_timeout,
        authentication_sk,
        client,
        headers,
        async move || {
            shared_state
                .upstream_handshake
                .read()
                .await
                .as_ref()
                .map(|v| v.software_version.clone())
        },
    )
    .await?;
    pool_states
        .write()
        .await
        .get_mut(&pool_config.p2_singleton_puzzle_hash)
        .ok_or_else(|| {
            Error::other(format!(
                "pool state disappeared for {}",
                pool_config.p2_singleton_puzzle_hash
            ))
        })?
        .current_difficulty = Some(response.current_difficulty);
    pool_states
        .write()
        .await
        .get_mut(&pool_config.p2_singleton_puzzle_hash)
        .ok_or_else(|| {
            Error::other(format!(
                "pool state disappeared for {}",
                pool_config.p2_singleton_puzzle_hash
            ))
        })?
        .current_points = response.current_points;
    info!(
        "Updating Pool Difficulty: {:?} ",
        pool_states
            .read()
            .await
            .get(&pool_config.p2_singleton_puzzle_hash)
            .ok_or_else(|| Error::other(format!(
                "pool state disappeared for {}",
                pool_config.p2_singleton_puzzle_hash
            )))?
            .current_difficulty
    );
    info!(
        "Updating Current Points: {:?} ",
        pool_states
            .read()
            .await
            .get(&pool_config.p2_singleton_puzzle_hash)
            .ok_or_else(|| Error::other(format!(
                "pool state disappeared for {}",
                pool_config.p2_singleton_puzzle_hash
            )))?
            .current_points
    );
    Ok(response)
}

pub async fn update_pool_state<'a, T, C: Clone, P: 'a + PoolClient + Sized + Sync + Send>(
    client: Arc<P>,
    config: &Config<C>,
    shared_state: Arc<FarmerSharedState<T>>,
) -> Result<(), Error> {
    let auth_keys = shared_state.owner_public_keys_to_auth_secret_keys.as_ref();
    let pool_states = shared_state.pool_states.clone();
    let headers = shared_state.additional_headers.as_ref().clone();
    for pool_config in &config.pool_info {
        if let Some(auth_secret_key) = auth_keys.get(&pool_config.owner_public_key) {
            if let Entry::Vacant(s) = pool_states
                .write()
                .await
                .entry(pool_config.p2_singleton_puzzle_hash)
            {
                info!(
                    "Adding Pool State for {}",
                    pool_config.p2_singleton_puzzle_hash
                );
                s.insert(FarmerPoolState {
                    points_found_since_start: 0,
                    points_found_24h: vec![],
                    points_acknowledged_since_start: 0,
                    points_acknowledged_24h: vec![],
                    next_farmer_update: Instant::now(),
                    next_pool_info_update: Instant::now(),
                    current_points: 0,
                    current_difficulty: None,
                    pool_config: None,
                    pool_errors_24h: vec![],
                    authentication_token_timeout: None,
                });
            }
            pool_states
                .write()
                .await
                .get_mut(&pool_config.p2_singleton_puzzle_hash)
                .ok_or_else(|| {
                    Error::other(format!(
                        "pool state disappeared for {}",
                        pool_config.p2_singleton_puzzle_hash
                    ))
                })?
                .pool_config = Some(pool_config.clone());
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
            let next_pool_info_update = pool_states
                .read()
                .await
                .get(&pool_config.p2_singleton_puzzle_hash)
                .ok_or_else(|| {
                    Error::other(format!(
                        "pool state disappeared for {}",
                        pool_config.p2_singleton_puzzle_hash
                    ))
                })?
                .next_pool_info_update;
            if Instant::now() >= next_pool_info_update {
                info!(
                    "Updating Pool Info {}",
                    pool_config.p2_singleton_puzzle_hash
                );
                //Makes a GET request to the pool to get the updated information
                match client.get_pool_info(&pool_config.pool_url).await {
                    Ok(pool_info) => {
                        if pool_info.authentication_token_timeout == 0
                            || pool_info.minimum_difficulty == 0
                            || pool_info.protocol_version
                                != pool_config.pooling_version.protocol_number()
                            || pool_info.target_puzzle_hash != pool_config.target_puzzle_hash
                        {
                            return Err(Error::new(
                                std::io::ErrorKind::InvalidData,
                                "pool returned invalid limits, protocol version, or target",
                            ));
                        }
                        pool_states
                            .write()
                            .await
                            .get_mut(&pool_config.p2_singleton_puzzle_hash)
                            .ok_or_else(|| {
                                Error::other(format!(
                                    "pool state disappeared for {}",
                                    pool_config.p2_singleton_puzzle_hash
                                ))
                            })?
                            .authentication_token_timeout =
                            Some(pool_info.authentication_token_timeout);
                        // Only update the first time from GET /pool_info, gets updated from GET /farmer later
                        let is_first = pool_states
                            .read()
                            .await
                            .get(&pool_config.p2_singleton_puzzle_hash)
                            .ok_or_else(|| {
                                Error::other(format!(
                                    "pool state disappeared for {}",
                                    pool_config.p2_singleton_puzzle_hash
                                ))
                            })?
                            .current_difficulty
                            .is_none();
                        if is_first {
                            pool_states
                                .write()
                                .await
                                .get_mut(&pool_config.p2_singleton_puzzle_hash)
                                .ok_or_else(|| {
                                    Error::other(format!(
                                        "pool state disappeared for {}",
                                        pool_config.p2_singleton_puzzle_hash
                                    ))
                                })?
                                .current_difficulty = Some(pool_info.minimum_difficulty);
                        }
                        pool_states
                            .write()
                            .await
                            .get_mut(&pool_config.p2_singleton_puzzle_hash)
                            .ok_or_else(|| {
                                Error::other(format!(
                                    "pool state disappeared for {}",
                                    pool_config.p2_singleton_puzzle_hash
                                ))
                            })?
                            .next_pool_info_update =
                            Instant::now() + Duration::from_secs(UPDATE_POOL_INFO_INTERVAL);
                    }
                    Err(e) => {
                        pool_states
                            .write()
                            .await
                            .get_mut(&pool_config.p2_singleton_puzzle_hash)
                            .ok_or_else(|| {
                                Error::other(format!(
                                    "pool state disappeared for {}",
                                    pool_config.p2_singleton_puzzle_hash
                                ))
                            })?
                            .next_pool_info_update = Instant::now()
                            + Duration::from_secs(UPDATE_POOL_INFO_FAILURE_RETRY_INTERVAL);
                        error!("Update Pool Info Error: {e:?}");
                    }
                }
            } else {
                debug!("Not Ready for Update");
            }
            let next_farmer_update = pool_states
                .read()
                .await
                .get(&pool_config.p2_singleton_puzzle_hash)
                .ok_or_else(|| {
                    Error::other(format!(
                        "pool state disappeared for {}",
                        pool_config.p2_singleton_puzzle_hash
                    ))
                })?
                .next_farmer_update;
            if Instant::now() >= next_farmer_update {
                info!(
                    "Updating Pool Info {}",
                    pool_config.p2_singleton_puzzle_hash
                );
                pool_states
                    .write()
                    .await
                    .get_mut(&pool_config.p2_singleton_puzzle_hash)
                    .ok_or_else(|| {
                        Error::other(format!(
                            "pool state disappeared for {}",
                            pool_config.p2_singleton_puzzle_hash
                        ))
                    })?
                    .next_farmer_update =
                    Instant::now() + Duration::from_secs(UPDATE_POOL_FARMER_INFO_INTERVAL);
                let authentication_token_timeout = pool_states
                    .read()
                    .await
                    .get(&pool_config.p2_singleton_puzzle_hash)
                    .ok_or_else(|| {
                        Error::other(format!(
                            "pool state disappeared for {}",
                            pool_config.p2_singleton_puzzle_hash
                        ))
                    })?
                    .authentication_token_timeout;
                if let Some(authentication_token_timeout) = authentication_token_timeout {
                    info!("Running Farmer Pool Update");
                    if let Err(error) = update_pool_farmer_info(
                        pool_states.clone(),
                        pool_config,
                        authentication_token_timeout,
                        auth_secret_key,
                        client.clone(),
                        headers.clone(),
                        shared_state.clone(),
                    )
                    .await
                    {
                        warn!(
                            "Pool account could not be read: {error:?}. Registration and settings changes require explicit user action; no pool values were changed."
                        );
                    }
                } else {
                    warn!(
                        "No pool specific authentication_token_timeout has been set for {}, check communication with the pool.",
                        pool_config.p2_singleton_puzzle_hash
                    );
                }
            }
        } else {
            warn!(
                "Could not find pool authentication key for: {:?}",
                pool_config.owner_public_key
            );
        }
    }
    Ok(())
}
