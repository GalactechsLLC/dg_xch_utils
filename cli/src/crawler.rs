use clap::Args;
use dg_xch_crawler::config::Config;
use std::{io::Error, net::SocketAddr, path::PathBuf};

#[derive(Args, Debug)]
pub struct CrawlerArgs {
    /// Complete JSON configuration, also supported through DGX_CRAWLER_CONFIG.
    #[arg(long)]
    config: Option<PathBuf>,
    /// Per-probe JSON logging: off, errors (default), or all.
    #[arg(long)]
    probe_log: Option<dg_xch_crawler::config::ProbeLog>,
    /// Shared PostgreSQL catalog; alternatively use DGX_CRAWLER_DATABASE_URL.
    #[arg(long)]
    database_url: Option<String>,
    /// JSON array of up to three {name,url,ssl} RPC sources (or DGX_CRAWLER_NODES).
    #[arg(long)]
    nodes: Option<String>,
    /// Local GeoLite2 Country MMDB path.
    #[arg(long)]
    country_database: Option<PathBuf>,
    /// Local GeoLite2 ASN MMDB path.
    #[arg(long)]
    asn_database: Option<PathBuf>,
    #[arg(long)]
    listen: Option<SocketAddr>,
    /// Chia network ID; alternatively supply the root --network flag.
    #[arg(long)]
    network: Option<String>,
    /// Repeat to supply bootstrap nodes (IP or hostname with port).
    #[arg(long)]
    seed: Vec<String>,
    /// Enable IPv6 seed addresses and probes (disabled by default).
    #[arg(long, action = clap::ArgAction::Set)]
    enable_ipv6: Option<bool>,
    /// Expected network port (8444 for mainnet, 58444 for testnet11).
    #[arg(long)]
    peer_port: Option<u16>,
    /// Minimum full-node peak height for a positive reliability observation.
    #[arg(long)]
    minimum_height: Option<u32>,
    /// Separate NewPeak wait deadline in milliseconds (default: 60000).
    #[arg(long)]
    peak_wait_ms: Option<u64>,
    #[arg(long)]
    concurrency: Option<usize>,
    /// Minimum spacing between all outbound connection attempts.
    #[arg(long)]
    interval_ms: Option<u64>,
    /// Successful peers are revisited after this delay (plus jitter).
    #[arg(long)]
    revisit_seconds: Option<u64>,
    /// TCP connection deadline in seconds, capped by the connection timeout.
    #[arg(long)]
    tcp_connect_timeout_seconds: Option<u64>,
    /// Deadline for TCP, TLS, WebSocket, and Chia handshake; discovery and NewPeak have separate deadlines.
    #[arg(long)]
    timeout_seconds: Option<u64>,
    /// Recent means a successful Chia handshake within this window.
    #[arg(long)]
    recent_seconds: Option<u64>,
    /// Remove endpoints without new gossip or connections for this many days.
    #[arg(long)]
    retention_days: Option<u64>,
    /// Permit local/private addresses for controlled testing.
    #[arg(long)]
    allow_private_addresses: bool,
}
impl CrawlerArgs {
    pub fn inherit_network(&mut self, root: Option<&str>) -> Result<(), Error> {
        crate::inherit_network(&mut self.network, root)
    }
    pub fn config(&self) -> Result<Config, Error> {
        let mut config: Config =
            if self.config.is_some() || std::env::var_os("DGX_CRAWLER_CONFIG").is_some() {
                crate::services::config::load(
                    "CRAWLER",
                    self.config.as_deref(),
                    crate::services::config::Format::Json,
                    1024 * 1024,
                )?
            } else {
                Config::default()
            };
        if let Some(network) = &self.network {
            config.network = network.clone();
        }
        if let Some(url) = &self.database_url {
            config.database_url = Some(url.clone());
        } else if let Ok(url) = std::env::var("DGX_CRAWLER_DATABASE_URL") {
            config.database_url = Some(url);
        }
        if let Some(value) = self.listen {
            config.listen = value;
        }
        if !self.seed.is_empty() {
            config.seed = self.seed.clone();
        }
        if let Some(value) = self.enable_ipv6 {
            config.enable_ipv6 = value;
        }
        if let Some(value) = self.peer_port {
            config.peer_port = Some(value);
        }
        if let Some(value) = self.minimum_height {
            config.minimum_height = Some(value);
        }
        if let Some(value) = self.peak_wait_ms {
            config.peak_wait_ms = value;
        }
        if let Some(value) = self.concurrency {
            config.concurrency = value;
        }
        if let Some(value) = self.interval_ms {
            config.interval_ms = value;
        }
        if let Some(value) = self.revisit_seconds {
            config.revisit_seconds = value;
        }
        if let Some(value) = self.tcp_connect_timeout_seconds {
            config.tcp_connect_timeout_seconds = value;
        }
        if let Some(value) = self.timeout_seconds {
            config.timeout_seconds = value;
        }
        if let Some(value) = self.recent_seconds {
            config.recent_seconds = value;
        }
        if let Some(value) = self.retention_days {
            config.retention_days = value;
        }
        if self.allow_private_addresses {
            config.allow_private_addresses = true;
        }
        if let Some(value) = self.probe_log {
            config.probe_log = value;
        }
        if let Some(nodes) = self
            .nodes
            .clone()
            .or_else(|| std::env::var("DGX_CRAWLER_NODES").ok())
        {
            config.telemetry.nodes = serde_json::from_str(&nodes)
                .map_err(|_| Error::other("invalid crawler node configuration JSON"))?;
        }
        if let Some(path) = self
            .country_database
            .clone()
            .or_else(|| std::env::var_os("DGX_CRAWLER_COUNTRY_DATABASE").map(PathBuf::from))
        {
            config.geography.country_database = Some(path);
        }
        if let Some(path) = self
            .asn_database
            .clone()
            .or_else(|| std::env::var_os("DGX_CRAWLER_ASN_DATABASE").map(PathBuf::from))
        {
            config.geography.asn_database = Some(path);
        }
        config.validate()?;
        Ok(config)
    }
}
pub async fn run(args: &CrawlerArgs) -> Result<(), Error> {
    dg_xch_crawler::run(args.config()?).await
}
