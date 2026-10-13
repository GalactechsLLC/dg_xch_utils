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
    #[arg(long)]
    listen: Option<SocketAddr>,
    /// Chia network ID; alternatively supply the root --network flag.
    #[arg(long)]
    network: Option<String>,
    /// Repeat to supply bootstrap nodes (IP or hostname with port).
    #[arg(long)]
    seed: Vec<String>,
    #[arg(long)]
    concurrency: Option<usize>,
    /// Minimum spacing between all outbound connection attempts.
    #[arg(long)]
    interval_ms: Option<u64>,
    /// Successful peers are revisited after this delay (plus jitter).
    #[arg(long)]
    revisit_seconds: Option<u64>,
    /// Deadline for the entire peer probe, including handshake and discovery.
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
        if let Some(value) = self.concurrency {
            config.concurrency = value;
        }
        if let Some(value) = self.interval_ms {
            config.interval_ms = value;
        }
        if let Some(value) = self.revisit_seconds {
            config.revisit_seconds = value;
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
        config.validate()?;
        Ok(config)
    }
}
pub async fn run(args: &CrawlerArgs) -> Result<(), Error> {
    dg_xch_crawler::run(args.config()?).await
}
