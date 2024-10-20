use crate::blockchain::proof_of_space::ProofOfSpace;
use crate::blockchain::sized_bytes::{Bytes32, Bytes48, Bytes96};

use dg_xch_macros::ChiaSerial;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

pub const POOL_PROTOCOL_VERSION: u8 = 1;
pub const SELF_POOLING: u8 = 1;
pub const LEAVING_POOL: u8 = 2;
pub const FARMING_TO_POOL: u8 = 3;

#[derive(ChiaSerial, Copy, PartialEq, Eq, Clone, Debug, Serialize, Deserialize)]
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

#[derive(ChiaSerial, Copy, PartialEq, Eq, Clone, Debug, Serialize, Deserialize)]
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
            _ => PoolErrorCode::RequestFailed,
        }
    }
}
#[derive(ChiaSerial, PartialEq, Eq, Clone, Debug, Serialize, Deserialize)]
pub struct PoolError {
    pub error_code: u8,        //Min Version 0.0.34
    pub error_message: String, //Min Version 0.0.34
}

#[derive(ChiaSerial, PartialEq, Eq, Clone, Debug, Serialize, Deserialize)]
pub struct AuthenticationPayload {
    pub method_name: String,         //Min Version 0.0.34
    pub launcher_id: Bytes32,        //Min Version 0.0.34
    pub target_puzzle_hash: Bytes32, //Min Version 0.0.34
    pub authentication_token: u64,   //Min Version 0.0.34
}
#[derive(ChiaSerial, PartialEq, Eq, Clone, Debug, Serialize, Deserialize)]
pub struct GetPoolInfoResponse {
    pub name: String,                     //Min Version 0.0.34
    pub logo_url: String,                 //Min Version 0.0.34
    pub minimum_difficulty: u64,          //Min Version 0.0.34
    pub relative_lock_height: u32,        //Min Version 0.0.34
    pub protocol_version: u8,             //Min Version 0.0.34
    pub fee: String,                      //Min Version 0.0.34
    pub description: String,              //Min Version 0.0.34
    pub target_puzzle_hash: Bytes32,      //Min Version 0.0.34
    pub authentication_token_timeout: u8, //Min Version 0.0.34
}

#[derive(ChiaSerial, PartialEq, Eq, Clone, Debug, Serialize, Deserialize)]
pub struct PostPartialPayload {
    pub launcher_id: Bytes32,         //Min Version 0.0.34
    pub authentication_token: u64,    //Min Version 0.0.34
    pub proof_of_space: ProofOfSpace, //Min Version 0.0.34
    pub sp_hash: Bytes32,             //Min Version 0.0.34
    pub end_of_sub_slot: bool,        //Min Version 0.0.34
    pub harvester_id: Bytes32,        //Min Version 0.0.34
}

#[derive(ChiaSerial, PartialEq, Eq, Clone, Debug, Serialize, Deserialize)]
pub struct PostPartialRequest {
    pub payload: PostPartialPayload,  //Min Version 0.0.34
    pub aggregate_signature: Bytes96, //Min Version 0.0.34
}

#[derive(ChiaSerial, PartialEq, Eq, Clone, Debug, Serialize, Deserialize)]
pub struct PostPartialResponse {
    pub new_difficulty: u64, //Min Version 0.0.34
}

#[derive(ChiaSerial, PartialEq, Eq, Clone, Debug, Serialize, Deserialize)]
pub struct GetFarmerRequest {
    pub launcher_id: Bytes32,      //Min Version 0.0.34
    pub authentication_token: u64, //Min Version 0.0.34
    pub signature: Bytes96,        //Min Version 0.0.34
}

#[derive(ChiaSerial, PartialEq, Eq, Clone, Debug, Serialize, Deserialize)]
pub struct GetFarmerResponse {
    pub authentication_public_key: Bytes48, //Min Version 0.0.34
    pub payout_instructions: String,        //Min Version 0.0.34
    pub current_difficulty: u64,            //Min Version 0.0.34
    pub current_points: u64,                //Min Version 0.0.34
}

#[derive(ChiaSerial, PartialEq, Eq, Clone, Debug, Serialize, Deserialize)]
pub struct PostFarmerPayload {
    pub launcher_id: Bytes32,               //Min Version 0.0.34
    pub authentication_token: u64,          //Min Version 0.0.34
    pub authentication_public_key: Bytes48, //Min Version 0.0.34
    pub payout_instructions: String,        //Min Version 0.0.34
    pub suggested_difficulty: Option<u64>,  //Min Version 0.0.34
}

#[derive(ChiaSerial, PartialEq, Eq, Clone, Debug, Serialize, Deserialize)]
pub struct PostFarmerRequest {
    pub payload: PostFarmerPayload, //Min Version 0.0.34
    pub signature: Bytes96,         //Min Version 0.0.34
}

#[derive(ChiaSerial, PartialEq, Eq, Clone, Debug, Serialize, Deserialize)]
pub struct PostFarmerResponse {
    pub welcome_message: String, //Min Version 0.0.34
}

#[derive(ChiaSerial, PartialEq, Eq, Clone, Debug, Serialize, Deserialize)]
pub struct PutFarmerPayload {
    pub launcher_id: Bytes32,                       //Min Version 0.0.34
    pub authentication_token: u64,                  //Min Version 0.0.34
    pub authentication_public_key: Option<Bytes48>, //Min Version 0.0.34
    pub payout_instructions: Option<String>,        //Min Version 0.0.34
    pub suggested_difficulty: Option<u64>,          //Min Version 0.0.34
}

#[derive(ChiaSerial, PartialEq, Eq, Clone, Debug, Serialize, Deserialize)]
pub struct PutFarmerRequest {
    pub payload: PutFarmerPayload, //Min Version 0.0.34
    pub signature: Bytes96,        //Min Version 0.0.34
}

#[derive(ChiaSerial, PartialEq, Eq, Clone, Debug, Serialize, Deserialize)]
pub struct PutFarmerResponse {
    pub authentication_public_key: Option<bool>, //Min Version 0.0.34
    pub payout_instructions: Option<bool>,       //Min Version 0.0.34
    pub suggested_difficulty: Option<bool>,      //Min Version 0.0.34
}

#[derive(ChiaSerial, PartialEq, Eq, Clone, Debug, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub error_code: u16,               //Min Version 0.0.34
    pub error_message: Option<String>, //Min Version 0.0.34
}

#[allow(clippy::cast_sign_loss)]
#[must_use]
pub fn get_current_authentication_token(timeout: u8) -> u64 {
    let now: u64 = OffsetDateTime::now_utc().unix_timestamp() as u64;
    now / 60 / u64::from(timeout)
}

#[must_use]
pub fn validate_authentication_token(token: u64, timeout: u8) -> bool {
    let cur_token = get_current_authentication_token(timeout);
    let dif = if token > cur_token {
        token - cur_token
    } else {
        cur_token - token
    };
    dif <= u64::from(timeout)
}
