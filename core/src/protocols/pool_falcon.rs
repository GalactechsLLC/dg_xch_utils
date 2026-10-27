//! Falcon-only pool authentication. Requests bind the chain, pool, operation and
//! complete payload; plot and owner signatures use separate operation domains.
use crate::blockchain::sized_bytes::Bytes32;
use crate::signatures::falcon::FalconPublicKey;
use crate::signatures::falcon_block::{FalconHeaderAuthorization, public_key_id};
use serde::{Deserialize, Serialize};
use std::io::Error;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicKey(#[serde(with = "crate::signatures::extension::array")] pub FalconPublicKey);

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Registration {
    pub launcher_id: Bytes32,
    pub payout_puzzle_hash: Bytes32,
    pub timestamp: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FarmerUpdate {
    pub launcher_id: Bytes32,
    pub payout_puzzle_hash: Bytes32,
    pub timestamp: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggested_difficulty: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FarmerQuery {
    pub launcher_id: Bytes32,
    pub timestamp: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Partial {
    pub partial: super::pool::PostPartialPayload,
    pub timestamp: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Authenticated<T> {
    pub payload: T,
    pub authorization: FalconHeaderAuthorization,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PartialRequest {
    pub owner: Authenticated<Partial>,
    pub plot_authorization: FalconHeaderAuthorization,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FarmerInfo {
    pub launcher_id: Bytes32,
    pub contract_puzzle_hash: Bytes32,
    pub payout_puzzle_hash: Bytes32,
    pub current_difficulty: u64,
    pub current_points: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PoolConfig {
    pub pool_url: String,
    pub launcher_id: Bytes32,
    pub contract_puzzle_hash: Bytes32,
    pub target_puzzle_hash: Bytes32,
}

pub fn message<T: Serialize>(
    genesis: Bytes32,
    target: Bytes32,
    operation: &str,
    payload: &T,
) -> Result<Vec<u8>, Error> {
    let encoded = serde_json::to_vec(payload).map_err(Error::other)?;
    if encoded.len() > 96 * 1024 || operation.len() > 64 {
        return Err(Error::other(
            "Falcon pool authentication payload exceeds limit",
        ));
    }
    let mut bytes = b"dg_xch/falcon/pool/v1\0".to_vec();
    bytes.extend_from_slice(genesis.as_ref());
    bytes.extend_from_slice(target.as_ref());
    bytes.extend_from_slice(&(operation.len() as u64).to_be_bytes());
    bytes.extend_from_slice(operation.as_bytes());
    bytes.extend_from_slice(&encoded);
    Ok(crate::utils::hash_256(bytes).to_vec())
}

pub fn verify<T: Serialize>(
    authorization: &FalconHeaderAuthorization,
    key: &PublicKey,
    genesis: Bytes32,
    target: Bytes32,
    operation: &str,
    payload: &T,
) -> Result<(), Error> {
    let message = message(genesis, target, operation, payload)?;
    if !authorization.verify(
        genesis,
        &public_key_id(&key.0),
        &message,
        &authorization.reference(genesis),
    ) {
        return Err(Error::other("invalid Falcon pool signature"));
    }
    Ok(())
}

pub fn timestamp(timestamp: u64, now: u64) -> Result<(), Error> {
    if timestamp.abs_diff(now) > 60 {
        return Err(Error::other(
            "Falcon pool request timestamp is stale or in the future",
        ));
    }
    Ok(())
}

#[cfg(feature = "falcon")]
pub fn sign<T: Serialize>(
    signer: &crate::signatures::falcon::FalconSigner,
    genesis: Bytes32,
    target: Bytes32,
    operation: &str,
    payload: T,
) -> Result<Authenticated<T>, Error> {
    let authorization = FalconHeaderAuthorization::sign(
        signer,
        genesis,
        &message(genesis, target, operation, &payload)?,
    )?;
    Ok(Authenticated {
        payload,
        authorization,
    })
}

pub use super::pool_web::{BrowserAction, BrowserProof, BrowserTicket};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserAuthorization {
    pub owner: Authenticated<BrowserProof>,
    pub linked_authorization: Option<FalconHeaderAuthorization>,
}
