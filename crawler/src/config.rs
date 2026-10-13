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
    pub concurrency: usize,
    /// Minimum spacing between all outbound connection attempts.
    pub interval_ms: u64,
    /// Successful peers are revisited after this delay (plus jitter).
    pub revisit_seconds: u64,
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
            concurrency: 4,
            interval_ms: 2000,
            revisit_seconds: 3600,
            timeout_seconds: 15,
            recent_seconds: 86400,
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
            || !(1..=32).contains(&self.concurrency)
            || !(500..=60_000).contains(&self.interval_ms)
            || !(600..=86400).contains(&self.revisit_seconds)
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
        if self.network != "mainnet" && self.seed.is_empty() {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "non-mainnet networks require explicit --seed host:port",
            ));
        }
        Ok(())
    }
    pub fn seeds(&self) -> Vec<String> {
        if self.seed.is_empty() {
            vec!["node.chia.net:8444".into()]
        } else {
            self.seed.clone()
        }
    }
}

pub use dg_xch_servers::discovery::admissible;
