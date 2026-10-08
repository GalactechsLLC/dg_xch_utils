use clap::Args;
use dg_full_node::Config;
use dg_logger::DruidGardenLogger;
use dg_xch_p2p::P2pSettings;
use std::io::Error;
use std::sync::Arc;
use std::time::Duration;

#[derive(Args, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FullNodeArgs {
    /// YAML or JSON node configuration; alternatively set DGX_FULL_NODE_CONFIG to its contents.
    #[arg(long = "config")]
    #[serde(skip)]
    config_file: Option<std::path::PathBuf>,
    #[arg(skip)]
    #[serde(skip)]
    cli_overrides: std::collections::HashSet<String>,
    #[arg(skip)]
    #[serde(skip)]
    document_loaded: bool,
    #[arg(env = "DGX_FULL_NODE_LISTEN", long, default_value = "0.0.0.0:8444")]
    listen: String,
    /// Deprecated: Portfu serves peers and RPC on `--listen`; if supplied this must match it.
    #[arg(env = "DGX_FULL_NODE_RPC", long)]
    rpc: Option<String>,
    #[arg(
        env = "DGX_FULL_NODE_RPC_TLS",
        long = "rpc-tls",
        default_value = "private-ca"
    )]
    rpc_tls: String,
    /// Directory containing the private RPC CA when `--rpc-tls private-ca` is used.
    #[arg(env = "DGX_FULL_NODE_SSL_DIR", long = "ssl-dir")]
    ssl_dir: Option<String>,
    #[arg(env = "DGX_FULL_NODE_INTRODUCER", long)]
    introducer: Option<String>,
    #[arg(env = "DGX_FULL_NODE_PEER", value_delimiter = ',', long = "peer")]
    peer: Vec<String>,
    /// External WAN address advertised for peer gossip behind NAT.
    #[arg(env = "DGX_FULL_NODE_ADVERTISE", long)]
    advertise: Option<String>,
    /// Storage URL: `sqlite://<path>`, `postgres://...`, or `mmap://<directory>`.
    #[arg(env = "DGX_FULL_NODE_DB", long)]
    db: Option<String>,
    /// Network id selecting consensus constants.
    #[arg(env = "DGX_FULL_NODE_NETWORK", long)]
    network: Option<String>,
    #[arg(env = "DGX_FULL_NODE_CHAIN_CONFIG", long)]
    chain_config: Option<std::path::PathBuf>,
    #[arg(env = "DGX_FULL_NODE_PRINT_CHAIN_INFO", long)]
    print_chain_info: bool,
    /// Directory used to capture sync data for offline replay and profiling.
    #[arg(env = "DGX_FULL_NODE_CAPTURE_DIR", long)]
    capture_dir: Option<String>,
    /// Skip weight-proof fork anchoring during catch-up; empty databases always start at genesis.
    #[arg(env = "DGX_FULL_NODE_GENESIS_SYNC", long, default_value_t = false)]
    genesis_sync: bool,
    /// Anchor validation at this height; zero disables the explicit anchor.
    #[arg(env = "DGX_FULL_NODE_SYNC_FROM", long, default_value_t = 0)]
    sync_from: u32,
    #[arg(env = "DGX_FULL_NODE_UNCOMPACT", long, default_value_t = false)]
    uncompact: bool,
    /// Resident memory budget in MiB for sync-window block prefetching.
    #[arg(env = "DGX_FULL_NODE_PREFETCH_MEMORY_MB", long)]
    prefetch_memory_mb: Option<u64>,
    /// Maximum aggregate outstanding block-range requests.
    #[arg(env = "DGX_FULL_NODE_PREFETCH_MAX_INFLIGHT", long)]
    prefetch_max_inflight: Option<usize>,
    #[arg(
        env = "DGX_FULL_NODE_COMPUTE_WORKERS",
        long,
        help = "Shared bulk CPU workers; defaults to available logical CPUs minus two"
    )]
    compute_workers: Option<usize>,
    #[arg(
        env = "DGX_FULL_NODE_VALIDATION_WINDOW_BLOCKS",
        long,
        help = "Maximum blocks per bulk validation window; default is CPU-derived"
    )]
    validation_window_blocks: Option<u32>,
    #[arg(
        env = "DGX_FULL_NODE_VALIDATION_WINDOW_MB",
        long,
        default_value_t = 128,
        help = "Estimated resident MiB per validation window; admits one oversized block"
    )]
    validation_window_mb: u64,
    #[arg(
        env = "DGX_FULL_NODE_CONFIRM_TRANSACTION_BLOCKS",
        long,
        help = "Maximum staged blocks per bulk confirmation transaction; defaults to whole window"
    )]
    confirm_transaction_blocks: Option<usize>,
    #[arg(
        env = "DGX_FULL_NODE_CONFIRM_TRANSACTION_COIN_CHANGES",
        long,
        help = "Maximum coin additions, spends and hints per bulk confirmation part; one oversized block is admitted"
    )]
    confirm_transaction_coin_changes: Option<usize>,
    #[arg(
        env = "DGX_FULL_NODE_CONFIRM_TRANSACTION_COIN_MB",
        long,
        help = "Maximum estimated coin payload MiB per bulk confirmation part; not an RSS or WAL limit"
    )]
    confirm_transaction_coin_mb: Option<u64>,
    #[arg(
        env = "DGX_FULL_NODE_SQLITE_WRITER_CACHE_MB",
        long,
        help = "SQLite bulk writer cache budget in MiB; default 256, near-tip remains 64"
    )]
    sqlite_writer_cache_mb: Option<u64>,
    #[arg(
        env = "DGX_FULL_NODE_COALESCE_COIN_WRITES",
        long,
        default_value_t = false,
        help = "Coalesce coin writes within each bulk confirmation transaction"
    )]
    coalesce_coin_writes: bool,
    /// Outbound connections to maintain.
    #[arg(env = "DGX_FULL_NODE_TARGET_OUTBOUND", long)]
    target_outbound: Option<usize>,
    /// Total inbound and outbound peers to accept.
    #[arg(env = "DGX_FULL_NODE_TARGET_PEER_COUNT", long)]
    target_peer_count: Option<usize>,
    #[arg(
        env = "DGX_FULL_NODE_HOST_POOL_CAPACITY",
        long,
        default_value_t = 1_000
    )]
    host_pool_capacity: usize,
    #[arg(env = "DGX_FULL_NODE_ADDRESS_LOWER", long, default_value_t = 5)]
    address_lower: usize,
    #[arg(env = "DGX_FULL_NODE_ADDRESS_UPPER", long, default_value_t = 10)]
    address_upper: usize,
    #[arg(env = "DGX_FULL_NODE_CONNECT_TIMEOUT_SECS", long, default_value_t = 30)]
    connect_timeout_secs: u64,
    #[arg(
        env = "DGX_FULL_NODE_HANDSHAKE_TIMEOUT_SECS",
        long,
        default_value_t = 15
    )]
    handshake_timeout_secs: u64,
    #[arg(env = "DGX_FULL_NODE_RETRY_TIMEOUT_SECS", long, default_value_t = 1)]
    retry_timeout_secs: u64,
    #[arg(env = "DGX_FULL_NODE_HEARTBEAT_SECS", long, default_value_t = 120)]
    heartbeat_secs: u64,
    #[arg(env = "DGX_FULL_NODE_PONG_DEADLINE_SECS", long, default_value_t = 30)]
    pong_deadline_secs: u64,
    #[arg(
        env = "DGX_FULL_NODE_RECENT_PEER_THRESHOLD_SECS",
        long,
        default_value_t = 6_000
    )]
    recent_peer_threshold_secs: u64,
    /// Lower bound for randomized reconnect backoff, from 0 through 1.
    #[arg(env = "DGX_FULL_NODE_JITTER_FLOOR", long, default_value_t = 0.5)]
    jitter_floor: f64,
    #[arg(
        env = "DGX_FULL_NODE_TRUSTED_PEER",
        value_delimiter = ',',
        long = "trusted-peer"
    )]
    trusted_peer: Vec<String>,
    /// Trusted IPv4 or IPv6 CIDR, repeatable.
    #[arg(
        env = "DGX_FULL_NODE_TRUSTED_CIDR",
        value_delimiter = ',',
        long = "trusted-cidr"
    )]
    trusted_cidr: Vec<String>,
    #[arg(
        env = "DGX_FULL_NODE_DEBUG_ENDPOINTS",
        long = "debug-endpoints",
        default_value_t = false
    )]
    debug_endpoints: bool,
}

impl FullNodeArgs {
    pub(crate) fn record_overrides(&mut self, matches: &clap::ArgMatches) {
        self.cli_overrides = matches
            .ids()
            .filter(|id| {
                matches.value_source(id.as_str()) == Some(clap::parser::ValueSource::CommandLine)
            })
            .map(|id| id.as_str().to_owned())
            .collect();
    }

    pub(crate) fn load_document(&mut self) -> Result<(), Error> {
        if self.config_file.is_none() && std::env::var_os("DGX_FULL_NODE_CONFIG").is_none() {
            return Ok(());
        }
        let document: serde_json::Value = crate::services::config::load(
            "FULL_NODE",
            self.config_file.as_deref(),
            crate::services::config::Format::Yaml,
            1024 * 1024,
        )?;
        self.apply_document(document)
    }

    fn apply_document(&mut self, document: serde_json::Value) -> Result<(), Error> {
        let document = document
            .as_object()
            .ok_or_else(|| Error::other("full-node configuration must be a YAML or JSON object"))?;
        let mut settings = serde_json::to_value(&*self).map_err(Error::other)?;
        let values = settings
            .as_object_mut()
            .ok_or_else(|| Error::other("invalid full-node settings"))?;
        for (key, value) in document {
            if !values.contains_key(key) {
                return Err(Error::other(format!(
                    "unknown full-node configuration field: {key}"
                )));
            }
            if !self.cli_overrides.contains(key) {
                values.insert(key.clone(), value.clone());
            }
        }
        let mut resolved: Self = serde_json::from_value(settings)
            .map_err(|_| Error::other("invalid full-node configuration field type or value"))?;
        resolved.config_file = self.config_file.clone();
        resolved.cli_overrides = self.cli_overrides.clone();
        resolved.document_loaded = true;
        *self = resolved;
        Ok(())
    }

    pub(crate) fn apply_profile(&mut self, root: &std::path::Path) -> Result<(), Error> {
        if self.print_chain_info {
            return Ok(());
        }
        if self.document_loaded && !root.join("dgx.json").try_exists()? {
            self.db
                .get_or_insert_with(|| "sqlite:///data/chain.db".to_owned());
            self.ssl_dir
                .get_or_insert_with(|| root.join("ssl").display().to_string());
        }
        // Explicit storage and TLS paths allow deployments with mounted secrets and no app profile.
        if root.join("dgx.json").try_exists()? || self.db.is_none() || self.ssl_dir.is_none() {
            let profile = dg_xch_servers::app_config::AppConfig::load(root)?;
            self.ssl_dir
                .get_or_insert_with(|| root.join("ssl").display().to_string());
            self.db.get_or_insert_with(|| {
                format!("sqlite://{}", profile.data_dir.join("chain.db").display())
            });
        }
        if self.introducer.is_none()
            && self.peer.is_empty()
            && self.chain_config.is_none()
            && self.network.as_deref().unwrap_or("mainnet") == "mainnet"
        {
            self.introducer = Some("introducer.chia.net:8444".into());
        }
        Ok(())
    }

    pub(crate) fn inherit_network(&mut self, root_network: Option<&str>) -> Result<(), Error> {
        crate::inherit_network(&mut self.network, root_network)
    }

    fn config(self) -> Result<Config, Error> {
        let selection =
            crate::chain::selection(self.network.as_deref(), self.chain_config.as_deref())?;
        let resolved = selection.resolve().map_err(Error::other)?;
        let chain_definition = match selection {
            dg_xch_core::consensus::chain_definition::ChainSelection::Custom(definition) => {
                Some(definition)
            }
            _ => None,
        };
        let network = resolved.network_id.as_str();
        let defaults = P2pSettings::default();
        let p2p = P2pSettings {
            target_outbound: self.target_outbound.unwrap_or(defaults.target_outbound),
            target_peer_count: self.target_peer_count.unwrap_or(defaults.target_peer_count),
            host_pool_capacity: self.host_pool_capacity,
            address_lower: self.address_lower,
            address_upper: self.address_upper,
            connect_timeout: Duration::from_secs(self.connect_timeout_secs),
            handshake_timeout: Duration::from_secs(self.handshake_timeout_secs),
            retry_timeout: Duration::from_secs(self.retry_timeout_secs),
            heartbeat: Duration::from_secs(self.heartbeat_secs),
            pong_deadline: Duration::from_secs(self.pong_deadline_secs),
            recent_peer_threshold: Duration::from_secs(self.recent_peer_threshold_secs),
            jitter_floor: self.jitter_floor,
        };
        let rpc = self.rpc.as_deref().unwrap_or(&self.listen);
        let mut config = Config::build(
            &self.listen,
            rpc,
            self.introducer.as_deref(),
            &self.peer,
            self.advertise.as_deref(),
            self.db.as_deref().unwrap_or("sqlite:///data/chain.db"),
            network,
            self.capture_dir.as_deref(),
            self.genesis_sync,
            self.sync_from,
            self.uncompact,
            self.prefetch_memory_mb,
            self.prefetch_max_inflight,
            p2p,
            &self.trusted_peer,
            &self.trusted_cidr,
        )
        .map_err(Error::other)?;
        config.chain_definition = chain_definition;
        config.consensus_constants().map_err(Error::other)?;
        if config.rpc != config.listen {
            return Err(Error::other(format!(
                "--rpc no longer opens a second listener; omit it or set it to --listen ({})",
                config.listen
            )));
        }
        config.rpc_tls = dg_full_node::RpcTlsMode::parse(
            &self.rpc_tls,
            self.ssl_dir.as_deref().unwrap_or("ssl"),
        )
        .map_err(Error::other)?;
        config.debug_endpoints = self.debug_endpoints;
        config.performance.compute_workers = self.compute_workers;
        config.performance.validation_window_blocks = self.validation_window_blocks;
        config.performance.validation_window_mb = self.validation_window_mb;
        config.performance.confirm_transaction_blocks = self.confirm_transaction_blocks;
        config.performance.confirm_transaction_coin_changes = self.confirm_transaction_coin_changes;
        config.performance.confirm_transaction_coin_mb = self.confirm_transaction_coin_mb;
        config.performance.sqlite_writer_cache_mb = self.sqlite_writer_cache_mb;
        config.performance.coalesce_coin_writes = self.coalesce_coin_writes;
        config.performance.validate().map_err(Error::other)?;
        Ok(config)
    }
}

pub async fn run(args: FullNodeArgs, logger: Arc<DruidGardenLogger>) -> Result<(), Error> {
    let print_chain_info = args.print_chain_info;
    let config = args.config()?;
    if print_chain_info {
        #[derive(serde::Serialize)]
        struct ChainInfo {
            network_id: String,
            handshake_network_id: String,
            constants: dg_xch_core::consensus::constants::ConsensusConstants,
        }
        let info = ChainInfo {
            network_id: config.network_id.clone(),
            handshake_network_id: config.handshake_network_id().map_err(Error::other)?,
            constants: config.consensus_constants().map_err(Error::other)?,
        };
        println!(
            "{}",
            serde_json::to_string_pretty(&info).map_err(Error::other)?
        );
        return Ok(());
    }
    dg_full_node::server::run(config, logger)
        .await
        .map_err(|error| Error::other(error.to_string()))
}

#[cfg(test)]
mod network_tests {
    use super::*;
    use crate::cli::{Cli, RootCommands};
    use clap::Parser;

    #[test]
    fn initialized_paths_supply_defaults_without_overriding_explicit_flags() {
        let directory = tempfile::tempdir().unwrap();
        let profile = dg_xch_servers::app_config::AppConfig {
            version: 1,
            data_dir: directory.path().join("data"),
            plot_directories: vec![directory.path().join("plots")],
        };
        profile.save_new(directory.path()).unwrap();
        let cli = Cli::try_parse_from(["dgx", "full-node"]).unwrap();
        let RootCommands::FullNode(mut args) = cli.action else {
            panic!("wrong command")
        };
        args.apply_profile(directory.path()).unwrap();
        assert_eq!(
            args.db,
            Some(format!(
                "sqlite://{}",
                profile.data_dir.join("chain.db").display()
            ))
        );
        assert_eq!(
            args.ssl_dir,
            Some(directory.path().join("ssl").display().to_string())
        );
        assert_eq!(args.introducer.as_deref(), Some("introducer.chia.net:8444"));
        let cli = Cli::try_parse_from([
            "dgx",
            "full-node",
            "--db",
            "sqlite://explicit.db",
            "--ssl-dir",
            "explicit-ssl",
            "--peer",
            "localhost:9000",
        ])
        .unwrap();
        let RootCommands::FullNode(mut args) = cli.action else {
            panic!("wrong command")
        };
        args.apply_profile(directory.path()).unwrap();
        assert_eq!(args.db.as_deref(), Some("sqlite://explicit.db"));
        assert_eq!(args.ssl_dir.as_deref(), Some("explicit-ssl"));
        assert!(args.introducer.is_none());
    }

    #[test]
    fn explicit_storage_and_tls_do_not_require_a_profile_and_keep_peer_discovery() {
        let directory = tempfile::tempdir().unwrap();
        let cli = Cli::try_parse_from([
            "dgx",
            "full-node",
            "--db",
            "sqlite:///data/chain.db",
            "--ssl-dir",
            "/secrets/ssl",
        ])
        .unwrap();
        let RootCommands::FullNode(mut args) = cli.action else {
            panic!("wrong command")
        };
        args.apply_profile(directory.path()).unwrap();
        assert_eq!(args.db.as_deref(), Some("sqlite:///data/chain.db"));
        assert_eq!(args.ssl_dir.as_deref(), Some("/secrets/ssl"));
        assert_eq!(args.introducer.as_deref(), Some("introducer.chia.net:8444"));
        assert!(!directory.path().join("dgx.json").exists());
    }

    fn config(arguments: &[&str]) -> Result<Config, Error> {
        let mut cli = Cli::try_parse_from(arguments).map_err(Error::other)?;
        crate::apply_network(&mut cli)?;
        match cli.action {
            RootCommands::FullNode(args) => args.config(),
            _ => Err(Error::other("expected full-node command")),
        }
    }

    #[test]
    fn root_network_reaches_the_full_node_without_changing_chia_defaults() {
        let defaults = config(&["dgx", "full-node"]).unwrap();
        assert_eq!(defaults.network_id, "mainnet");
        assert_eq!(defaults.handshake_network_id().unwrap(), "mainnet");
        assert_eq!(
            defaults.consensus_constants().unwrap(),
            dg_xch_core::consensus::constants::MAINNET
        );
        let inherited = config(&["dgx", "--network", "testnet11", "full-node"]).unwrap();
        assert_eq!(inherited.network_id, "testnet11");
        assert_eq!(inherited.handshake_network_id().unwrap(), "testnet11");
        let local = config(&["dgx", "full-node", "--network", "dgx"]).unwrap();
        assert!(local.allows_chain_bootstrap());
        assert_eq!(local.network_id, "dgx");
    }

    #[test]
    fn conflicting_and_unknown_full_node_networks_fail_closed() {
        assert!(
            config(&[
                "dgx",
                "--network",
                "mainnet",
                "full-node",
                "--network",
                "testnet11"
            ])
            .is_err()
        );
        assert!(config(&["dgx", "--network", "not-a-network", "full-node"]).is_err());
        let matching = config(&[
            "dgx",
            "--network",
            "testnet11",
            "full-node",
            "--network",
            "testnet11",
        ])
        .unwrap();
        assert_eq!(matching.network_id, "testnet11");
    }
}
