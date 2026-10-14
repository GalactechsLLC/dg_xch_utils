use serde::{Deserialize, Serialize};
use std::{
    io::{Error, ErrorKind},
    net::SocketAddr,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub probe_log: ProbeLog,
    pub listen: SocketAddr,
    pub database_url: Option<String>,
    pub network: String,
    /// Repeat to supply bootstrap full nodes (IP or hostname with port).
    pub seed: Vec<String>,
    /// Include IPv6 seed addresses and outbound probes; discovered records are retained.
    pub enable_ipv6: bool,
    /// Expected full-node port; inferred for mainnet and testnet11.
    pub peer_port: Option<u16>,
    /// Minimum advertised peak height; mainnet defaults to 240000.
    pub minimum_height: Option<u32>,
    /// Separate deadline for a full node to announce NewPeak after discovery.
    pub peak_wait_ms: u64,
    pub concurrency: usize,
    /// Minimum spacing between all outbound connection attempts.
    pub interval_ms: u64,
    /// Successful peers are revisited after this delay (plus jitter).
    pub revisit_seconds: u64,
    /// TCP connection deadline, also bounded by the connection timeout.
    pub tcp_connect_timeout_seconds: u64,
    /// Deadline for TCP, TLS, WebSocket, and the Chia handshake.
    pub timeout_seconds: u64,
    /// Recent means a successful Chia handshake within this window.
    pub recent_seconds: u64,
    /// Remove endpoints with neither gossip nor successful connections in this period.
    pub retention_days: u64,
    /// Permit local/private addresses for controlled local testing.
    pub allow_private_addresses: bool,
}
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProbeLog {
    Off,
    #[default]
    Errors,
    All,
}
impl std::str::FromStr for ProbeLog {
    type Err = Error;
    fn from_str(value: &str) -> Result<Self, Error> {
        match value {
            "off" => Ok(Self::Off),
            "errors" => Ok(Self::Errors),
            "all" => Ok(Self::All),
            _ => Err(Error::new(
                ErrorKind::InvalidInput,
                "probe-log must be off, errors or all",
            )),
        }
    }
}
impl Default for Config {
    fn default() -> Self {
        Self {
            probe_log: ProbeLog::Errors,
            database_url: None,
            listen: SocketAddr::from(([127, 0, 0, 1], 8580)),
            network: "mainnet".into(),
            seed: vec![],
            enable_ipv6: true,
            peer_port: None,
            minimum_height: None,
            peak_wait_ms: 60_000,
            concurrency: 250,
            interval_ms: 10,
            revisit_seconds: 1000,
            tcp_connect_timeout_seconds: 2,
            timeout_seconds: 15,
            recent_seconds: 5 * 86400,
            retention_days: 90,
            allow_private_addresses: false,
        }
    }
}
impl Config {
    pub fn validate(&self) -> Result<(), Error> {
        if self.network.is_empty()
            || self.network.len() > 64
            || self.listen.port() == 0
            || !(1..=256).contains(&self.concurrency)
            || !(1..=60_000).contains(&self.interval_ms)
            || !(600..=86400).contains(&self.revisit_seconds)
            || !(100..=120_000).contains(&self.peak_wait_ms)
            || !(1..=60).contains(&self.tcp_connect_timeout_seconds)
            || !(1..=60).contains(&self.timeout_seconds)
            || !(60..=604800).contains(&self.recent_seconds)
            || !(1..=365).contains(&self.retention_days)
        {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "invalid crawler configuration; see dgx crawler --help",
            ));
        }
        if self
            .database_url
            .as_deref()
            .is_none_or(|url| url.trim().is_empty())
        {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "crawler requires database_url or DGX_CRAWLER_DATABASE_URL",
            ));
        }
        let port = self.crawler_policy()?.port;
        for seed in &self.seed {
            let (host, value) = seed
                .rsplit_once(':')
                .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "seed must be host:port"))?;
            if host.is_empty() || value.parse::<u16>().ok() != Some(port) {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "seed port must match the network peer port",
                ));
            }
        }
        if self.network != "mainnet" && self.seed.is_empty() {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "non-mainnet networks require explicit --seed host:port",
            ));
        }
        Ok(())
    }
    pub fn crawler_policy(&self) -> Result<dg_xch_servers::discovery::CrawlerPolicy, Error> {
        let network_port = match self.network.as_str() {
            "mainnet" => Some(8444),
            "testnet11" => Some(58444),
            _ => None,
        };
        if network_port.is_some() && self.peer_port.is_some() && network_port != self.peer_port {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "peer port does not match the selected network",
            ));
        }
        let port = self
            .peer_port
            .or(network_port)
            .filter(|port| *port >= 1024)
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::InvalidInput,
                    "custom networks require --peer-port (1024..65535)",
                )
            })?;
        Ok(dg_xch_servers::discovery::CrawlerPolicy {
            port,
            enable_ipv6: self.enable_ipv6,
            minimum_height: self.minimum_height.unwrap_or(if self.network == "mainnet" {
                240000
            } else {
                0
            }),
            peak_wait_ms: self.peak_wait_ms,
            tcp_connect_timeout_seconds: self.tcp_connect_timeout_seconds,
        })
    }
    pub fn seeds(&self) -> Vec<String> {
        if self.seed.is_empty() {
            vec![format!("node.chia.net:{}", self.peer_port.unwrap_or(8444))]
        } else {
            self.seed.clone()
        }
    }
}

pub use dg_xch_servers::discovery::admissible;
