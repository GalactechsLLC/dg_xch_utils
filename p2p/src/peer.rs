use crate::address_manager::Endpoint;
use crate::config::P2pSettings;
use dg_xch_clients::ClientSSLConfig;
use dg_xch_clients::websocket::{WsClient, WsClientConfig};
use dg_xch_core::constants::{CHIA_CA_CRT, CHIA_CA_KEY};
use dg_xch_core::protocols::{ChiaMessageHandler, NodeType};
use dg_xch_serialize::ChiaProtocolVersion;
use std::collections::{HashMap, HashSet};
use std::io::Error;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use tokio::sync::RwLock;
use uuid::Uuid;

#[derive(Debug, PartialEq, Eq)]
pub enum AdmitError {
    InboundCapReached,
    DuplicateEndpoint,
    SelfConnection,
}

// One live outbound channel = one sync-reservation slot. The
// connection is reachable for the sync layer to issue RequestBlocks against.
pub struct OutboundPeer {
    pub endpoint: Endpoint,
    pub client: WsClient,
    pub run: Arc<AtomicBool>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DialIdentity {
    pub network_id: String,
    pub server_port: u16,
}

impl Default for DialIdentity {
    fn default() -> Self {
        Self {
            network_id: "mainnet".to_string(),
            server_port: 8444,
        }
    }
}
impl OutboundPeer {
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.client.is_closed()
    }
    pub fn stop(&self) {
        self.run.store(false, std::sync::atomic::Ordering::Relaxed);
    }
}

// Shared connection bookkeeping: the inbound cap, duplicate-endpoint reserve, and
// self-connection detection (NAT hairpin), plus the live outbound set for the sync seam.
pub struct PeerRegistry {
    inner: RwLock<Inner>,
    pub settings: P2pSettings,
    pub self_nonce: u64,
}
#[derive(Default)]
struct Inner {
    outbound: Vec<Arc<OutboundPeer>>,
    inbound: HashSet<Endpoint>,
    endpoints: HashSet<Endpoint>,
    selfs: HashSet<Endpoint>,
}

impl PeerRegistry {
    #[must_use]
    pub fn new(settings: P2pSettings) -> Self {
        Self {
            inner: RwLock::new(Inner::default()),
            settings,
            self_nonce: rand::random(),
        }
    }

    pub async fn add_self(&self, ep: Endpoint) {
        self.inner.write().await.selfs.insert(ep);
    }

    // Admit only while total < target_peer_count, and reject a duplicate endpoint or a dial
    // that resolves to our own authority.
    pub async fn admit_inbound(&self, ep: &Endpoint) -> Result<(), AdmitError> {
        let mut g = self.inner.write().await;
        if g.selfs.contains(ep) {
            return Err(AdmitError::SelfConnection);
        }
        if g.endpoints.contains(ep) {
            return Err(AdmitError::DuplicateEndpoint);
        }
        if g.inbound.len() + g.outbound.len() >= self.settings.target_peer_count {
            return Err(AdmitError::InboundCapReached);
        }
        g.inbound.insert(ep.clone());
        g.endpoints.insert(ep.clone());
        Ok(())
    }

    pub async fn release_inbound(&self, ep: &Endpoint) {
        let mut g = self.inner.write().await;
        g.inbound.remove(ep);
        g.endpoints.remove(ep);
    }

    pub async fn reserve_outbound(&self, ep: &Endpoint) -> Result<(), AdmitError> {
        let mut g = self.inner.write().await;
        if g.selfs.contains(ep) {
            return Err(AdmitError::SelfConnection);
        }
        if g.endpoints.contains(ep) {
            return Err(AdmitError::DuplicateEndpoint);
        }
        g.endpoints.insert(ep.clone());
        Ok(())
    }

    pub async fn register_outbound(&self, peer: Arc<OutboundPeer>) {
        self.inner.write().await.outbound.push(peer);
    }

    pub async fn release_outbound(&self, ep: &Endpoint) {
        let mut g = self.inner.write().await;
        g.outbound.retain(|p| &p.endpoint != ep);
        g.endpoints.remove(ep);
    }

    // The sync seam: iterate the live outbound channels (reservation slots).
    pub async fn outbound_peers(&self) -> Vec<Arc<OutboundPeer>> {
        self.inner.read().await.outbound.clone()
    }

    // Evict a misbehaving/slow peer: stop its channel. The owning slot observes the stop,
    // releases the reservation, and re-dials — eviction never revives the dead channel.
    pub async fn evict(&self, ep: &Endpoint) -> bool {
        if let Some(p) = self
            .inner
            .read()
            .await
            .outbound
            .iter()
            .find(|p| &p.endpoint == ep)
        {
            p.stop();
            return true;
        }
        false
    }

    pub async fn outbound_count(&self) -> usize {
        self.inner.read().await.outbound.len()
    }

    pub async fn inbound_count(&self) -> usize {
        self.inner.read().await.inbound.len()
    }
}

pub fn empty_handlers() -> Arc<RwLock<HashMap<Uuid, Arc<ChiaMessageHandler>>>> {
    Arc::new(RwLock::new(HashMap::new()))
}

/// Builds a FRESH per-connection handler map on each dial. A fresh map per connection is required: the
/// oneshot request machinery subscribes/unsubscribes temporary handlers on the same map, so sharing one map
/// across peers would cross-deliver responses. The server supplies `full_node_handlers_client` here so
/// outbound peers dispatch NewPeak/RequestBlock/gossip; `None` falls back to `empty_handlers`.
pub type HandlerFactory = Arc<dyn Fn() -> HashMap<Uuid, Arc<ChiaMessageHandler>> + Send + Sync>;

// Build the per-connection handler map for a dial: the factory's fresh map, or an empty one.
pub(crate) fn dial_handlers(
    factory: Option<&HandlerFactory>,
) -> Arc<RwLock<HashMap<Uuid, Arc<ChiaMessageHandler>>>> {
    match factory {
        Some(f) => Arc::new(RwLock::new(f())),
        None => empty_handlers(),
    }
}

pub async fn dial(
    host: &str,
    port: u16,
    handlers: Arc<RwLock<HashMap<Uuid, Arc<ChiaMessageHandler>>>>,
    run: Arc<AtomicBool>,
    settings: &P2pSettings,
) -> Result<WsClient, Error> {
    dial_with_identity(
        host,
        port,
        handlers,
        run,
        settings,
        &DialIdentity::default(),
    )
    .await
}

pub async fn dial_with_identity(
    host: &str,
    port: u16,
    handlers: Arc<RwLock<HashMap<Uuid, Arc<ChiaMessageHandler>>>>,
    run: Arc<AtomicBool>,
    settings: &P2pSettings,
    identity: &DialIdentity,
) -> Result<WsClient, Error> {
    let config = Arc::new(WsClientConfig {
        host: host.to_string(),
        port,
        server_port: identity.server_port,
        network_id: identity.network_id.clone(),
        ssl_info: None::<ClientSSLConfig>,
        software_version: None,
        protocol_version: ChiaProtocolVersion::default(),
        additional_headers: None,
        // Outbound full-node link: police what the peer sends us, so a peer we dial to sync from
        // cannot flood us and our own solicited RespondBlocks bursts are accounted at the read
        // loop.
        rate_limited: true,
    });
    let timeout = settings.connect_timeout.as_secs();
    let runtime = tokio::runtime::Handle::current();
    let client = tokio::task::spawn_blocking(move || {
        runtime.block_on(WsClient::with_ca(
            config,
            NodeType::FullNode,
            handlers,
            run,
            CHIA_CA_CRT.as_bytes(),
            CHIA_CA_KEY.as_bytes(),
            timeout,
        ))
    })
    .await
    .map_err(|error| Error::other(format!("connection task failed: {error}")))??;
    Ok(client)
}

impl dg_xch_core::errors::ErrorCode for AdmitError {
    fn band(&self) -> dg_xch_core::errors::ErrorBand {
        dg_xch_core::errors::ErrorBand::Peer
    }

    fn variant(&self) -> u16 {
        match self {
            AdmitError::InboundCapReached => 1,
            AdmitError::DuplicateEndpoint => 2,
            AdmitError::SelfConnection => 3,
        }
    }
}

#[cfg(test)]
#[path = "../tests/unit/peer/tests.rs"]
mod tests;
