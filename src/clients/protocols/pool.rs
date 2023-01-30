use crate::types::blockchain::proof_of_space::ProofOfSpace;
use crate::types::blockchain::sized_bytes::{Bytes32, Bytes48, Bytes96};
use crate::types::ChiaSerialize;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::io::Error;

pub const POOL_PROTOCOL_VERSION: u8 = 1;

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
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

#[derive(Debug, Serialize, Deserialize)]
pub struct PoolError {
    pub error_code: u8,
    pub error_message: String,
}

pub struct AuthenticationPayload {
    pub method_name: String,
    pub launcher_id: Bytes32,
    pub target_puzzle_hash: Bytes32,
    pub authentication_token: u64,
}
impl ChiaSerialize for AuthenticationPayload {
    fn to_bytes(&self) -> Vec<u8>
    where
        Self: Sized,
    {
        let mut buf = vec![];
        buf.extend((self.method_name.len() as u32).to_be_bytes());
        buf.extend(self.method_name.as_bytes());
        buf.extend(self.launcher_id.to_sized_bytes());
        buf.extend(self.target_puzzle_hash.to_sized_bytes());
        buf.extend(self.authentication_token.to_be_bytes());
        buf
    }

    fn from_bytes(_bytes: &[u8]) -> Result<Self, Error>
    where
        Self: Sized,
    {
        todo!()
    }
}
#[derive(Debug, Serialize, Deserialize)]
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

#[derive(Debug, Serialize, Deserialize)]
pub struct PostPartialPayload {
    pub launcher_id: Bytes32,
    pub authentication_token: u64,
    pub proof_of_space: ProofOfSpace,
    pub sp_hash: Bytes32,
    pub end_of_sub_slot: bool,
    pub harvester_id: Bytes32,
}
impl ChiaSerialize for PostPartialPayload {
    fn to_bytes(&self) -> Vec<u8>
    where
        Self: Sized,
    {
        let mut buf = vec![];
        buf.extend(self.launcher_id.to_sized_bytes());
        buf.extend(self.authentication_token.to_be_bytes());
        buf.extend(self.proof_of_space.to_bytes());
        buf.extend(self.sp_hash.to_sized_bytes());
        buf.push(self.end_of_sub_slot as u8);
        buf.extend(self.harvester_id.to_sized_bytes());
        buf
    }

    fn from_bytes(_bytes: &[u8]) -> Result<Self, Error>
    where
        Self: Sized,
    {
        todo!()
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PostPartialRequest {
    pub payload: PostPartialPayload,
    pub aggregate_signature: Bytes96,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PostPartialResponse {
    pub new_difficulty: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GetFarmerRequest {
    pub launcher_id: Bytes32,
    pub authentication_token: u64,
    pub signature: Bytes96,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GetFarmerResponse {
    pub authentication_public_key: Bytes48,
    pub payout_instructions: String,
    pub current_difficulty: u64,
    pub current_points: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PostFarmerPayload {
    pub launcher_id: Bytes32,
    pub authentication_token: u64,
    pub authentication_public_key: Bytes48,
    pub payout_instructions: String,
    pub suggested_difficulty: Option<u64>,
}
impl ChiaSerialize for PostFarmerPayload {
    fn to_bytes(&self) -> Vec<u8>
    where
        Self: Sized,
    {
        let mut buf = vec![];
        buf.extend(self.launcher_id.to_sized_bytes());
        buf.extend(self.authentication_token.to_be_bytes());
        buf.extend(self.authentication_public_key.to_sized_bytes());
        buf.extend((self.payout_instructions.len() as u32).to_be_bytes());
        buf.extend(self.payout_instructions.as_bytes());
        if let Some(d) = self.suggested_difficulty {
            buf.push(1);
            buf.extend(d.to_be_bytes());
        } else {
            buf.push(0);
        }
        buf
    }

    fn from_bytes(_bytes: &[u8]) -> Result<Self, Error>
    where
        Self: Sized,
    {
        todo!()
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PostFarmerRequest {
    pub payload: PostFarmerPayload,
    pub signature: Bytes96,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PostFarmerResponse {
    pub welcome_message: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PutFarmerPayload {
    pub launcher_id: Bytes32,
    pub authentication_token: u64,
    pub authentication_public_key: Option<Bytes48>,
    pub payout_instructions: Option<String>,
    pub suggested_difficulty: Option<u64>,
}
impl ChiaSerialize for PutFarmerPayload {
    fn to_bytes(&self) -> Vec<u8>
    where
        Self: Sized,
    {
        let mut buf = vec![];
        buf.extend(self.launcher_id.to_sized_bytes());
        buf.extend(self.authentication_token.to_be_bytes());
        if let Some(d) = &self.authentication_public_key {
            buf.push(1);
            buf.extend(d.to_sized_bytes());
        } else {
            buf.push(0);
        }
        if let Some(d) = &self.payout_instructions {
            buf.push(1);
            buf.extend((d.len() as u32).to_be_bytes());
            buf.extend(d.as_bytes());
        } else {
            buf.push(0);
        }
        if let Some(d) = &self.suggested_difficulty {
            buf.push(1);
            buf.extend(d.to_be_bytes());
        } else {
            buf.push(0);
        }
        buf
    }

    fn from_bytes(_bytes: &[u8]) -> Result<Self, Error>
    where
        Self: Sized,
    {
        todo!()
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PutFarmerRequest {
    pub payload: PutFarmerPayload,
    pub signature: Bytes96,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PutFarmerResponse {
    pub authentication_public_key: Option<bool>,
    pub payout_instructions: Option<bool>,
    pub suggested_difficulty: Option<bool>,
}

pub struct ErrorResponse {
    pub error_code: u16,
    pub error_message: Option<String>,
}

pub fn get_current_authentication_token(timeout: u8) -> u64 {
    let now: u64 = Utc::now().timestamp() as u64;
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
