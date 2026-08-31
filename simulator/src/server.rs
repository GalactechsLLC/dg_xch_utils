//! Serve the peer/wallet protocol and the simulator RPC endpoints (`farm_block`,
//! `set_auto_farming`, `get_auto_farming`) from the simulator's own chain, so a wallet dials the
//! simulator as it would a full node.
//!
//! The simulator boots [`full_node::Node`] over its own store ([`Node::boot_with_store_constants`])
//! and reuses [`Node::spawn_peer_server`] + [`Node::spawn_rpc_server`]; the simulator endpoints are
//! the node's RPC surface plus a [`SimControl`] hook the node calls back into. [`Node::run`] is
//! never started: the [`ChainBuilder`] is the sole block producer, driven by `farm_block` or the
//! auto-farm loop.

use crate::chain::ChainBuilder;
use crate::error::SimError;
use crate::pos2::PlotSet;
use async_trait::async_trait;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::consensus::constants::ConsensusConstants;
use dg_xch_keys::decode_puzzle_hash;
use dg_xch_stores::SqliteStore;
use full_node::{Config, Node, SimControl};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;
use tokio::sync::Mutex;

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

/// Farm `blocks` blocks whose rewards pay `ph`, sealing any wallet-submitted transactions, and push
/// each new peak to wallet peers.
pub(crate) async fn farm_reward_blocks(
    chain: &SharedChain,
    node: &Node<SqliteStore>,
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
    node: Weak<Node<SqliteStore>>,
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
    node: Arc<Node<SqliteStore>>,
    chain: SharedChain,
    auto_farm: Arc<AtomicBool>,
    run: Arc<AtomicBool>,
    peer_run: Arc<AtomicBool>,
    rpc_run: Arc<AtomicBool>,
    control_run: Arc<AtomicBool>,
}

impl SimulatorServer {
    /// Boot the simulator behind a live peer server (for the wallet) and RPC server (for the harness).
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
    #[allow(clippy::too_many_arguments)]
    pub async fn start(
        db_path: &Path,
        listen: &str,
        rpc: &str,
        control: &str,
        network_id: &str,
        constants: ConsensusConstants,
        plots: PlotSet,
        interval: Duration,
    ) -> Result<Self, SimError> {
        // rustls 0.23 needs a process-wide CryptoProvider before any TLS handshake; match the daemon
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
            "off",
            None,
            false,
            0,
            false,
            None,
            None,
            None,
            None,
            &[],
            &[],
        )
        .map_err(SimError::Invariant)?;
        let node = Arc::new(
            Node::boot_with_store_constants(config, store.clone(), constants)
                .map_err(SimError::Io)?,
        );

        // Genesis pays a throwaway address; a wallet is funded later via `farm_block(its address)`.
        let mut builder = ChainBuilder::new(store.clone(), constants, plots, Bytes32::default());
        builder.farm_genesis().await?;
        let genesis_delta = builder.take_last_delta();
        let chain: SharedChain = Arc::new(Mutex::new(builder));
        // The simulator is the sole authority on its own chain, so it is always "synced": this clears
        // the node's NO_TRANSACTIONS_WHILE_SYNCING gate on `SendTransaction`.
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
        node.rpc.attach_sim(Arc::new(SimControlImpl {
            chain: chain.clone(),
            node: Arc::downgrade(&node),
            auto_farm: auto_farm.clone(),
        }));

        let (peer_run, _peers) = node.spawn_peer_server().map_err(SimError::Io)?;
        let rpc_run = node.spawn_rpc_server().map_err(SimError::Io)?;
        let control_addr = control
            .parse()
            .map_err(|e| SimError::Invariant(format!("bad control address {control}: {e}")))?;
        let control_run = crate::control::spawn(control_addr, chain.clone(), node.clone());

        let run = Arc::new(AtomicBool::new(true));
        Self::spawn_auto_farm(
            chain.clone(),
            node.clone(),
            auto_farm.clone(),
            run.clone(),
            interval,
        );
        Ok(Self {
            node,
            chain,
            auto_farm,
            run,
            peer_run,
            rpc_run,
            control_run,
        })
    }

    fn spawn_auto_farm(
        chain: SharedChain,
        node: Arc<Node<SqliteStore>>,
        auto_farm: Arc<AtomicBool>,
        run: Arc<AtomicBool>,
        interval: Duration,
    ) {
        tokio::spawn(async move {
            while run.load(Ordering::Relaxed) {
                tokio::time::sleep(interval).await;
                if !auto_farm.load(Ordering::Relaxed) {
                    continue;
                }
                // Seal any pending wallet transactions and advance the peak every tick, so
                // confirmations never race a stalled chain.
                let mut c = chain.lock().await;
                if c.farm_next_from_shared_mempool(&node.mempool, false)
                    .await
                    .is_ok()
                    && let Some(delta) = c.take_last_delta()
                {
                    let _ = node.notify_new_peak(&delta, None).await;
                }
            }
        });
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
    pub fn node(&self) -> &Arc<Node<SqliteStore>> {
        &self.node
    }

    /// Stop the peer server, RPC server, control server, and auto-farm loop.
    pub fn stop(&self) {
        self.run.store(false, Ordering::Relaxed);
        self.peer_run.store(false, Ordering::Relaxed);
        self.rpc_run.store(false, Ordering::Relaxed);
        self.control_run.store(false, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dg_xch_core::clvm::program::Program;
    use dg_xch_core::consensus::block_rewards::calculate_base_farmer_reward;
    use dg_xch_core::consensus::coinbase::create_farmer_coin;
    use dg_xch_stores::traits::{BlockStore, CoinStore};

    async fn start_server(dir: &Path) -> SimulatorServer {
        let db = dir.join("sim.sqlite");
        let plots = PlotSet::setup(dir, 15, 12, 18, 2, false).expect("plots");
        SimulatorServer::start(
            &db,
            "127.0.0.1:0",
            "127.0.0.1:0",
            "127.0.0.1:0",
            "simulator0",
            simulator_constants(),
            plots,
            Duration::from_millis(50),
        )
        .await
        .expect("server starts")
    }

    #[tokio::test]
    async fn farm_block_funds_an_address_through_the_shared_store() {
        let dir = tempfile::tempdir().expect("tempdir");
        let server = start_server(dir.path()).await;

        // Genesis peak is live before anything else.
        let (_, peak) = server
            .node()
            .store
            .get_peak()
            .await
            .expect("peak")
            .expect("has peak");
        assert_eq!(peak, 0);

        // Farm reward blocks to a wallet's puzzle hash; the height-1 reward is claimed by height 2,
        // creating a spendable coin at that address, visible through the served store.
        let wallet_ph = Program::to(1_u8).tree_hash();
        server.farm_to(wallet_ph, 3).await.expect("farm to address");
        let genesis = simulator_constants().genesis_challenge;
        let funded = create_farmer_coin(1, wallet_ph, calculate_base_farmer_reward(1), genesis);
        assert!(
            server
                .node()
                .store
                .get_coin_record(&funded.name())
                .await
                .expect("store")
                .is_some_and(|r| !r.spent),
            "farm_block did not fund the address"
        );
        server.stop();
    }

    #[tokio::test]
    async fn farming_past_a_sub_slot_keeps_the_node_running() {
        let dir = tempfile::tempdir().expect("tempdir");
        let server = start_server(dir.path()).await;
        let wallet_ph = Program::to(1_u8).tree_hash();
        // Farm past one sub-slot's worth of signage points; the node must cross rather than stall.
        server
            .farm_to(wallet_ph, 70)
            .await
            .expect("farm across a sub-slot");
        let (_, height) = server
            .node()
            .store
            .get_peak()
            .await
            .expect("peak")
            .expect("has peak");
        assert_eq!(
            height, 70,
            "the node stalled instead of crossing a sub-slot"
        );
        let mut crossed = false;
        for h in 1..=height {
            if let Some(rec) = server
                .node()
                .store
                .get_block_record_by_height(h)
                .await
                .expect("store")
            {
                if rec.first_in_sub_slot() {
                    crossed = true;
                    break;
                }
            }
        }
        assert!(crossed, "no sub-slot crossing occurred");
        server.stop();
    }
}
