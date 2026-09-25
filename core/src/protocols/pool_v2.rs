use crate::blockchain::sized_bytes::{Bytes32, Bytes96};
use crate::protocols::pool::{GetPoolInfoResponse, PostPartialPayload, PutFarmerPayload};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GetAuthRequest {
    pub launcher_id: Bytes32,
    pub timestamp: u64,
    pub signature: Bytes96,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GetAuthResponse {
    pub authentication_token: String,
    pub expiration: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PoolInfo {
    #[serde(flatten)]
    pub info: GetPoolInfoResponse,
    pub pool_memoization: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GetFarmerRequest {
    pub launcher_id: Bytes32,
    pub authentication_token: u64,
    pub authentication_token_v2: String,
    pub signature: Option<Bytes96>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PostPartialRequest {
    pub payload: PostPartialPayload,
    pub authentication_token_v2: String,
    pub aggregate_signature: Bytes96,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UpdateFarmerPayload {
    #[serde(flatten)]
    pub farmer: PutFarmerPayload,
    pub authentication_token_v2: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PutFarmerRequest {
    pub payload: UpdateFarmerPayload,
    pub signature: Option<Bytes96>,
}
