//! Authenticated Chia RPC routes.

use crate::rpc::{
    MAX_BLOCK_RECORDS_PER_REQUEST, MAX_BLOCKS_PER_REQUEST, MAX_RPC_BODY_BYTES, Node, RpcError,
    apply_coin_query_window, check_id_cap, eos_not_in_cache, generator_input_for_block,
    mempool_item_json, network_space_between, puzzle_and_solution_coin_spend, sp_not_in_cache,
};
use async_trait::async_trait;
use dg_xch_core::blockchain::block_record::BlockRecord;
use dg_xch_core::blockchain::blockchain_state::{BlockchainState, MinMempoolFees};
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::blockchain::sync::Sync as SyncStatus;
use dg_xch_core::blockchain::tx_status::TXStatus;
use dg_xch_core::blockchain::unfinished_header_block::UnfinishedHeaderBlock;
use dg_xch_core::blockchain::unsized_bytes::UnsizedBytes;
use dg_xch_core::consensus::block_generator::RawCondition;
use dg_xch_core::consensus::block_generator::{
    coin_spends_from_generator, coin_spends_with_conditions_from_generator,
    conditions_from_spend_bundle,
};
use dg_xch_core::consensus::constants::MAINNET;
#[cfg(feature = "coin-index")]
use dg_xch_core::protocols::full_node::{
    AdditionsAndRemovals, AdditionsAndRemovalsRequest, CoinRecordsByParentIdsRequest,
    CoinRecordsByPuzzleHashRequest, CoinRecordsByPuzzleHashesRequest,
};
use dg_xch_core::protocols::full_node::{
    BlockRecordByHeightRequest, BlockRecordsRequest, BlockRequest, BlocksRequest, CoinQueryWindow,
    CoinRecordByNameRequest, CoinRecordByNamesRequest, ConnectionsRequest, FeeEstimateRequest,
    MempoolItemByCoinNameRequest, MempoolItemByTxIdRequest, NetworkSpaceRequest, PushTxRequest,
    PuzzleAndSolutionRequest, RecentSignagePointorEOSRequest,
};
#[cfg(feature = "hint")]
use dg_xch_core::protocols::full_node::{CoinRecordsByHintRequest, CoinRecordsByHintsRequest};
use dg_xch_core::protocols::simulator::{AutoFarmRequest, FarmBlockRequest};
use http::StatusCode;
use portfu::prelude::{ConnectionInfo, PortfuError, Request, Response, State};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::future::Future;
use std::ops::BitOr;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

const RPC_BODY_READ_TIMEOUT: Duration = Duration::from_secs(5);
const RPC_TRUST_STORE: &str = "chia-rpc";

/// Access methods accepted by an RPC route hosted on the shared Portfu listener.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RpcAccessPolicy(u8);

impl RpcAccessPolicy {
    #[allow(non_upper_case_globals)]
    pub const Loopback: Self = Self(1 << 0);
    #[allow(non_upper_case_globals)]
    pub const PrivateCa: Self = Self(1 << 1);

    const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

impl BitOr for RpcAccessPolicy {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

mod farm_block;
#[cfg(feature = "coin-index")]
mod get_additions_and_removals;
mod get_aggsig_additional_data;
mod get_all_mempool_items;
mod get_all_mempool_tx_ids;
mod get_auto_farming;
mod get_block;
mod get_block_record;
mod get_block_record_by_height;
mod get_block_records;
mod get_block_spends;
mod get_block_spends_with_conditions;
mod get_blockchain_state;
mod get_blocks;
mod get_coin_record_by_name;
#[cfg(feature = "hint")]
mod get_coin_records_by_hint;
#[cfg(feature = "hint")]
mod get_coin_records_by_hints;
mod get_coin_records_by_names;
#[cfg(feature = "coin-index")]
mod get_coin_records_by_parent_ids;
#[cfg(feature = "coin-index")]
mod get_coin_records_by_puzzle_hash;
#[cfg(feature = "coin-index")]
mod get_coin_records_by_puzzle_hashes;
mod get_connections;
mod get_fee_estimate;
mod get_mempool_item_by_tx_id;
mod get_mempool_items_by_coin_name;
mod get_network_info;
mod get_network_space;
mod get_puzzle_and_solution;
mod get_recent_signage_point_or_eos;
mod get_routes;
mod get_unfinished_block_headers;
mod get_version;
mod healthz;
mod push_tx;
mod set_auto_farming;

/// The block-production control a simulator attaches to the RPC, adding the `farm_block`,
/// `set_auto_farming` and `get_auto_farming` endpoints. A production node attaches none, and those
/// endpoints 404.
#[async_trait]
pub trait SimControl: Send + Sync {
    /// Farm `blocks` blocks whose rewards pay `address` (a bech32 puzzle-hash address), sealing any
    /// pending mempool transactions; `guarantee_tx_block` forces each to be a transaction block.
    async fn farm_block(
        &self,
        address: &str,
        blocks: u32,
        guarantee_tx_block: bool,
    ) -> Result<(), String>;
    /// Set auto-farming (a block is sealed whenever a wallet submits a transaction); returns the new
    /// state.
    fn set_auto_farming(&self, should_auto_farm: bool) -> bool;
    /// Whether auto-farming is on.
    fn auto_farming(&self) -> bool;
}

pub use dg_xch_core::protocols::full_node::FeeEstimate as FeeEstimateResponse;

fn json_response(status: StatusCode, payload: Value) -> Response {
    Response::from_status_and_message(status, payload.to_string()).content_type("application/json")
}

fn access_denied(connection: &ConnectionInfo, policy: RpcAccessPolicy) -> Option<Response> {
    if policy.contains(RpcAccessPolicy::Loopback) && connection.peer_addr.ip().is_loopback() {
        return None;
    }
    if policy.contains(RpcAccessPolicy::PrivateCa) {
        return match connection.client_identity.as_ref() {
            None => Some(json_response(
                StatusCode::UNAUTHORIZED,
                serde_json::json!({"success": false, "error": "RPC client certificate required"}),
            )),
            Some(identity)
                if !identity
                    .verified_by
                    .iter()
                    .any(|name| name == RPC_TRUST_STORE) =>
            {
                Some(json_response(
                    StatusCode::FORBIDDEN,
                    serde_json::json!({"success": false, "error": "RPC client certificate is not trusted"}),
                ))
            }
            Some(_) => None,
        };
    }
    Some(json_response(
        StatusCode::FORBIDDEN,
        serde_json::json!({"success": false, "error": "local RPC requires a loopback client"}),
    ))
}

macro_rules! check_access_policy {
    ($connection:expr, $policy:expr) => {
        if let Some(response) = access_denied(&$connection, $policy) {
            return Ok(response);
        }
    };
}
pub(super) use check_access_policy;

/// Apply shared RPC access controls, body limits, and Chia response envelopes.
pub async fn serve_portfu<F, Fut>(
    node: State<Node>,
    request: &mut Request,
    handler: F,
) -> Result<Response, PortfuError>
where
    F: FnOnce(Arc<Node>, Vec<u8>) -> Fut,
    Fut: Future<Output = Result<Option<Map<String, Value>>, RpcError>>,
{
    let body = match request
        .consume_body_bytes_limited(MAX_RPC_BODY_BYTES, RPC_BODY_READ_TIMEOUT)
        .await
    {
        Ok(body) => body,
        Err(PortfuError::PayloadTooLarge(_)) => {
            return Ok(json_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                serde_json::json!({
                    "success": false,
                    "error": format!("request body exceeds {MAX_RPC_BODY_BYTES} bytes"),
                }),
            ));
        }
        Err(error) => return Err(error),
    };

    let (status, payload) = match handler(node.0.clone(), body.to_vec()).await {
        Ok(Some(mut map)) => {
            map.entry("success".to_string())
                .or_insert(Value::from(true));
            (StatusCode::OK, Value::Object(map))
        }
        Ok(None) => (StatusCode::NOT_FOUND, serde_json::json!({"success": false})),
        Err(error) => (
            StatusCode::OK,
            serde_json::json!({
                "success": false,
                "error": error.to_string(),
                "traceback": Value::Null,
                "structuredError": Value::Null,
            }),
        ),
    };
    Ok(json_response(status, payload))
}

#[must_use]
pub fn route_names() -> Vec<&'static str> {
    vec![
        get_blockchain_state::PATH,
        get_block::PATH,
        get_blocks::PATH,
        get_block_record::PATH,
        get_block_record_by_height::PATH,
        get_block_records::PATH,
        get_block_spends::PATH,
        get_block_spends_with_conditions::PATH,
        get_unfinished_block_headers::PATH,
        get_network_space::PATH,
        get_recent_signage_point_or_eos::PATH,
        get_coin_records_by_names::PATH,
        get_coin_record_by_name::PATH,
        #[cfg(feature = "coin-index")]
        get_coin_records_by_parent_ids::PATH,
        #[cfg(feature = "coin-index")]
        get_coin_records_by_puzzle_hash::PATH,
        #[cfg(feature = "coin-index")]
        get_coin_records_by_puzzle_hashes::PATH,
        push_tx::PATH,
        #[cfg(feature = "hint")]
        get_coin_records_by_hint::PATH,
        #[cfg(feature = "hint")]
        get_coin_records_by_hints::PATH,
        #[cfg(feature = "coin-index")]
        get_additions_and_removals::PATH,
        get_puzzle_and_solution::PATH,
        get_all_mempool_tx_ids::PATH,
        get_all_mempool_items::PATH,
        get_mempool_item_by_tx_id::PATH,
        get_mempool_items_by_coin_name::PATH,
        get_fee_estimate::PATH,
        get_aggsig_additional_data::PATH,
        get_network_info::PATH,
        get_connections::PATH,
        get_routes::PATH,
        get_version::PATH,
        healthz::PATH,
    ]
}

// Clamp each element to the running minimum so the sequence never increases (the fee estimator
// can quote a HIGHER rate for a LONGER wait — an artifact users do not expect).
pub(crate) fn make_monotonically_decreasing(seq: &[f64]) -> Vec<f64> {
    let mut out = Vec::with_capacity(seq.len());
    let mut min = f64::INFINITY;
    for (i, &n) in seq.iter().enumerate() {
        if i == 0 || n <= min {
            out.push(n);
            min = n;
        } else {
            out.push(min);
        }
    }
    out
}

pub(super) fn parse<T: for<'de> Deserialize<'de>>(body: &[u8]) -> Result<T, RpcError> {
    serde_json::from_slice(body).map_err(|e| RpcError::BadRequest(e.to_string()))
}

// For endpoints whose parameters are all optional: an empty body reads as `{}`.
pub(super) fn parse_or_default<T: for<'de> Deserialize<'de> + Default>(
    body: &[u8],
) -> Result<T, RpcError> {
    if body.is_empty() {
        return Ok(T::default());
    }
    parse(body)
}

pub(crate) fn to_value<T: Serialize>(value: &T) -> Result<Value, RpcError> {
    serde_json::to_value(value).map_err(|e| RpcError::Corrupt(e.to_string()))
}

pub(super) fn envelope<T: Serialize>(key: &str, value: &T) -> Result<Map<String, Value>, RpcError> {
    Ok(obj_with(key, to_value(value)?))
}

pub(super) fn obj_with(key: &str, value: Value) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert(key.to_string(), value);
    m
}

// A u128 as JSON: an exact integer while it fits u64, else an f64 (serde_json cannot carry
// arbitrary-precision integers). Mainnet netspace exceeds u64 —
// the f64 form keeps 53 bits of mantissa, far inside the estimate's own error bar.
#[allow(clippy::cast_precision_loss)]
pub(super) fn json_u128(value: u128) -> Value {
    u64::try_from(value).map_or_else(|_| Value::from(value as f64), Value::from)
}

// PLAIN hex (no 0x) — the wire convention for injected header hashes, mempool map keys, and
// `additional_data`.
pub(crate) fn plain_hex(bytes: &Bytes32) -> String {
    let s = bytes.to_string();
    s.strip_prefix("0x").map_or(s.clone(), ToString::to_string)
}

// The condition JSON shape:
// opcode as 0x-prefixed hex, vars as plain hex.
pub(super) fn condition_json(cond: &RawCondition) -> Value {
    let mut m = Map::new();
    m.insert(
        "opcode".to_string(),
        Value::from(format!("0x{}", hex_of(&cond.opcode))),
    );
    m.insert(
        "vars".to_string(),
        Value::from(
            cond.vars
                .iter()
                .map(|v| Value::from(hex_of(v)))
                .collect::<Vec<_>>(),
        ),
    );
    Value::Object(m)
}

fn hex_of(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}
