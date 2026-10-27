//! Shared pool browser statements and sessions; signature verification is profile-specific.
use crate::blockchain::sized_bytes::{Bytes32, Bytes48, Bytes96};
use serde::{Deserialize, Serialize};
use std::io::Error;

/// Browser authentication is signed locally by the PlotNFT owner. A linked
/// identity must independently authorize the same complete statement.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserAction {
    Login,
    Link,
    Unlink,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserProof {
    pub launcher_id: Bytes32,
    pub linked_launcher_id: Option<Bytes32>,
    pub action: BrowserAction,
    pub nonce: String,
    pub timestamp: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BrowserTicket {
    pub ticket: String,
    pub expires: u64,
    pub launcher_id: Bytes32,
    pub linked_farmers: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlsAuthorization {
    pub public_key: Bytes48,
    pub signature: Bytes96,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlsBrowserAuthorization {
    pub proof: BrowserProof,
    pub owner: BlsAuthorization,
    pub linked_authorization: Option<BlsAuthorization>,
}

pub fn timestamp(timestamp: u64, now: u64) -> Result<(), Error> {
    if timestamp.abs_diff(now) > 60 {
        return Err(Error::other(
            "pool browser proof timestamp is stale or in the future",
        ));
    }
    Ok(())
}

pub fn bls_message(
    genesis: Bytes32,
    target: Bytes32,
    operation: &str,
    proof: &BrowserProof,
) -> Result<Vec<u8>, Error> {
    if !matches!(operation, "web_login" | "web_link") || proof.nonce.len() != 64 {
        return Err(Error::other("invalid pool browser statement"));
    }
    let mut message = b"dg_xch/bls/pool/web/v1\0".to_vec();
    message.extend_from_slice(genesis.as_ref());
    message.extend_from_slice(target.as_ref());
    message.extend_from_slice(operation.as_bytes());
    message.push(0);
    message.extend_from_slice(&serde_json::to_vec(proof).map_err(Error::other)?);
    Ok(crate::utils::hash_256(message).to_vec())
}
