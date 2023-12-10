use std::collections::HashMap;
use std::io::{Error};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;
use blst::min_pk::SecretKey;
use log::{error, info};
use tokio::sync::Mutex;
use uuid::Uuid;
use dg_xch_clients::api::pool::PoolClient;
use dg_xch_clients::websocket::farmer::FarmerClient;
use dg_xch_core::blockchain::proof_of_space::ProofOfSpace;
use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes48, hex_to_bytes};
use dg_xch_core::clvm::bls_bindings::{sign, verify_signature};
use dg_xch_core::config::PoolWalletConfig;
use dg_xch_core::protocols::{ChiaMessageFilter, ChiaMessageHandler, PeerMap, ProtocolMessageTypes};
use dg_xch_core::protocols::farmer::NewSignagePoint;
use dg_xch_core::protocols::pool::{AuthenticationPayload, get_current_authentication_token, GetFarmerRequest, GetFarmerResponse, PoolError, PoolErrorCode, PostFarmerPayload, PostFarmerRequest, PostFarmerResponse, PutFarmerPayload, PutFarmerRequest, PutFarmerResponse};
use dg_xch_keys::decode_puzzle_hash;
use dg_xch_serialize::{ChiaSerialize, hash_256};
use crate::websocket::{WebsocketServer, WebsocketServerConfig};

mod handshake;
mod new_proof_or_space;
mod respond_signatures;

use handshake::HandshakeHandle;
use crate::websocket::farmer::new_proof_or_space::NewProofOfSpaceHandle;
use crate::websocket::farmer::respond_signatures::RespondSignaturesHandle;

pub struct FarmerServerConfig {
    pub network: String,
    pub websocket: WebsocketServerConfig,
    pub farmer_reward_payout_address: Bytes32,
    pub pool_rewards_payout_address: Bytes32,
}

pub struct FarmerIdentifier {
    pub plot_identifier: String,
    pub challenge_hash: Bytes32,
    pub sp_hash: Bytes32,
    pub peer_node_id: Bytes32,
}

pub type ProofsMap = Arc<Mutex<HashMap<Bytes32, Vec<(String, ProofOfSpace)>>>>;

#[derive(Debug, Clone)]
pub struct FarmerPoolState {
    pub points_found_since_start: u64,
    pub points_found_24h: Vec<(Instant, u64)>,
    pub points_acknowledged_since_start: u64,
    pub points_acknowledged_24h: Vec<(Instant, u64)>,
    pub next_farmer_update: Instant,
    pub next_pool_info_update: Instant,
    pub current_points: u64,
    pub current_difficulty: Option<u64>,
    pub pool_config: Option<PoolWalletConfig>,
    pub pool_errors_24h: Vec<(Instant, String)>,
    pub authentication_token_timeout: Option<u8>,
}

pub struct FarmerServer<T> {
    pub server: WebsocketServer,
    pub peers: PeerMap,
    pub signage_points: Arc<Mutex<HashMap<Bytes32, Vec<NewSignagePoint>>>>,
    pub quality_to_identifiers: Arc<Mutex<HashMap<Bytes32, FarmerIdentifier>>>,
    pub proofs_of_space: ProofsMap,
    pub cache_time: Arc<Mutex<HashMap<Bytes32, Instant>>>,
    pub pool_state: Arc<Mutex<HashMap<Bytes32, FarmerPoolState>>>,
    pub farmer_public_keys: Arc<Mutex<HashMap<Bytes48, SecretKey>>>,
    pub farmer_private_keys: Arc<Mutex<Vec<SecretKey>>>,
    pub pool_public_keys: Arc<Mutex<HashMap<Bytes48, SecretKey>>>,
    pub owner_secret_keys: Arc<Mutex<HashMap<Bytes48, SecretKey>>>,
    pub full_node_client: Arc<Mutex<Option<FarmerClient>>>,
    pub pool_client: Arc<T>,
    pub additional_headers: Arc<HashMap<String, String>>,
    pub config: Arc<FarmerServerConfig>
}
impl<T: PoolClient + Sized + Sync + Send + 'static> FarmerServer<T> {
    pub fn new(config: FarmerServerConfig, pool_client: Arc<T>, additional_headers: Arc<HashMap<String, String>>) -> Result<Self, Error> {
        let peers = Arc::new(Mutex::new(HashMap::new()));
        let config = Arc::new(config);
        let signage_points: Arc<Mutex<HashMap<Bytes32, Vec<NewSignagePoint>>>> = Default::default();
        let quality_to_identifiers: Arc<Mutex<HashMap<Bytes32, FarmerIdentifier>>> = Default::default();
        let proofs_of_space: ProofsMap  = Default::default();
        let cache_time: Arc<Mutex<HashMap<Bytes32, Instant>>> = Default::default();
        let pool_state: Arc<Mutex<HashMap<Bytes32, FarmerPoolState>>> = Default::default();
        let farmer_public_keys: Arc<Mutex<HashMap<Bytes48, SecretKey>>> = Default::default();
        let farmer_private_keys: Arc<Mutex<Vec<SecretKey>>> = Default::default();
        let pool_public_keys: Arc<Mutex<HashMap<Bytes48, SecretKey>>> = Default::default();
        let owner_secret_keys: Arc<Mutex<HashMap<Bytes48, SecretKey>>> = Default::default();
        let full_node_client: Arc<Mutex<Option<FarmerClient>>> = Default::default();
        let handles = Arc::new(Mutex::new(Self::handles(
            signage_points.clone(),
            quality_to_identifiers.clone(),
            proofs_of_space.clone(),
            cache_time.clone(),
            pool_state.clone(),
            farmer_private_keys.clone(),
            pool_public_keys.clone(),
            owner_secret_keys.clone(),
            full_node_client.clone(),
            pool_client.clone(),
            additional_headers.clone(),
            config.clone(),
        )));
        Ok(Self {
            server: WebsocketServer::new(&config.websocket, peers.clone(), handles)?,
            peers,
            signage_points,
            quality_to_identifiers,
            proofs_of_space,
            cache_time,
            pool_state,
            farmer_public_keys,
            farmer_private_keys,
            pool_public_keys,
            owner_secret_keys,
            full_node_client,
            pool_client,
            additional_headers,
            config
        })
    }

    fn handles(
        signage_points: Arc<Mutex<HashMap<Bytes32, Vec<NewSignagePoint>>>>,
        quality_to_identifiers: Arc<Mutex<HashMap<Bytes32, FarmerIdentifier>>>,
        proofs_of_space: ProofsMap,
        cache_time: Arc<Mutex<HashMap<Bytes32, Instant>>>,
        pool_state: Arc<Mutex<HashMap<Bytes32, FarmerPoolState>>>,
        farmer_private_keys: Arc<Mutex<Vec<SecretKey>>>,
        pool_public_keys: Arc<Mutex<HashMap<Bytes48, SecretKey>>>,
        owner_secret_keys: Arc<Mutex<HashMap<Bytes48, SecretKey>>>,
        full_node_client: Arc<Mutex<Option<FarmerClient>>>,
        pool_client: Arc<T>,
        additional_headers: Arc<HashMap<String, String>>,
        config: Arc<FarmerServerConfig>
    ) -> HashMap<Uuid, Arc<ChiaMessageHandler>> {
        HashMap::from([
            (Uuid::new_v4(), Arc::new(ChiaMessageHandler::new(
                Arc::new(ChiaMessageFilter {
                    msg_type: Some(ProtocolMessageTypes::Handshake),
                    id: None,
                }),
                Arc::new(HandshakeHandle {
                    config: config.clone(),
                    farmer_private_keys: Arc::new(Default::default()),
                    pool_public_keys: Arc::new(Default::default()),
                }),
            ))),
            (
                Uuid::new_v4(),
                Arc::new(ChiaMessageHandler::new(
                    Arc::new(ChiaMessageFilter {
                        msg_type: Some(ProtocolMessageTypes::NewProofOfSpace),
                        id: None,
                    }),
                    Arc::new(NewProofOfSpaceHandle {
                        pool_client: pool_client.clone(),
                        signage_points: signage_points.clone(),
                        quality_to_identifiers: quality_to_identifiers.clone(),
                        proofs_of_space: proofs_of_space.clone(),
                        cache_time: cache_time.clone(),
                        farmer_private_keys: farmer_private_keys.clone(),
                        owner_secret_keys: owner_secret_keys.clone(),
                        pool_state: pool_state.clone(),
                        config: config.clone(),
                        headers: additional_headers.clone(),
                    }),
                )),
            ),
            (
                Uuid::new_v4(),
                Arc::new(ChiaMessageHandler::new(
                    Arc::new(ChiaMessageFilter {
                        msg_type: Some(ProtocolMessageTypes::RespondSignatures),
                        id: None,
                    }),
                    Arc::new(RespondSignaturesHandle {
                        signage_points,
                        quality_to_identifiers,
                        proofs_of_space,
                        cache_time,
                        pool_public_keys,
                        farmer_private_keys,
                        owner_secret_keys,
                        pool_state,
                        full_node_client,
                        config,
                        headers: Arc::new(Default::default()),
                    }),
                )),
            ),
        ])
    }

    pub async fn run(&self, run: Arc<AtomicBool>) -> Result<(), Error> {
        self.server.run(run).await
    }
}

fn parse_payout_address(s: String) -> Result<String, Error> {
    Ok(if s.starts_with("xch") || s.starts_with("txch") {
        hex::encode(decode_puzzle_hash(&s)?)
    } else if s.len() == 64 {
        match hex_to_bytes(&s) {
            Ok(h) => hex::encode(h),
            Err(_) => s,
        }
    } else {
        s
    })
}

pub async fn get_farmer<T: PoolClient + Sized + Sync + Send>(
    pool_config: &PoolWalletConfig,
    authentication_token_timeout: u8,
    authentication_sk: &SecretKey,
    client: Arc<T>,
    additional_headers: Arc<HashMap<String, String>>,
) -> Result<GetFarmerResponse, PoolError> {
    let authentication_token = get_current_authentication_token(authentication_token_timeout);
    let msg = AuthenticationPayload {
        method_name: "get_farmer".to_string(),
        launcher_id: pool_config.launcher_id,
        target_puzzle_hash: pool_config.target_puzzle_hash,
        authentication_token,
    }.to_bytes();
    let to_sign = hash_256(&msg);
    let signature = sign(authentication_sk, &to_sign);
    if !verify_signature(&authentication_sk.sk_to_pk(), &to_sign, &signature) {
        error!("Farmer GET Failed to Validate Signature");
        return Err(PoolError {
            error_code: PoolErrorCode::InvalidSignature as u8,
            error_message: "Local Failed to Validate Signature".to_string(),
        });
    }
    client
        .get_farmer(
            &pool_config.pool_url,
            GetFarmerRequest {
                launcher_id: pool_config.launcher_id,
                authentication_token,
                signature: signature.to_bytes().into(),
            },
            &Some(additional_headers.as_ref().clone()),
        )
        .await
}

async fn do_auth(
    pool_config: &PoolWalletConfig,
    owner_sk: &SecretKey,
) -> Result<Bytes48, PoolError> {
    if owner_sk.sk_to_pk().to_bytes() != *pool_config.owner_public_key.to_sized_bytes() {
        return Err(PoolError {
            error_code: PoolErrorCode::ServerException as u8,
            error_message: "Owner Keys Mismatch".to_string(),
        });
    }
    Ok(owner_sk.sk_to_pk().to_bytes().into())
}

pub async fn post_farmer<T: PoolClient + Sized + Sync + Send>(
    pool_config: &PoolWalletConfig,
    payout_instructions: &str,
    authentication_token_timeout: u8,
    owner_sk: &SecretKey,
    suggested_difficulty: Option<u64>,
    client: Arc<T>,
    additional_headers: Arc<HashMap<String, String>>,
) -> Result<PostFarmerResponse, PoolError> {
    let payload = PostFarmerPayload {
        launcher_id: pool_config.launcher_id,
        authentication_token: get_current_authentication_token(authentication_token_timeout),
        authentication_public_key: do_auth(pool_config, owner_sk).await?,
        payout_instructions: parse_payout_address(payout_instructions.to_string()).map_err(
            |e| PoolError {
                error_code: PoolErrorCode::InvalidPayoutInstructions as u8,
                error_message: format!(
                    "Failed to Parse Payout Instructions: {}, {:?}",
                    payout_instructions, e
                ),
            },
        )?,
        suggested_difficulty,
    };
    let to_sign = hash_256(payload.to_bytes());
    let signature = sign(owner_sk, &to_sign);
    if !verify_signature(&owner_sk.sk_to_pk(), &to_sign, &signature) {
        error!("Farmer POST Failed to Validate Signature");
        return Err(PoolError {
            error_code: PoolErrorCode::InvalidSignature as u8,
            error_message: "Local Failed to Validate Signature".to_string(),
        });
    }
    client
        .post_farmer(
            &pool_config.pool_url,
            PostFarmerRequest {
                payload,
                signature: signature.to_bytes().into(),
            },
            &Some(additional_headers.as_ref().clone()),
        )
        .await
}

pub async fn put_farmer<T: PoolClient + Sized + Sync + Send>(
    pool_config: &PoolWalletConfig,
    payout_instructions: &str,
    authentication_token_timeout: u8,
    owner_sk: &SecretKey,
    suggested_difficulty: Option<u64>,
    client: Arc<T>,
    additional_headers: Arc<HashMap<String, String>>,
) -> Result<PutFarmerResponse, PoolError> {
    let authentication_public_key = do_auth(pool_config, owner_sk).await?;
    let payload = PutFarmerPayload {
        launcher_id: pool_config.launcher_id,
        authentication_token: get_current_authentication_token(authentication_token_timeout),
        authentication_public_key: Some(authentication_public_key),
        payout_instructions: parse_payout_address(payout_instructions.to_string()).ok(),
        suggested_difficulty,
    };
    let to_sign = hash_256(payload.to_bytes());
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
    client
        .put_farmer(
            &pool_config.pool_url,
            request,
            &Some(additional_headers.as_ref().clone()),
        )
        .await
}

pub async fn update_pool_farmer_info<T: PoolClient + Sized + Sync + Send>(
    pool_state: &mut FarmerPoolState,
    pool_config: &PoolWalletConfig,
    authentication_token_timeout: u8,
    authentication_sk: &SecretKey,
    client: Arc<T>,
    additional_headers: Arc<HashMap<String, String>>,
) -> Result<GetFarmerResponse, PoolError> {
    let response = get_farmer(
            pool_config,
            authentication_token_timeout,
            authentication_sk,
            client,
            additional_headers
        )
        .await?;
    pool_state.current_difficulty = Some(response.current_difficulty);
    pool_state.current_points = response.current_points;
    info!(
            "Updating Pool Difficulty: {:?} ",
            pool_state.current_difficulty
        );
    info!("Updating Current Points: {:?} ", pool_state.current_points);
    Ok(response)
}