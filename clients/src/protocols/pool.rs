use std::io::{Error, ErrorKind};
use blst::min_pk::{AggregateSignature, SecretKey, Signature};
use dg_xch_core::blockchain::proof_of_space::ProofOfSpace;
use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes48, Bytes96, SizedBytes};
use dg_xch_macros::ChiaSerial;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use dg_xch_core::clvm::bls_bindings::sign;
use dg_xch_serialize::{ChiaSerialize, hash_256};
use crate::api::pool::{DefaultPoolClient, PoolClient};

pub const POOL_PROTOCOL_VERSION: u8 = 1;
pub const SELF_POOLING: u8 = 1;
pub const LEAVING_POOL: u8 = 2;
pub const FARMING_TO_POOL: u8 = 3;

#[derive(ChiaSerial, Copy, Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum PoolSingletonState {
    SelfPooling = SELF_POOLING as isize,
    LeavingPool = LEAVING_POOL as isize,
    FarmingToPool = FARMING_TO_POOL as isize,
    Unknown = 0,
}
impl From<u8> for PoolSingletonState {
    fn from(byte: u8) -> Self {
        match byte {
            SELF_POOLING => PoolSingletonState::SelfPooling,
            LEAVING_POOL => PoolSingletonState::LeavingPool,
            FARMING_TO_POOL => PoolSingletonState::FarmingToPool,
            _ => PoolSingletonState::Unknown,
        }
    }
}

#[derive(ChiaSerial, Copy, Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum PoolErrorCode {
    RevertedSignagePoint = 1,
    TooLate = 2,
    NotFound = 3,
    InvalidProof = 4,
    ProofNotGoodEnough = 5,
    InvalidDifficulty = 6,
    InvalidSignature = 7,
    ServerException = 8,
    InvalidP2SingletonPuzzleHash = 9,
    FarmerNotKnown = 10,
    FarmerAlreadyKnown = 11,
    InvalidAuthenticationToken = 12,
    InvalidPayoutInstructions = 13,
    InvalidSingleton = 14,
    DelayTimeTooShort = 15,
    RequestFailed = 16,
}
impl From<u8> for PoolErrorCode {
    fn from(byte: u8) -> Self {
        match byte {
            1 => PoolErrorCode::RevertedSignagePoint,
            2 => PoolErrorCode::TooLate,
            3 => PoolErrorCode::NotFound,
            4 => PoolErrorCode::InvalidProof,
            5 => PoolErrorCode::ProofNotGoodEnough,
            6 => PoolErrorCode::InvalidDifficulty,
            7 => PoolErrorCode::InvalidSignature,
            8 => PoolErrorCode::ServerException,
            9 => PoolErrorCode::InvalidP2SingletonPuzzleHash,
            10 => PoolErrorCode::FarmerNotKnown,
            11 => PoolErrorCode::FarmerAlreadyKnown,
            12 => PoolErrorCode::InvalidAuthenticationToken,
            13 => PoolErrorCode::InvalidPayoutInstructions,
            14 => PoolErrorCode::InvalidSingleton,
            15 => PoolErrorCode::DelayTimeTooShort,
            16 => PoolErrorCode::RequestFailed,
            _ => PoolErrorCode::RequestFailed,
        }
    }
}
#[derive(ChiaSerial, Clone, Debug, Serialize, Deserialize)]
pub struct PoolError {
    pub error_code: u8,
    pub error_message: String,
}

#[derive(ChiaSerial, Clone, Debug, Serialize, Deserialize)]
pub struct AuthenticationPayload {
    pub method_name: String,
    pub launcher_id: Bytes32,
    pub target_puzzle_hash: Bytes32,
    pub authentication_token: u64,
}
#[derive(ChiaSerial, Clone, Debug, Serialize, Deserialize)]
pub struct GetPoolInfoResponse {
    pub name: String,
    pub logo_url: String,
    pub minimum_difficulty: u64,
    pub relative_lock_height: u32,
    pub protocol_version: u8,
    pub fee: String,
    pub description: String,
    pub target_puzzle_hash: Bytes32,
    pub authentication_token_timeout: u8,
}

#[derive(ChiaSerial, Clone, Debug, Serialize, Deserialize)]
pub struct PostPartialPayload {
    pub launcher_id: Bytes32,
    pub authentication_token: u64,
    pub proof_of_space: ProofOfSpace,
    pub sp_hash: Bytes32,
    pub end_of_sub_slot: bool,
    pub harvester_id: Bytes32,
}

#[derive(ChiaSerial, Clone, Debug, Serialize, Deserialize)]
pub struct PostPartialRequest {
    pub payload: PostPartialPayload,
    pub aggregate_signature: Bytes96,
}

#[derive(ChiaSerial, Clone, Debug, Serialize, Deserialize)]
pub struct PostPartialResponse {
    pub new_difficulty: u64,
}

#[derive(ChiaSerial, Clone, Debug, Serialize, Deserialize)]
pub struct GetFarmerRequest {
    pub launcher_id: Bytes32,
    pub authentication_token: u64,
    pub signature: Bytes96,
}

#[derive(ChiaSerial, Clone, Debug, Serialize, Deserialize)]
pub struct GetFarmerResponse {
    pub authentication_public_key: Bytes48,
    pub payout_instructions: String,
    pub current_difficulty: u64,
    pub current_points: u64,
}

#[derive(ChiaSerial, Clone, Debug, Serialize, Deserialize)]
pub struct PostFarmerPayload {
    pub launcher_id: Bytes32,
    pub authentication_token: u64,
    pub authentication_public_key: Bytes48,
    pub payout_instructions: String,
    pub suggested_difficulty: Option<u64>,
}

#[derive(ChiaSerial, Clone, Debug, Serialize, Deserialize)]
pub struct PostFarmerRequest {
    pub payload: PostFarmerPayload,
    pub signature: Bytes96,
}

#[derive(ChiaSerial, Clone, Debug, Serialize, Deserialize)]
pub struct PostFarmerResponse {
    pub welcome_message: String,
}

#[derive(ChiaSerial, Clone, Debug, Serialize, Deserialize)]
pub struct PutFarmerPayload {
    pub launcher_id: Bytes32,
    pub authentication_token: u64,
    pub authentication_public_key: Option<Bytes48>,
    pub payout_instructions: Option<String>,
    pub suggested_difficulty: Option<u64>,
}

#[derive(ChiaSerial, Clone, Debug, Serialize, Deserialize)]
pub struct PutFarmerRequest {
    pub payload: PutFarmerPayload,
    pub signature: Bytes96,
}

#[derive(ChiaSerial, Clone, Debug, Serialize, Deserialize)]
pub struct PutFarmerResponse {
    pub authentication_public_key: Option<bool>,
    pub payout_instructions: Option<bool>,
    pub suggested_difficulty: Option<bool>,
}

#[derive(ChiaSerial, Clone, Debug, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub error_code: u16,
    pub error_message: Option<String>,
}

pub fn get_current_authentication_token(timeout: u8) -> u64 {
    let now: u64 = OffsetDateTime::now_utc().unix_timestamp() as u64;
    now / 60 / timeout as u64
}

pub fn validate_authentication_token(token: u64, timeout: u8) -> bool {
    let cur_token = get_current_authentication_token(timeout);
    let dif = if token > cur_token {
        token - cur_token
    } else {
        cur_token - token
    };
    dif <= timeout as u64
}

#[derive(serde::Serialize, serde::Deserialize, Debug)]
pub struct PoolLoginParts {
    pub auth_token: u64,
    pub aggregate_signature: String,
}

pub async fn create_pool_login_url(target_pool: &str, keys_and_launcher_ids: &[(SecretKey, Bytes32)] ) -> Result<String, Error> {
    let parts = create_pool_login_parts(target_pool, keys_and_launcher_ids).await?;
    let mut ids = String::new();
    for (index, (_, launcher_id)) in keys_and_launcher_ids.iter().enumerate() {
        if index != 0 {
            ids.push(',')
        }
        ids.extend(hex::encode(launcher_id.as_slice()).chars());
    }
    Ok(format!("{target_pool}/login?launcher_id={ids}&authentication_token={}&signature={})", parts.auth_token, parts.aggregate_signature))
}

pub async fn create_pool_login_parts(target_pool: &str, keys_and_launcher_ids: &[(SecretKey, Bytes32)]) -> Result<PoolLoginParts, Error> {
    let pool_client = DefaultPoolClient::new();
    let pool_info = pool_client
        .get_pool_info(target_pool)
        .await
        .map_err(|e| Error::new(ErrorKind::Other, format!("{:?}", e)))?;
    let current_auth_token =
        get_current_authentication_token(pool_info.authentication_token_timeout);
    let mut sigs = vec![];
    for (sec_key, launcher_id) in keys_and_launcher_ids {
        let payload = AuthenticationPayload {
            method_name: String::from("get_login"),
            launcher_id: *launcher_id,
            target_puzzle_hash: pool_info.target_puzzle_hash,
            authentication_token: current_auth_token,
        };
        let to_sign = hash_256(payload.to_bytes());
        let sig = sign(&sec_key, &to_sign);
        sigs.push(sig);
    }
    if !sigs.is_empty() {
        let aggregate_signature =
            AggregateSignature::aggregate(sigs.iter().collect::<Vec<&Signature>>().as_ref(), true)
                .map_err(|e| {
                    Error::from(std::io::Error::new(
                        ErrorKind::InvalidInput,
                        format!("Failed to calculate signature: {:?}", e),
                    ))
                })?;
        Ok(PoolLoginParts {
            auth_token: current_auth_token,
            aggregate_signature: hex::encode(aggregate_signature.to_signature().to_bytes()),
        })
    } else {
        Err(Error::new(
            ErrorKind::NotFound,
            "No Launcher IDs with Keys found",
        ))
    }
}
