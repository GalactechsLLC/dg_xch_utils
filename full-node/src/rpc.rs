use crate::config::RpcTlsMode;
use dg_xch_core::blockchain::block_record::BlockRecord;
use dg_xch_core::blockchain::coin_record::CoinRecord;
use dg_xch_core::blockchain::coin_spend::CoinSpend;
use dg_xch_core::blockchain::full_block::FullBlock;
use dg_xch_core::blockchain::mempool_item::MempoolItem as MempoolItemJson;
use dg_xch_core::blockchain::npc_result::NPCResult;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::consensus::block_generator::{
    BlockGeneratorFlags, BlockGeneratorInput, GeneratorReference, additions_for_conditions,
    coin_spend_from_generator,
};
use dg_xch_core::consensus::constants::ConsensusConstants;
use dg_xch_core::constants::{CHIA_CA_CRT, CHIA_CA_KEY};
use dg_xch_core::protocols::PeerMap;
pub use dg_xch_core::protocols::full_node::CoinQueryWindow;
use dg_xch_core::protocols::full_node::NewTransaction;
use dg_xch_core::ssl::{generate_ca_signed_cert_data, load_certs_from_bytes, make_ca_cert};
use dg_xch_core::traits::SizedBytes;
use dg_xch_core::utils::hash_256;
use dg_xch_node::slots::SlotState;
use dg_xch_node::unfinished::UnfinishedCache;
use dg_xch_node::{Mempool, MempoolError};
use dg_xch_stores::{BlockStore, CoinStore, StoreError};
use portfu::prelude::{
    ClientAuthConfig, ClientCertificateMode, TlsConfig, TlsIdentity, TlsVersionPolicy, TrustStore,
};
use std::error::Error;
use std::fmt;
use std::io::Error as IoError;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32};
use std::sync::{Arc, OnceLock};
use tokio::sync::Mutex;

// ---- bounds (every range is bounded LOUDLY — an over-cap request errors, it is never silently
// truncated) ------------------------------------------------------------------------------------

/// Most block records one `get_block_records` call may return — one full mainnet day (4608
/// blocks), the window the `get_blockchain_state` netspace math reads.
pub const MAX_BLOCK_RECORDS_PER_REQUEST: u32 = 4608;
/// Most FULL blocks (bodies included) one `get_blocks` call may return.
pub const MAX_BLOCKS_PER_REQUEST: u32 = 128;
/// Most ids (names / parent ids / puzzle hashes / hints) one coin query may carry.
pub const MAX_IDS_PER_REQUEST: usize = 32_690;
/// Request-body cap (1 MiB). An oversize body is refused with HTTP 413.
pub const MAX_RPC_BODY_BYTES: usize = 1024 * 1024;
// UI_ACTUAL_SPACE_CONSTANT_FACTOR — the netspace estimate's plot-efficiency constant.
const UI_ACTUAL_SPACE_CONSTANT_FACTOR: f64 = 0.762;

#[derive(Debug)]
pub enum RpcError {
    Store(StoreError),
    Mempool(MempoolError),
    BadRequest(String),
    Corrupt(String),
}

impl fmt::Display for RpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RpcError::Store(e) => write!(f, "store error: {e}"),
            RpcError::Mempool(e) => write!(f, "mempool rejected: {e}"),
            RpcError::BadRequest(s) => write!(f, "{s}"),
            RpcError::Corrupt(s) => write!(f, "inconsistent store: {s}"),
        }
    }
}

impl Error for RpcError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            RpcError::Store(e) => Some(e),
            RpcError::Mempool(e) => Some(e),
            RpcError::BadRequest(_) | RpcError::Corrupt(_) => None,
        }
    }
}

impl From<StoreError> for RpcError {
    fn from(e: StoreError) -> Self {
        RpcError::Store(e)
    }
}

impl From<MempoolError> for RpcError {
    fn from(e: MempoolError) -> Self {
        RpcError::Mempool(e)
    }
}

/// Server-owned live data beyond the store and mempool: the caches behind
/// `get_unfinished_block_headers` / `get_recent_signage_point_or_eos`, the inbound peer map
/// behind `get_connections`, and identity fields for `get_blockchain_state` /
/// `get_network_info`. Attached once during server construction; a `Node` without it (unit tests)
/// serves the store-backed endpoints and answers the live ones empty / not-in-cache.
pub struct NodeLive {
    /// sha256 of our RPC leaf certificate — the cert-hash node identity.
    pub node_id: Bytes32,
    /// The `--network` id (`selected_network`).
    pub network_id: String,
    pub local_port: u16,
    /// The heaviest claimed peer peak — sync_tip_height.
    pub claimed_peak: Arc<AtomicU32>,
    /// Phase-2 slot state: recent signage points + finished sub-slots.
    pub slot_state: Arc<Mutex<SlotState>>,
    /// The unfinished-block cache.
    pub unfinished: Arc<Mutex<UnfinishedCache>>,
    /// The inbound peer sessions map.
    pub inbound_peers: PeerMap,
}

/// Backend-neutral store access held by the shared route state.
pub trait RpcStore: CoinStore + BlockStore + Send + Sync {}

impl<S> RpcStore for S where S: CoinStore + BlockStore + Send + Sync {}

/// Passive state shared by Portfu routes.
pub struct Node {
    pub store: Arc<dyn RpcStore>,
    pub mempool: Arc<Mutex<Mempool>>,
    pub constants: ConsensusConstants,
    pub synced: Arc<AtomicBool>,
    /// Accepted transactions waiting for `NewTransaction` gossip.
    pub tx_announce: Arc<Mutex<Vec<NewTransaction>>>,
    /// Live server data attached before Portfu starts.
    pub live: OnceLock<NodeLive>,
    /// Optional simulator block-production control.
    pub sim: OnceLock<Arc<dyn SimControl>>,
}

impl Node {
    #[must_use]
    pub fn new<S>(
        store: Arc<S>,
        mempool: Arc<Mutex<Mempool>>,
        constants: ConsensusConstants,
        synced: Arc<AtomicBool>,
        tx_announce: Arc<Mutex<Vec<NewTransaction>>>,
    ) -> Self
    where
        S: RpcStore + 'static,
    {
        Self {
            store,
            mempool,
            constants,
            synced,
            tx_announce,
            live: OnceLock::new(),
            sim: OnceLock::new(),
        }
    }

    /// Attach the server's live state (idempotent - the first attach wins).
    pub fn attach_live(&self, live: NodeLive) {
        let _ = self.live.set(live);
    }

    /// Attach a simulator's block-production control, enabling the `farm_block` / `set_auto_farming`
    /// / `get_auto_farming` endpoints (idempotent — the first attach wins).
    pub fn attach_sim(&self, sim: Arc<dyn SimControl>) {
        let _ = self.sim.set(sim);
    }
}

pub(crate) fn check_id_cap(len: usize) -> Result<(), RpcError> {
    if len > MAX_IDS_PER_REQUEST {
        return Err(RpcError::BadRequest(format!(
            "{len} ids exceeds the {MAX_IDS_PER_REQUEST}-id cap"
        )));
    }
    Ok(())
}

pub(crate) fn sp_not_in_cache(sp_hash: &Bytes32) -> RpcError {
    RpcError::BadRequest(format!("Did not find sp {} in cache", plain_hex(sp_hash)))
}

pub(crate) fn eos_not_in_cache(challenge_hash: &Bytes32) -> RpcError {
    RpcError::BadRequest(format!(
        "Did not find eos {} in cache",
        plain_hex(challenge_hash)
    ))
}

// The v1 plot-filter halvings ladder, keyed on the newer block's height.
pub(crate) fn plot_filter_prefix_bits(constants: &ConsensusConstants, height: u32) -> u8 {
    let mut bits = constants.number_zero_bits_plot_filter;
    if height >= constants.plot_filter_32_height {
        bits = bits.saturating_sub(4);
    } else if height >= constants.plot_filter_64_height {
        bits = bits.saturating_sub(3);
    } else if height >= constants.plot_filter_128_height {
        bits = bits.saturating_sub(2);
    } else if height >= constants.hard_fork_height {
        bits = bits.saturating_sub(1);
    }
    bits
}

#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
pub(crate) fn network_space_between(
    constants: &ConsensusConstants,
    older: &BlockRecord,
    newer: &BlockRecord,
) -> Option<u128> {
    let delta_weight = newer.weight.checked_sub(older.weight)?;
    let delta_iters = newer.total_iters.checked_sub(older.total_iters)?;
    if delta_iters == 0 {
        return None;
    }
    let prefix_bits = plot_filter_prefix_bits(constants, newer.height);
    let estimate = UI_ACTUAL_SPACE_CONSTANT_FACTOR
        * (delta_weight as f64 / delta_iters as f64)
        * constants.difficulty_constant_factor as f64
        * 2f64.powi(i32::from(prefix_bits));
    Some(estimate as u128)
}

// The MempoolItem JSON shape (spend_bundle / fee / cost / npc_result / spend_bundle_name /
// additions / removals), built from the node's internal item.
pub(crate) fn mempool_item_json(item: &dg_xch_node::MempoolItem) -> MempoolItemJson {
    MempoolItemJson {
        spend_bundle: item.bundle.clone(),
        fee: item.fee,
        cost: item.cost,
        npc_result: NPCResult {
            error: None,
            conds: Some(item.conds.clone()),
        },
        spend_bundle_name: item.name,
        additions: additions_for_conditions(&item.conds, &[]),
        removals: item.bundle.removals(),
        ..Default::default()
    }
}

// Resolve a block's generator back-references from the confirmed chain and assemble the CLVM
// runner input (shared by get_puzzle_and_solution and the get_block_spends pair).
pub(crate) async fn generator_input_for_block<S: CoinStore + BlockStore + ?Sized>(
    store: &S,
    constants: &ConsensusConstants,
    block: &FullBlock,
) -> Result<BlockGeneratorInput, RpcError> {
    let Some(generator) = block.transactions_generator.clone() else {
        return Err(RpcError::BadRequest(
            "block carries no transactions generator".to_string(),
        ));
    };
    let mut generator_refs = Vec::with_capacity(block.transactions_generator_ref_list.len());
    for (index, ref_height) in block.transactions_generator_ref_list.iter().enumerate() {
        let g = store
            .get_generator_at_height(*ref_height)
            .await?
            .ok_or_else(|| {
                RpcError::Corrupt(format!("missing generator ref at height {ref_height}"))
            })?;
        generator_refs.push(GeneratorReference {
            height: *ref_height,
            index: u32::try_from(index).unwrap_or(u32::MAX),
            generator: g,
        });
    }
    Ok(BlockGeneratorInput {
        transactions_generator: generator,
        generator_refs,
        constants: *constants,
        height: block.height(),
        flags: BlockGeneratorFlags::for_height(constants, block.height()),
    })
}

/// Recover the [`CoinSpend`] (coin + puzzle reveal + solution) of a coin spent at exactly `height` by
/// re-running that block's generator through the existing CLVM runner —
/// `get_puzzle_and_solution`. Shared by the HTTP RPC and the light-wallet p2p handler
/// (`RequestPuzzleSolution`) so both paths run the one tested extraction, never a second VM.
///
/// # Errors
/// Returns [`RpcError::BadRequest`] if the coin is unknown, not spent at `height`, or its block or
/// generator is missing; [`RpcError::Store`] on a query failure; [`RpcError::Corrupt`] if a confirmed
/// generator back-reference is absent.
pub(crate) async fn puzzle_and_solution_coin_spend<S: CoinStore + BlockStore + ?Sized>(
    store: &S,
    constants: &ConsensusConstants,
    coin_id: &Bytes32,
    height: u32,
) -> Result<CoinSpend, RpcError> {
    let coin_record = store
        .get_coin_record(coin_id)
        .await?
        .ok_or_else(|| RpcError::BadRequest(format!("coin {coin_id} not found")))?;
    if !coin_record.spent || coin_record.spent_block_index != height {
        return Err(RpcError::BadRequest(format!(
            "invalid height {height} for coin {coin_id} (spent at {})",
            coin_record.spent_block_index
        )));
    }
    let record = store
        .get_block_record_by_height(height)
        .await?
        .ok_or_else(|| RpcError::BadRequest(format!("no confirmed block at height {height}")))?;
    let block = store
        .get_block(&record.header_hash)
        .await?
        .ok_or_else(|| RpcError::BadRequest(format!("block {} has no body", record.header_hash)))?;
    if block.transactions_generator.is_none() {
        return Err(RpcError::BadRequest(format!(
            "block at height {height} carries no transactions generator"
        )));
    }
    let input = generator_input_for_block(store, constants, &block).await?;
    coin_spend_from_generator(&input, coin_id)
        .map_err(|e| RpcError::BadRequest(format!("failed to extract puzzle and solution: {e:?}")))?
        .ok_or_else(|| {
            RpcError::BadRequest(format!(
                "coin {coin_id} is not spent by the generator at height {height}"
            ))
        })
}

// ---- TLS ---------------------------------------------------------------------------------------

/// TLS configuration consumed by the shared Portfu HTTP/WebSocket listener.
pub struct PortfuRpcTlsContext {
    pub tls_config: TlsConfig,
    pub node_id: Bytes32,
}

/// Build TLS for the unified Portfu listener.
///
/// Certificate presentation is optional at the TLS layer so public health and metrics routes
/// remain usable. Protected routes select one of these named stores and require a matching cert:
/// `chia-peers` for the Chia protocol and `rpc-clients` for administrative APIs.
pub fn build_portfu_rpc_tls_context(
    mode: &RpcTlsMode,
    bind: SocketAddr,
) -> Result<PortfuRpcTlsContext, IoError> {
    let rpc_ca = match mode {
        RpcTlsMode::PrivateCa { ssl_dir } => {
            let (ca_crt, ca_key) = resolve_private_ca(ssl_dir)?;
            if ca_crt == CHIA_CA_CRT.as_bytes() {
                return Err(IoError::other(
                    "refusing to root RPC client-auth at the public network CA; supply a private CA",
                ));
            }
            let _ = ca_key;
            ca_crt
        }
        RpcTlsMode::Local => {
            if !bind.ip().is_loopback() {
                return Err(IoError::other(format!(
                    "--rpc-tls local is only allowed on loopback; got {bind}"
                )));
            }
            CHIA_CA_CRT.as_bytes().to_vec()
        }
    };
    let client_auth = ClientAuthConfig {
        presentation: ClientCertificateMode::Optional,
        trust_stores: vec![
            TrustStore::new("chia-peers", CHIA_CA_CRT.as_bytes()),
            TrustStore::new("rpc-clients", &rpc_ca),
        ],
    };
    let (cert_bytes, key_bytes) =
        generate_ca_signed_cert_data(CHIA_CA_CRT.as_bytes(), CHIA_CA_KEY.as_bytes())?;
    let certs = load_certs_from_bytes(&cert_bytes)?;
    let node_id = Bytes32::new(hash_256(
        certs.first().map(AsRef::as_ref).unwrap_or_default(),
    ));
    let tls_config = TlsConfig::new(TlsIdentity::new("localhost", cert_bytes, key_bytes))
        .client_auth(client_auth)
        .versions(TlsVersionPolicy::Tls13Only);
    Ok(PortfuRpcTlsContext {
        tls_config,
        node_id,
    })
}

/// Resolve the RPC private CA: inline-PEM env override, else load-or-generate under `<ssl_dir>/ca`.
fn resolve_private_ca(ssl_dir: &Path) -> Result<(Vec<u8>, Vec<u8>), IoError> {
    if let (Ok(crt), Ok(key)) = (
        std::env::var("PRIVATE_CA_CRT"),
        std::env::var("PRIVATE_CA_KEY"),
    ) {
        return Ok((crt.into_bytes(), key.into_bytes()));
    }
    let ca_dir = ssl_dir.join("ca");
    let crt_path = ca_dir.join("private_ca.crt");
    let key_path = ca_dir.join("private_ca.key");
    if crt_path.exists() && key_path.exists() {
        return Ok((std::fs::read(&crt_path)?, std::fs::read(&key_path)?));
    }
    std::fs::create_dir_all(&ca_dir)?;
    // Generate a unique private CA ONCE and persist it. Distribute <ssl_dir>/ca/private_ca.crt to
    // RPC tooling and sign client certs with the paired key.
    let (crt, key) = make_ca_cert(&crt_path, &key_path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600));
    }
    Ok((crt, key))
}

impl dg_xch_core::errors::ErrorCode for RpcError {
    fn band(&self) -> dg_xch_core::errors::ErrorBand {
        match self {
            RpcError::Store(inner) => inner.band(),
            RpcError::Mempool(inner) => inner.band(),
            _ => dg_xch_core::errors::ErrorBand::Rpc,
        }
    }
    fn variant(&self) -> u16 {
        match self {
            RpcError::Store(inner) => inner.variant(),
            RpcError::Mempool(inner) => inner.variant(),
            RpcError::BadRequest(_) => 1,
            RpcError::Corrupt(_) => 2,
        }
    }
}
use crate::routes::rpc::plain_hex;
pub use crate::routes::rpc::{SimControl, route_names};

pub(crate) fn apply_coin_query_window(
    window: CoinQueryWindow,
    records: Vec<CoinRecord>,
) -> Vec<CoinRecord> {
    records
        .into_iter()
        .filter(|record| {
            (window.include_spent_coins || !record.spent)
                && window
                    .start_height
                    .is_none_or(|start| record.confirmed_block_index >= start)
                && window
                    .end_height
                    .is_none_or(|end| record.confirmed_block_index < end)
        })
        .collect()
}
