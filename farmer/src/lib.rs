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
use crate::harvesters::druid_garden::DruidGardenHarvester;
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
    RespondSignaturesHandler<DefaultPoolClient, (), DruidGardenHarvester<()>, ()>;
pub type NewProofHandler =
    NewProofOfSpaceHandle<DefaultPoolClient, SignaturesHandler, (), DruidGardenHarvester<()>, ()>;

pub struct FarmerService {
    pub state: Arc<FarmerSharedState<()>>,
    pos2: Arc<harvesters::Pos2Harvester>,
    tasks: JoinSet<()>,
}

impl FarmerService {
    pub fn pos2_status(&self) -> harvesters::Pos2Status {
        self.pos2.status()
    }

    pub fn failure(&mut self) -> Option<String> {
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
        let config = Arc::new(RwLock::new(config));
        let harvester =
            <DruidGardenHarvester<()> as Harvester<(), DruidGardenHarvester<()>, ()>>::load(
                state.clone(),
                config.clone(),
            )
            .await?;
        let farmer = Farmer::<DefaultPoolClient, NewProofHandler, SignaturesHandler>::new(
            state.clone(),
            Arc::new(DefaultPoolClient::new()?),
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
            config,
        ));
        Ok(Self {
            state,
            tasks,
            pos2: harvester.pos2.clone(),
        })
    }

    pub async fn stop(mut self) {
        self.state.signal.store(false, Ordering::Release);
        self.tasks.shutdown().await;
    }
}

impl Drop for FarmerService {
    fn drop(&mut self) {
        self.state.signal.store(false, Ordering::Release);
        self.tasks.abort_all();
    }
}
