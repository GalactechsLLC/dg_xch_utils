//! Portfu server state: one type-erased handle over the concrete store
//! instantiations so tasks and endpoints register once, plus the shared service
//! state (peer registry, run flags, supervisor) the tasks operate through.

use crate::config::{Backend, Config};
use crate::metrics::MetricsSources;
use crate::node::FullNode;
use crate::node::OutboundPeers;
use dg_logger::DruidGardenLogger;
use dg_xch_core::protocols::PeerMap;
use dg_xch_p2p::sessions::Supervisor;
use dg_xch_servers::websocket::WebsocketServer;
use dg_xch_stores::SqliteStore;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// A concrete backend instantiation: the node plus its metrics/health sources.
pub struct BackendHandle<S> {
    pub node: Arc<FullNode<S>>,
    pub sources: Arc<MetricsSources<S>>,
}

/// The one process-wide node handle. `State<T>` is type-keyed and task
/// registration is monomorphic, so the generic `FullNode<S>` is carried behind this
/// enum and every portfu construct dispatches through it.
pub enum ActiveNode {
    Sqlite(BackendHandle<SqliteStore>),
    #[cfg(feature = "postgres")]
    Postgres(BackendHandle<dg_xch_stores::PostgresStore>),
    #[cfg(feature = "mmap")]
    Mmap(BackendHandle<dg_xch_stores::MmapStore>),
}

macro_rules! with_backend {
    ($self:expr, $h:ident => $body:expr) => {
        match $self {
            ActiveNode::Sqlite($h) => $body,
            #[cfg(feature = "postgres")]
            ActiveNode::Postgres($h) => $body,
            #[cfg(feature = "mmap")]
            ActiveNode::Mmap($h) => $body,
        }
    };
}

impl ActiveNode {
    #[must_use]
    pub fn backend_name(&self) -> &'static str {
        match self {
            ActiveNode::Sqlite(_) => "sqlite",
            #[cfg(feature = "postgres")]
            ActiveNode::Postgres(_) => "postgres",
            #[cfg(feature = "mmap")]
            ActiveNode::Mmap(_) => "mmap",
        }
    }

    #[must_use]
    pub fn debug_endpoints(&self) -> bool {
        with_backend!(self, h => h.node.config.debug_endpoints)
    }

    pub async fn metrics_text(&self) -> String {
        with_backend!(self, h => h.sources.metrics_text().await)
    }

    pub async fn health_check(&self) -> (&'static str, String) {
        with_backend!(self, h => h.sources.health_check().await)
    }

    pub async fn status_json(&self) -> String {
        with_backend!(self, h => {
            let snap = h.sources.sample_liveness().await;
            let (health, _) = h.sources.health_check().await;
            format!(
                "{{\"backend\":\"{}\",\"peak\":{},\"claimed\":{},\"tip_lag\":{},\"healthy\":{}}}",
                self.backend_name(),
                snap.peak_height,
                snap.claimed_peak,
                snap.tip_lag,
                health.starts_with("200"),
            )
        })
    }

    pub fn set_run(&self, value: bool) {
        with_backend!(self, h => h.node.run.store(value, Ordering::Relaxed));
    }

    pub async fn shutdown(&self) {
        self.set_run(false);
        with_backend!(self, h => h.node.stop_maintenance().await);
    }

    #[must_use]
    pub fn is_running(&self) -> bool {
        with_backend!(self, h => h.node.run.load(Ordering::Relaxed))
    }

    pub async fn run_sync_driver(&self, services: &NodeServices) {
        with_backend!(self, h => {
            crate::node::sync_driver(
                h.node.clone(),
                services.registry.clone(),
                services.inbound_peers.clone(),
            )
            .await;
        });
    }

    pub async fn run_tip_follower(&self, services: &NodeServices) {
        with_backend!(self, h => {
            crate::node::tip_follower(
                h.node.clone(),
                services.registry.clone(),
                services.inbound_peers.clone(),
            )
            .await;
        });
    }

    pub async fn reap_wallet_subscriptions(&self, services: &NodeServices) {
        with_backend!(self, h => {
            crate::node::reap_wallet_subscriptions_once(&h.node, &services.inbound_peers).await;
        });
    }

    pub async fn run_tx_validator(&self) {
        with_backend!(self, h => h.node.clone().run_tx_validator().await);
    }

    pub async fn run_weight_proof_worker(&self) {
        with_backend!(self, h => h.node.clone().run_weight_proof_worker().await);
    }

    pub async fn run_uncompact_scanner(&self) {
        with_backend!(self, h => h.node.clone().run_uncompact_scanner().await);
    }

    #[must_use]
    pub fn state(&self) -> Arc<crate::rpc::Node> {
        with_backend!(self, h => h.node.state.clone())
    }
}

pub struct NodeServices {
    pub registry: Arc<dyn OutboundPeers>,
    pub peer_registry: Arc<dg_xch_p2p::PeerRegistry>,
    pub inbound_peers: PeerMap,
    pub peer_run: Arc<AtomicBool>,
    pub peer_server: Arc<WebsocketServer>,
    pub introducer: Option<(String, u16)>,
    pub supervisor: tokio::sync::Mutex<Option<Supervisor>>,
}

impl NodeServices {
    pub async fn run_peer_supervisor(&self) {
        {
            let mut supervisor = self.supervisor.lock().await;
            let Some(supervisor) = supervisor.as_mut() else {
                return;
            };
            if let Some((host, port)) = &self.introducer {
                supervisor.start_introducer(host, *port);
            }
            supervisor.start_outbound();
        }
        while self.peer_run.load(Ordering::Relaxed) {
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
        if let Some(supervisor) = self.supervisor.lock().await.as_mut() {
            supervisor.stop().await;
        }
    }

    /// Signal protocol services to stop accepting or initiating peer work.
    pub fn begin_shutdown(&self) {
        self.peer_run.store(false, Ordering::Relaxed);
    }

    /// Stop the protocol servers and the supervisor. Called once after the
    /// portfu server exits.
    pub async fn drain(&self) {
        self.begin_shutdown();
        if let Some(mut supervisor) = self.supervisor.lock().await.take() {
            supervisor.stop().await;
        }
    }
}

/// Run the full-node server until Portfu shuts down.
///
/// # Errors
/// Returns startup, backend, listener, or shutdown errors.
pub async fn run(
    config: Config,
    logger: Arc<DruidGardenLogger>,
) -> Result<(), Box<dyn std::error::Error>> {
    let server_bind = config.listen;
    if config.rpc != server_bind {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "the unified Portfu server has one bind; configured --listen={server_bind} but --rpc={}",
                config.rpc
            ),
        )
        .into());
    }
    let tls = crate::build_portfu_rpc_tls_context(&config.rpc_tls)?;
    let node_id = tls.node_id;

    macro_rules! activate {
        ($variant:ident, $node:expr) => {{
            let node = $node;
            node.attach_rpc_live(node_id);
            let services = node.start_services().await?;
            let sources = Arc::new(node.metrics_sources(&services));
            (
                ActiveNode::$variant(BackendHandle { node, sources }),
                services,
            )
        }};
    }

    let (active, services) = match config.backend.clone() {
        Backend::Sqlite(_) => activate!(Sqlite, Arc::new(FullNode::boot(config).await?)),
        #[cfg(feature = "postgres")]
        Backend::Postgres(url) => {
            let store = Arc::new(
                dg_xch_stores::PostgresStore::open(&url)
                    .await
                    .map_err(|error| std::io::Error::other(format!("open postgres: {error}")))?,
            );
            activate!(
                Postgres,
                Arc::new(FullNode::boot_with_store(config, store)?)
            )
        }
        #[cfg(not(feature = "postgres"))]
        Backend::Postgres(url) => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                format!(
                    "postgres:// --db ({url}) requires a binary built with --features postgres"
                ),
            )
            .into());
        }
        #[cfg(feature = "mmap")]
        Backend::Mmap(directory) => {
            let store = Arc::new(
                dg_xch_stores::MmapStore::open(&directory)
                    .await
                    .map_err(|error| std::io::Error::other(format!("open mmap store: {error}")))?,
            );
            activate!(Mmap, Arc::new(FullNode::boot_with_store(config, store)?))
        }
        #[cfg(not(feature = "mmap"))]
        Backend::Mmap(directory) => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                format!(
                    "mmap:// --db ({}) requires a binary built with --features mmap",
                    directory.display()
                ),
            )
            .into());
        }
    };

    let host = server_bind.ip().to_string();
    let port = server_bind.port();
    log::info!(
        "portfu server hosting Chia peers/RPC/metrics/health/sockets/tasks backend={} bind={host}:{port}",
        active.backend_name()
    );
    let active = Arc::new(active);
    let services = Arc::new(services);
    let server = portfu::prelude::ServerBuilder::new()
        .host(host)
        .port(port)
        .tls(tls.tls_config)
        .global_state::<ActiveNode>(active.clone())
        .global_state::<NodeServices>(services.clone())
        .global_state::<crate::Node>(active.state())
        .global_state::<DruidGardenLogger>(logger)
        .build();
    let result = server.run().await;
    active.shutdown().await;
    services.drain().await;
    result.map_err(|error| std::io::Error::other(error.to_string()))?;
    Ok(())
}

#[cfg(test)]
#[path = "../tests/unit/server.rs"]
mod tests;
