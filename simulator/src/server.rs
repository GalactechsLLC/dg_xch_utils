//! Serve the peer/wallet protocol and the simulator RPC endpoints (`farm_block`,
//! `set_auto_farming`, `get_auto_farming`) from the simulator's own chain, so a wallet dials the
//! simulator as it would a full node.
//!
//! The simulator boots [`dg_full_node::FullNode`] over its own store ([`FullNode::boot_with_store_constants`])
//! and reuses the full-node Portfu routes and protocol services; the simulator endpoints are
//! the node's RPC surface plus a [`SimControl`] hook the node calls back into. [`FullNode::run`] is
//! never started: the [`ChainBuilder`] is the sole block producer, driven by `farm_block` or the
//! auto-farm loop.

use crate::chain::ChainBuilder;
use crate::error::SimError;
use crate::pos2::PlotSet;
use async_trait::async_trait;
use dg_full_node::server::{ActiveNode, BackendHandle, NodeServices};
use dg_full_node::{Config, FullNode, SimControl};
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::consensus::constants::ConsensusConstants;
use dg_xch_keys::decode_puzzle_hash;
use dg_xch_stores::SqliteStore;
use portfu::prelude::{ServerBuilder, ServerHandle};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

/// The consensus constants a served simulator runs under: the `SIMULATOR` base (pos2 active from
/// height 0) with a small plot size, a permissive plot filter, and a 16-bit-discriminant VDF over a
/// small sub-slot. The genesis challenge and `AGG_SIG_ME` data stay at their mainnet values, so a
/// mainnet wallet syncs and spends against it unchanged.
#[must_use]
pub fn simulator_constants() -> ConsensusConstants {
    use dg_xch_core::consensus::constants::{MAINNET, SIMULATOR};
    use dg_xch_core::consensus::overrides::{ConsensusOverrides, apply_overrides};
    apply_overrides(
        SIMULATOR,
        &ConsensusOverrides {
            plot_size_v2: Some(18),
            number_zero_bits_plot_filter_v2: Some(0),
            difficulty_constant_factor: Some(2u128.pow(25)),
            difficulty_starting: Some(7),
            discriminant_size_bits: Some(num_bigint::BigInt::from(16)),
            sub_slot_iters_starting: Some(65_536),
            genesis_challenge: Some(MAINNET.genesis_challenge),
            agg_sig_me_additional_data: Some(MAINNET.agg_sig_me_additional_data),
            ..Default::default()
        },
    )
}

pub(crate) type SharedChain = Arc<Mutex<ChainBuilder<Arc<SqliteStore>>>>;

pub(crate) struct AutoFarmTaskState {
    pub(crate) chain: SharedChain,
    pub(crate) node: Arc<FullNode<SqliteStore>>,
    pub(crate) enabled: Arc<AtomicBool>,
    pub(crate) interval: Duration,
}

/// Farm `blocks` blocks whose rewards pay `ph`, sealing any wallet-submitted transactions, and push
/// each new peak to wallet peers.
pub(crate) async fn farm_reward_blocks(
    chain: &SharedChain,
    node: &FullNode<SqliteStore>,
    ph: Bytes32,
    blocks: u32,
) -> Result<(), SimError> {
    let mut chain = chain.lock().await;
    chain.set_reward_ph(ph);
    for _ in 0..blocks.max(1) {
        chain
            .farm_next_from_shared_mempool(&node.mempool, true)
            .await?;
        if let Some(delta) = chain.take_last_delta() {
            node.notify_new_peak(&delta, None)
                .await
                .map_err(SimError::Io)?;
        }
    }
    Ok(())
}

/// The [`SimControl`] the node's RPC calls into for `farm_block` / `set_auto_farming` /
/// `get_auto_farming`. Holds a weak node handle to avoid the node → rpc → control → node cycle.
struct SimControlImpl {
    chain: SharedChain,
    node: Weak<FullNode<SqliteStore>>,
    auto_farm: Arc<AtomicBool>,
}

#[async_trait]
impl SimControl for SimControlImpl {
    async fn farm_block(
        &self,
        address: &str,
        blocks: u32,
        _guarantee_tx_block: bool,
    ) -> Result<(), String> {
        let ph = decode_puzzle_hash(address).map_err(|e| format!("bad address: {e}"))?;
        let node = self.node.upgrade().ok_or("node stopped")?;
        farm_reward_blocks(&self.chain, &node, ph, blocks)
            .await
            .map_err(|e| e.to_string())
    }

    fn set_auto_farming(&self, should_auto_farm: bool) -> bool {
        self.auto_farm.store(should_auto_farm, Ordering::Relaxed);
        should_auto_farm
    }

    fn auto_farming(&self) -> bool {
        self.auto_farm.load(Ordering::Relaxed)
    }
}

/// A running simulator that serves the peer/wallet protocol and the simulator RPC from its chain.
pub struct SimulatorServer {
    node: Arc<FullNode<SqliteStore>>,
    chain: SharedChain,
    auto_farm: Arc<AtomicBool>,
    services: Arc<NodeServices>,
    server: ServerHandle,
    server_task: std::sync::Mutex<Option<JoinHandle<()>>>,
}

impl SimulatorServer {
    /// Boot the simulator behind a live peer server and shared Portfu server.
    /// The genesis block is farmed and its peak published before either server accepts, so a wallet
    /// that connects immediately receives the `NewPeakWallet` greeting it requires. The auto-farm loop
    /// seals a block whenever a wallet has a pending transaction, but only while auto-farming is on
    /// (off by default; `set_auto_farming` or `farm_block` drive it).
    ///
    /// `network_id` is the handshake network the wallet must be configured for; `constants` are the
    /// consensus constants the node serves and the chain farms under.
    ///
    /// # Errors
    /// Propagates store-open, node-boot, farming, and server-start failures.
    pub async fn start(
        db_path: &Path,
        listen: &str,
        rpc: &str,
        network_id: &str,
        constants: ConsensusConstants,
        plots: PlotSet,
        interval: Duration,
    ) -> Result<Self, SimError> {
        // rustls 0.23 needs a process-wide CryptoProvider before any TLS handshake; match the server
        // (ring). Idempotent — a second install is a no-op.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let store = Arc::new(
            SqliteStore::open(db_path)
                .await
                .map_err(|e| SimError::Invariant(format!("open store: {e}")))?,
        );
        let config = Config::build(
            listen,
            rpc,
            None,
            &[],
            None,
            "sqlite://simulator",
            network_id,
            None,
            false,
            0,
            false,
            None,
            None,
            Default::default(),
            &[],
            &[],
        )
        .map_err(SimError::Invariant)?;
        let rpc_bind = config.rpc;
        let tls =
            dg_full_node::build_portfu_rpc_tls_context(&config.rpc_tls).map_err(SimError::Io)?;
        let node = Arc::new(
            FullNode::boot_with_store_constants(config, store.clone(), constants)
                .map_err(SimError::Io)?,
        );

        // Genesis pays a throwaway address; a wallet is funded later via `farm_block(its address)`.
        let mut builder = ChainBuilder::new(store.clone(), constants, plots, Bytes32::default());
        builder.farm_genesis().await?;
        let genesis_delta = builder.take_last_delta();
        let chain: SharedChain = Arc::new(Mutex::new(builder));
        node.synced.store(true, Ordering::Relaxed);
        // Serve stock wallets a v1-shaped proof of space in block headers so they can deserialize
        // `RespondBlockHeader` (a stock wallet has no v2 proof decoder). The chain keeps its v2 proofs.
        node.wallet_compat.store(true, Ordering::Relaxed);
        if let Some(delta) = genesis_delta {
            node.notify_new_peak(&delta, None)
                .await
                .map_err(SimError::Io)?;
        }

        let auto_farm = Arc::new(AtomicBool::new(false));
        node.state.attach_sim(Arc::new(SimControlImpl {
            chain: chain.clone(),
            node: Arc::downgrade(&node),
            auto_farm: auto_farm.clone(),
        }));
        node.attach_rpc_live(tls.node_id);
        let services = Arc::new(node.start_services().await.map_err(SimError::Io)?);
        let sources = Arc::new(node.metrics_sources(&services));
        let active = Arc::new(ActiveNode::Sqlite(BackendHandle {
            node: node.clone(),
            sources,
        }));
        let state = active.state();
        let portfu_server = ServerBuilder::new()
            .host(rpc_bind.ip().to_string())
            .port(rpc_bind.port())
            .tls(tls.tls_config)
            .global_state::<ActiveNode>(active)
            .global_state::<NodeServices>(services.clone())
            .global_state::<dg_full_node::Node>(state)
            .scoped_state::<_, AutoFarmTaskState>(
                "simulator",
                Arc::new(AutoFarmTaskState {
                    chain: chain.clone(),
                    node: node.clone(),
                    enabled: auto_farm.clone(),
                    interval,
                }),
            )
            .build();
        let server = portfu_server.handle();
        let server_task = tokio::spawn(async move {
            if let Err(error) = portfu_server.run().await {
                log::error!("simulator Portfu server failed: {error}");
            }
        });
        Ok(Self {
            node,
            chain,
            auto_farm,
            services,
            server,
            server_task: std::sync::Mutex::new(Some(server_task)),
        })
    }

    /// Farm `blocks` blocks whose rewards pay `ph`, funding that address — the direct form of the
    /// `farm_block` RPC.
    ///
    /// # Errors
    /// Propagates farming and peak-notification failures.
    pub async fn farm_to(&self, ph: Bytes32, blocks: u32) -> Result<(), SimError> {
        farm_reward_blocks(&self.chain, &self.node, ph, blocks).await
    }

    /// Turn auto-farming on or off.
    pub fn set_auto_farming(&self, on: bool) {
        self.auto_farm.store(on, Ordering::Relaxed);
    }

    /// The served node, for tests that assert on its store or wallet notifier.
    #[must_use]
    pub fn node(&self) -> &Arc<FullNode<SqliteStore>> {
        &self.node
    }

    /// Stop the peer server, shared Portfu server, and auto-farm loop.
    pub async fn stop(&self) {
        self.node.run.store(false, Ordering::Relaxed);
        self.services.begin_shutdown();
        self.server.shutdown();
        let task = self.server_task.lock().expect("server task lock").take();
        if let Some(task) = task {
            let _ = task.await;
        }
        self.services.drain().await;
    }
}

#[cfg(test)]
#[path = "../tests/unit/server/tests.rs"]
mod tests;
