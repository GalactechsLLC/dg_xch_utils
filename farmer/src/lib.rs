#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::todo,
        clippy::unimplemented
    )
)]
use crate::farmer::Farmer;
use crate::farmer::config::{Config, load_keys};
use crate::farmer::protocols::harvester::new_proof_of_space::NewProofOfSpaceHandle;
use crate::farmer::protocols::harvester::respond_signatures::RespondSignaturesHandler;
use crate::harvesters::Harvester;
use crate::harvesters::UnifiedHarvester;
use dg_xch_clients::api::pool::DefaultPoolClient;
use dg_xch_core::protocols::farmer::FarmerSharedState;
use dg_xch_serialize::ChiaProtocolVersion;
use std::collections::HashMap;
use std::io::{Error, ErrorKind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};
use tokio::sync::RwLock;
use tokio::task::JoinSet;

pub mod farmer;
pub mod harvesters;
pub mod pool_management;
pub mod tasks;
pub mod utils;

pub const UPSTREAM_REVISION: &str = "270d44e1d885bd2d21ed658345042c954abfdcdc";
pub const PROTOCOL_VERSION: ChiaProtocolVersion = ChiaProtocolVersion::Chia0_0_37;
pub static HEADERS: LazyLock<HashMap<String, String>> =
    LazyLock::new(|| HashMap::from([("User-Agent".to_string(), version())]));

pub fn version() -> String {
    format!("dg_xch_farmer/{}", env!("CARGO_PKG_VERSION"))
}

pub type SignaturesHandler =
    RespondSignaturesHandler<DefaultPoolClient, (), UnifiedHarvester<()>, ()>;
pub type NewProofHandler =
    NewProofOfSpaceHandle<DefaultPoolClient, SignaturesHandler, (), UnifiedHarvester<()>, ()>;

pub struct FarmerService {
    pub state: Arc<FarmerSharedState<()>>,
    config: Arc<RwLock<Config<()>>>,
    pos2: Arc<harvesters::Pos2Harvester>,
    tasks: JoinSet<()>,
    monitor: Option<dg_xch_servers::observability::Monitor>,
}

impl FarmerService {
    pub async fn pool_settings(&self) -> Result<Vec<pool_management::PoolSettings>, Error> {
        #[cfg(feature = "falcon")]
        {
            let config = self.config.read().await;
            if let Some(keys) = config.falcon_keys()? {
                let client = config.pool_client()?;
                let mut settings = Vec::new();
                for pool in &config.falcon_pools {
                    settings.push(
                        pool_management::read_falcon_settings(
                            &client,
                            pool,
                            &keys.wallet,
                            &config.constants()?,
                        )
                        .await?,
                    );
                }
                return Ok(settings);
            }
        }
        let client = Arc::new(self.config.read().await.pool_client()?);
        let configs = self.config.read().await.pool_info.clone();
        let mut settings = Vec::new();
        for config in configs.iter().filter(|config| !config.pool_url.is_empty()) {
            let key = self
                .state
                .owner_public_keys_to_auth_secret_keys
                .get(&config.owner_public_key)
                .ok_or_else(|| Error::other("pool authentication key is unavailable"))?;
            settings.push(pool_management::read_settings(client.clone(), config, key).await?);
        }
        Ok(settings)
    }

    pub async fn update_pool_settings(
        &self,
        expected: &pool_management::PoolSettings,
        payout: &str,
        difficulty: u64,
    ) -> Result<pool_management::PoolSettings, Error> {
        let config = match &expected.identity {
            pool_management::PoolIdentity::Bls(config, _) => config,
            #[cfg(feature = "falcon")]
            pool_management::PoolIdentity::Falcon(pool) => {
                let config = self.config.read().await;
                if !config.falcon_pools.contains(pool) {
                    return Err(Error::other("pool is no longer configured on this farmer"));
                }
                let keys = config
                    .falcon_keys()?
                    .ok_or_else(|| Error::other("Falcon keys unavailable"))?;
                return pool_management::update_falcon_settings(
                    &config.pool_client()?,
                    expected,
                    &keys.wallet,
                    &config.constants()?,
                    payout,
                    difficulty,
                )
                .await;
            }
        };
        if !self.config.read().await.pool_info.contains(config) {
            return Err(Error::other("pool is no longer configured on this farmer"));
        }
        let authentication_key = self
            .state
            .owner_public_keys_to_auth_secret_keys
            .get(&config.owner_public_key)
            .ok_or_else(|| Error::other("pool authentication key is unavailable"))?;
        let owner_key = self
            .state
            .owner_secret_keys
            .get(&config.owner_public_key)
            .ok_or_else(|| Error::other("pool owner key is unavailable"))?;
        pool_management::update_settings(
            Arc::new(self.config.read().await.pool_client()?),
            expected,
            authentication_key,
            owner_key,
            payout,
            difficulty,
        )
        .await
    }

    pub fn pos2_status(&self) -> harvesters::Pos2Status {
        self.pos2.status()
    }

    pub fn failure(&mut self) -> Option<String> {
        if self
            .monitor
            .as_ref()
            .is_some_and(|monitor| monitor.finished())
        {
            self.state.signal.store(false, Ordering::Release);
            return Some("farmer monitoring server stopped unexpectedly".into());
        }
        match self.tasks.try_join_next() {
            Some(Ok(())) => Some("farmer background task stopped unexpectedly".into()),
            Some(Err(error)) => Some(format!("farmer background task failed: {error}")),
            None => None,
        }
    }

    pub async fn start(mut config: Config<()>) -> Result<Self, Error> {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        if !config.is_ready() {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "farmer configuration is incomplete or has an unknown network",
            ));
        }
        config.validate_keys()?;
        let monitoring = config.metrics.clone().filter(|metrics| metrics.enabled);
        if monitoring.as_ref().is_some_and(|metrics| metrics.port == 0) {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "farmer metrics port must be nonzero",
            ));
        }
        let ssl_root = utils::get_ssl_root_path(&config)?;
        utils::ensure_farmer_tls(&ssl_root)?;
        config.ssl_root_path = Some(ssl_root.to_string_lossy().into_owned());
        let (farmer_keys, owner_keys, auth_keys, pool_keys) = load_keys(&config).await;
        let state = Arc::new(FarmerSharedState {
            farmer_private_keys: Arc::new(farmer_keys),
            owner_secret_keys: Arc::new(owner_keys),
            owner_public_keys_to_auth_secret_keys: Arc::new(auth_keys),
            pool_public_keys: Arc::new(pool_keys),
            data: Arc::new(()),
            signal: Arc::new(AtomicBool::new(true)),
            ..Default::default()
        });
        if monitoring.is_some() {
            *state.metrics.write().await =
                Some(dg_xch_core::protocols::farmer::FarmerMetrics::new(
                    prometheus::Registry::new(),
                    uuid::Uuid::new_v4(),
                ));
        }
        let config = Arc::new(RwLock::new(config));
        let harvester = <UnifiedHarvester<()> as Harvester<(), UnifiedHarvester<()>, ()>>::load(
            state.clone(),
            config.clone(),
        )
        .await?;
        let farmer = Farmer::<DefaultPoolClient, NewProofHandler, SignaturesHandler>::new(
            state.clone(),
            Arc::new(config.read().await.pool_client()?),
            harvester.clone(),
            config.clone(),
        )
        .await?;
        let mut tasks = JoinSet::new();
        tasks.spawn(farmer.run());
        tasks.spawn(tasks::pool_state_updater::pool_updater(
            state.clone(),
            config.clone(),
        ));
        tasks.spawn(tasks::blockchain_state_updater::update_blockchain(
            state.clone(),
            config.clone(),
        ));
        let monitor = monitoring.map(|monitoring| {
            let snapshot_state = state.clone();
            let pos2 = harvester.pos2.clone();
            let snapshot: dg_xch_servers::observability::SnapshotFn = Arc::new(move || {
                let state = snapshot_state.clone();
                let pos2 = pos2.clone();
                Box::pin(async move {
                    let healthy = state.signal.load(Ordering::Acquire);
                    let status = pos2.status();
                    let mut text = dg_xch_servers::observability::gauges(
                        "chia_farmer",
                        &[
                            ("running", "Farmer runtime is active", f64::from(healthy)),
                            (
                                "pos2_loaded_plots",
                                "Loaded PoS2 plots",
                                status.loaded_plots as f64,
                            ),
                            (
                                "pos2_filter_passes",
                                "PoS2 filter passes since startup",
                                status.filter_passes as f64,
                            ),
                            (
                                "pos2_eligible_qualities",
                                "PoS2 eligible qualities since startup",
                                status.eligible_qualities as f64,
                            ),
                            (
                                "pos2_recovered_proofs",
                                "PoS2 recovered proofs since startup",
                                status.recovered_proofs as f64,
                            ),
                            (
                                "pos2_recovery_errors",
                                "PoS2 recovery errors since startup",
                                status.recovery_errors as f64,
                            ),
                            (
                                "pos2_deadlines_exceeded",
                                "PoS2 recovery deadlines exceeded",
                                status.deadlines_exceeded as f64,
                            ),
                        ],
                    )?;
                    if let Some(metrics) = state.metrics.read().await.as_ref() {
                        metrics.uptime.set(metrics.start_time.elapsed().as_secs());
                        text.push_str(&dg_xch_servers::observability::encode(
                            &*metrics.registry.read().await,
                        )?);
                    }
                    Ok(dg_xch_servers::observability::Snapshot {
                        healthy,
                        message: if healthy {
                            "farmer runtime active"
                        } else {
                            "farmer stopped"
                        }
                        .into(),
                        metrics: text,
                    })
                })
            });
            dg_xch_servers::observability::Monitor::start(
                std::net::SocketAddr::from(([0, 0, 0, 0], monitoring.port)),
                snapshot,
            )
        });
        Ok(Self {
            state,
            config,
            tasks,
            monitor,
            pos2: harvester.pos2.clone(),
        })
    }

    pub async fn stop(mut self) {
        self.state.signal.store(false, Ordering::Release);
        self.tasks.shutdown().await;
        if let Some(monitor) = self.monitor.take()
            && let Err(error) = monitor.stop().await
        {
            log::error!("farmer monitoring shutdown: {error}");
        }
    }
}

impl Drop for FarmerService {
    fn drop(&mut self) {
        self.state.signal.store(false, Ordering::Release);
        self.tasks.abort_all();
    }
}
