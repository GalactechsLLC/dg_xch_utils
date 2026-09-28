use crate::PROTOCOL_VERSION;
use crate::farmer::config::Config;
use crate::farmer::protocols::fullnode::new_signage_point::NewSignagePointHandle;
use crate::farmer::protocols::fullnode::request_signed_values::RequestSignedValuesHandle;
use crate::harvesters::UnifiedHarvester;
use crate::harvesters::{Harvester, ProofHandler, SignatureHandler};
use crate::utils::{get_ssl_root_path, load_client_id};
use dg_xch_clients::ClientSSLConfig;
use dg_xch_clients::api::pool::PoolClient;
use dg_xch_clients::websocket::WsClientConfig;
use dg_xch_clients::websocket::farmer::FarmerClient;
use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes48};

use dg_xch_core::protocols::farmer::FarmerSharedState;
use dg_xch_core::protocols::{ChiaMessageFilter, ChiaMessageHandler, ProtocolMessageTypes};
use dg_xch_pos::plots::disk_plot::DiskPlot;
use dg_xch_pos::plots::plot_reader::PlotReader;
use log::{error, info};
use once_cell::sync::Lazy;
use std::hash::{Hash, Hasher};
use std::io::Error;
use std::marker::PhantomData;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::fs::File;
use tokio::sync::RwLock;
use uuid::Uuid;

pub mod config;
pub mod protocols;

struct ConnectionGuard(Arc<AtomicBool>);

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

pub static PUBLIC_CRT: &str = "farmer/public_farmer.crt";
pub static PUBLIC_KEY: &str = "farmer/public_farmer.key";
pub static CA_PUBLIC_CRT: &str = "ca/chia_ca.crt";
pub static PRIVATE_CRT: &str = "farmer/private_farmer.crt";
pub static PRIVATE_KEY: &str = "farmer/private_farmer.key";
pub static CA_PRIVATE_CRT: &str = "ca/private_ca.crt";

pub static HARVESTER_CRT: &str = "harvester/private_harvester.crt";

#[derive(Debug, Clone)]
pub struct PathInfo {
    pub path: PathBuf,
    pub file_name: String,
    identifier: String,
}
impl PathInfo {
    pub fn new(path: PathBuf) -> Self {
        let file_name = path
            .file_name()
            .map(|s| s.to_str().unwrap_or_default())
            .unwrap_or_default()
            .to_string();
        let identifier = hex::encode(dg_xch_core::utils::hash_256(
            path.as_os_str().as_encoded_bytes(),
        ));
        Self {
            path,
            file_name,
            identifier,
        }
    }

    pub fn identifier(&self) -> &str {
        &self.identifier
    }
}
impl std::borrow::Borrow<str> for PathInfo {
    fn borrow(&self) -> &str {
        self.identifier()
    }
}
impl Hash for PathInfo {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.identifier.hash(state)
    }
}
impl Eq for PathInfo {}
impl PartialEq for PathInfo {
    fn eq(&self, other: &Self) -> bool {
        self.identifier == other.identifier
    }
}

#[derive(Debug)]
pub struct PlotInfo {
    pub reader: PlotReader<File, DiskPlot<File>>,
    pub pool_public_key: Option<Bytes48>,
    pub pool_contract_puzzle_hash: Option<Bytes32>,
    pub plot_public_key: Bytes48,
    pub file_size: u64,
    pub time_modified: u64,
}

pub struct Farmer<P, O, S, T = (), H = UnifiedHarvester<T>, C = ()>
where
    P: PoolClient + Sized + Sync + Send + 'static,
    O: ProofHandler<T, H, C> + Sync + Send + 'static,
    S: SignatureHandler<T, H, C> + Sync + Send + 'static,
    T: Sync + Send + 'static,
    H: Harvester<T, H, C> + Sync + Send + 'static,
    C: Sync + Send + Clone + 'static,
{
    shared_state: Arc<FarmerSharedState<T>>,
    harvester: Arc<H>,
    pool_client: Arc<P>,
    full_node_client: Arc<RwLock<Option<FarmerClient<T>>>>,
    config: Arc<RwLock<Config<C>>>,
    phantom_proof_handler: PhantomData<O>,
    phantom_signature_handler: PhantomData<S>,
}
impl<P, O, S, T, H, C> Farmer<P, O, S, T, H, C>
where
    P: PoolClient + Default + Sized + Sync + Send + 'static,
    O: ProofHandler<T, H, C> + Sync + Send + 'static,
    S: SignatureHandler<T, H, C> + Sync + Send + 'static,
    T: Sync + Send + 'static,
    H: Harvester<T, H, C> + Sync + Send + 'static,
    C: Sync + Send + Clone + 'static,
{
    pub async fn new(
        shared_state: Arc<FarmerSharedState<T>>,
        pool_client: Arc<P>,
        harvester: Arc<H>,
        config: Arc<RwLock<Config<C>>>,
    ) -> Result<Self, Error> {
        Ok(Self {
            shared_state,
            harvester,
            pool_client,
            full_node_client: Default::default(),
            config,
            phantom_proof_handler: Default::default(),
            phantom_signature_handler: Default::default(),
        })
    }

    pub async fn run(self) {
        let s = self;
        let mut client_run = Arc::new(AtomicBool::new(true));
        let mut connection_guard = ConnectionGuard(client_run.clone());
        let mut last_clear = Instant::now();
        'retry: loop {
            if !s.shared_state.signal.load(Ordering::Relaxed) {
                break;
            }
            let config = s.config.read().await;
            info!(
                "Starting Farmer FullNode Connection to: {}:{}",
                config.fullnode_ws_host, config.fullnode_ws_port
            );
            loop {
                if !s.shared_state.signal.load(Ordering::Relaxed) {
                    break;
                }
                if let Some(client) = &*s.full_node_client.read().await {
                    client_run.store(false, Ordering::Relaxed);
                    client
                        .client
                        .connection
                        .write()
                        .await
                        .shutdown()
                        .await
                        .unwrap_or_default();
                }
                client_run = Arc::new(AtomicBool::new(true));
                connection_guard.0 = client_run.clone();
                match s
                    .create_farmer_client(
                        s.shared_state.clone(),
                        s.config.clone(),
                        client_run.clone(),
                    )
                    .await
                {
                    Ok(c) => {
                        if let Some(handshake) = &c.client.handshake {
                            info!(
                                "Using node with Upstream Version: {}",
                                handshake.software_version
                            );
                        } else {
                            error!("Failed to read chia version from client handshake");
                        }
                        *s.full_node_client.write().await = Some(c);
                        if let Err(e) = s
                            .attach_client_handlers(s.full_node_client.clone(), s.config.clone())
                            .await
                        {
                            error!("Failed to attach socket listeners: {e:?}");
                            continue;
                        } else {
                            info!("Farmer Client Initialized");
                            break;
                        }
                    }
                    Err(e) => {
                        error!("Failed to Start Farmer Client, Waiting and trying again: {e:?}");
                        tokio::time::sleep(Duration::from_secs(3)).await;
                        continue;
                    }
                }
            }
            loop {
                if let Some(client) = s.full_node_client.read().await.as_ref()
                    && client.is_closed()
                {
                    if !s.shared_state.signal.load(Ordering::Relaxed) {
                        info!("Farmer Stopped");
                        break 'retry;
                    } else {
                        info!("Unexpected Farmer Client Closed, Reconnecting");
                        break;
                    }
                }
                let dur = Instant::now()
                    .duration_since(*s.shared_state.last_sp_timestamp.read().await)
                    .as_secs();
                if dur >= 60 {
                    info!(
                        "Failed to get Signage Point after {dur} seconds, restarting farmer client"
                    );
                    *s.shared_state.last_sp_timestamp.write().await = Instant::now();
                    if let Some(c) = &*s.full_node_client.read().await {
                        info!(
                            "Shutting Down old Farmer Client: {}:{}",
                            config.fullnode_ws_host, config.fullnode_ws_port
                        );
                        client_run.store(false, Ordering::Relaxed);
                        c.client
                            .connection
                            .write()
                            .await
                            .shutdown()
                            .await
                            .unwrap_or_default();
                        break;
                    }
                }
                if last_clear.elapsed() > Duration::from_secs(300) {
                    let expired: Vec<Bytes32> = s
                        .shared_state
                        .cache_time
                        .write()
                        .await
                        .iter()
                        .filter_map(|(k, v)| {
                            if v.elapsed() > Duration::from_secs(1800) {
                                Some(*k)
                            } else {
                                None
                            }
                        })
                        .collect();
                    s.shared_state
                        .cache_time
                        .write()
                        .await
                        .retain(|k, _| !expired.contains(k));
                    s.shared_state
                        .signage_points
                        .write()
                        .await
                        .retain(|k, _| !expired.contains(k));
                    s.shared_state
                        .quality_to_identifiers
                        .write()
                        .await
                        .retain(|k, _| !expired.contains(k));
                    s.shared_state
                        .proofs_of_space
                        .write()
                        .await
                        .retain(|k, _| !expired.contains(k));
                    last_clear = Instant::now();
                }
                if !s.shared_state.signal.load(Ordering::Relaxed) {
                    info!("Farmer Stopping");
                    break 'retry;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        }
    }

    async fn create_farmer_client(
        &self,
        shared_state: Arc<FarmerSharedState<T>>,
        config: Arc<RwLock<Config<C>>>,
        client_run: Arc<AtomicBool>,
    ) -> Result<FarmerClient<T>, Error> {
        let config = config.read().await;
        let network_id = config.network_id()?;
        let ssl_path = get_ssl_root_path(&*config)?;
        crate::utils::ensure_farmer_tls(&ssl_path)?;
        FarmerClient::new_with_constants(
            Arc::new(WsClientConfig {
                host: config.fullnode_ws_host.clone(),
                port: config.fullnode_ws_port,
                network_id,
                ssl_info: Some(ClientSSLConfig {
                    ssl_crt_path: ssl_path.join(PUBLIC_CRT).to_string_lossy().to_string(),
                    ssl_key_path: ssl_path.join(PUBLIC_KEY).to_string_lossy().to_string(),
                    ssl_ca_crt_path: ssl_path.join(CA_PUBLIC_CRT).to_string_lossy().to_string(),
                }),
                software_version: None,
                protocol_version: PROTOCOL_VERSION,
                additional_headers: None,
                server_port: 0,
                rate_limited: true,
            }),
            shared_state.clone(),
            client_run.clone(),
            30,
            config.constants()?,
        )
        .await
    }

    async fn attach_client_handlers(
        &self,
        client: Arc<RwLock<Option<FarmerClient<T>>>>,
        config: Arc<RwLock<Config<C>>>,
    ) -> Result<(), Error> {
        if let Some(c) = &*client.read().await {
            c.client.connection.write().await.clear().await;
        }
        let signage_handle_id = Uuid::new_v4();
        let request_signed_values_id = Uuid::new_v4();
        let harvester_id = load_client_id(config.clone()).await?;
        let proof_handle = O::load(
            self.shared_state.clone(),
            config.clone(),
            self.harvester.clone(),
            client.clone(),
        )
        .await?;
        let signature_handle = S::load(
            self.shared_state.clone(),
            config.clone(),
            self.harvester.clone(),
            client.clone(),
        )
        .await?;
        let config = config.read().await;
        let inner_client = client.clone();
        if let Some(c) = &*client.read().await {
            c.client
                .connection
                .write()
                .await
                .subscribe(
                    signage_handle_id,
                    Arc::new(ChiaMessageHandler::new(
                        NEW_SIGNAGE_POINT_FILTER.clone(),
                        Arc::new(NewSignagePointHandle {
                            id: signage_handle_id,
                            harvester_id,
                            shared_state: self.shared_state.clone(),
                            pool_state: self.shared_state.pool_states.clone(),
                            pool_client: self.pool_client.clone(),
                            signage_points: self.shared_state.signage_points.clone(),
                            cache_time: self.shared_state.cache_time.clone(),
                            harvester: self.harvester.clone(),
                            constants: config.constants()?,
                            client: inner_client.clone(),
                            config: self.config.clone(),
                            proof_handle,
                        }),
                    )),
                )
                .await;
            c.client
                .connection
                .write()
                .await
                .subscribe(
                    request_signed_values_id,
                    Arc::new(ChiaMessageHandler::new(
                        SIGNED_VALUES_FILTER.clone(),
                        Arc::new(RequestSignedValuesHandle {
                            id: request_signed_values_id,
                            shared_state: self.shared_state.clone(),
                            pool_client: self.pool_client.clone(),
                            harvester: self.harvester.clone(),
                            constants: config.constants()?,
                            client: inner_client.clone(),
                            config: self.config.clone(),
                            signature_handle,
                        }),
                    )),
                )
                .await;
        }
        Ok(())
    }
}
static NEW_SIGNAGE_POINT_FILTER: Lazy<Arc<ChiaMessageFilter>> = Lazy::new(|| {
    Arc::new(ChiaMessageFilter {
        msg_type: Some(ProtocolMessageTypes::NewSignagePoint),
        id: None,
        custom_fn: None,
    })
});
static SIGNED_VALUES_FILTER: Lazy<Arc<ChiaMessageFilter>> = Lazy::new(|| {
    Arc::new(ChiaMessageFilter {
        msg_type: Some(ProtocolMessageTypes::RequestSignedValues),
        id: None,
        custom_fn: None,
    })
});
