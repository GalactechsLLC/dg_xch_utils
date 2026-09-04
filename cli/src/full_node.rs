use clap::Args;
use dg_full_node::Config;
use dg_logger::DruidGardenLogger;
use dg_xch_p2p::P2pSettings;
use std::io::Error;
use std::sync::Arc;
use std::time::Duration;

#[derive(Args, Debug)]
pub struct FullNodeArgs {
    #[arg(long, default_value = "0.0.0.0:8444")]
    listen: String,
    /// Deprecated: Portfu serves peers and RPC on `--listen`; if supplied this must match it.
    #[arg(long)]
    rpc: Option<String>,
    #[arg(long = "rpc-tls", default_value = "private-ca")]
    rpc_tls: String,
    /// Directory containing the private RPC CA when `--rpc-tls private-ca` is used.
    #[arg(long = "ssl-dir", default_value = "ssl")]
    ssl_dir: String,
    #[arg(long)]
    introducer: Option<String>,
    #[arg(long = "peer")]
    peer: Vec<String>,
    /// External WAN address advertised for peer gossip behind NAT.
    #[arg(long)]
    advertise: Option<String>,
    /// Storage URL: `sqlite://<path>`, `postgres://...`, or `mmap://<directory>`.
    #[arg(long, default_value = "sqlite:///data/chain.db")]
    db: String,
    /// Network id selecting consensus constants.
    #[arg(long, default_value = "mainnet")]
    network: String,
    /// Directory used to capture sync data for offline replay and profiling.
    #[arg(long)]
    capture_dir: Option<String>,
    /// Validate historical blocks from genesis instead of using weight-proof fast sync.
    #[arg(long, default_value_t = false)]
    genesis_sync: bool,
    /// Anchor validation at this height; zero disables the explicit anchor.
    #[arg(long, default_value_t = 0)]
    sync_from: u32,
    #[arg(long, default_value_t = false)]
    uncompact: bool,
    /// Resident memory budget in MiB for sync-window block prefetching.
    #[arg(long)]
    prefetch_memory_mb: Option<u64>,
    /// Maximum aggregate outstanding block-range requests.
    #[arg(long)]
    prefetch_max_inflight: Option<usize>,
    /// Outbound connections to maintain.
    #[arg(long)]
    target_outbound: Option<usize>,
    /// Total inbound and outbound peers to accept.
    #[arg(long)]
    target_peer_count: Option<usize>,
    #[arg(long, default_value_t = 1_000)]
    host_pool_capacity: usize,
    #[arg(long, default_value_t = 5)]
    address_lower: usize,
    #[arg(long, default_value_t = 10)]
    address_upper: usize,
    #[arg(long, default_value_t = 30)]
    connect_timeout_secs: u64,
    #[arg(long, default_value_t = 15)]
    handshake_timeout_secs: u64,
    #[arg(long, default_value_t = 1)]
    retry_timeout_secs: u64,
    #[arg(long, default_value_t = 120)]
    heartbeat_secs: u64,
    #[arg(long, default_value_t = 30)]
    pong_deadline_secs: u64,
    #[arg(long, default_value_t = 6_000)]
    recent_peer_threshold_secs: u64,
    /// Lower bound for randomized reconnect backoff, from 0 through 1.
    #[arg(long, default_value_t = 0.5)]
    jitter_floor: f64,
    #[arg(long = "trusted-peer")]
    trusted_peer: Vec<String>,
    /// Trusted IPv4 or IPv6 CIDR, repeatable.
    #[arg(long = "trusted-cidr")]
    trusted_cidr: Vec<String>,
    #[arg(long = "debug-endpoints", default_value_t = false)]
    debug_endpoints: bool,
}

impl FullNodeArgs {
    fn config(self) -> Result<Config, Error> {
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
            &self.db,
            &self.network,
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
        if config.rpc != config.listen {
            return Err(Error::other(format!(
                "--rpc no longer opens a second listener; omit it or set it to --listen ({})",
                config.listen
            )));
        }
        config.rpc_tls =
            dg_full_node::RpcTlsMode::parse(&self.rpc_tls, &self.ssl_dir).map_err(Error::other)?;
        config.debug_endpoints = self.debug_endpoints;
        Ok(config)
    }
}

pub async fn run(args: FullNodeArgs, logger: Arc<DruidGardenLogger>) -> Result<(), Error> {
    dg_full_node::server::run(args.config()?, logger)
        .await
        .map_err(|error| Error::other(error.to_string()))
}
