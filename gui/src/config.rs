use serde::{Deserialize, Serialize};
use std::convert::TryFrom;
use std::fs;
use std::io::{Error, ErrorKind};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Eq, PartialEq, Deserialize, Serialize)]
pub struct Config {
    pub enabled: bool,
    pub full_node_config: FullNodeConfig,
    pub wallet_config: WalletConfig,
    pub simulator_config: SimulatorConfig,
    pub farmer_config: FarmerConfig,
}

#[derive(Debug, Clone, Eq, PartialEq, Deserialize, Serialize)]
pub struct WalletConfig {
    pub enabled: bool,
    pub full_node_hostname: String,
    pub full_node_rpc_port: u16,
    pub full_node_ssl: Option<String>,
}

#[derive(Debug, Clone, Eq, PartialEq, Deserialize, Serialize)]
pub struct FullNodeConfig {
    pub enabled: bool,
    pub full_node_hostname: String,
    pub full_node_ws_port: u16,
    pub full_node_rpc_port: u16,
    pub full_node_ssl: Option<String>,
}

#[derive(Debug, Clone, Eq, PartialEq, Deserialize, Serialize)]
pub struct SimulatorConfig {
    pub enabled: bool,
    pub full_node_hostname: String,
    pub full_node_rpc_port: u16,
}

#[derive(Debug, Clone, Eq, PartialEq, Deserialize, Serialize)]
pub struct FarmerConfig {
    pub enabled: bool,
    pub full_node_hostname: String,
    pub full_node_rpc_port: u16,
    pub full_node_ssl: Option<String>,
}

impl Config {
    pub fn save_as_yaml<P: AsRef<Path>>(&self, path: P) -> Result<(), Error> {
        fs::write(
            path.as_ref(),
            serde_yaml::to_string(&self)
                .map_err(|e| Error::new(ErrorKind::Other, format!("{:?}", e)))?,
        )
    }
}

impl TryFrom<&Path> for Config {
    type Error = Error;
    fn try_from(value: &Path) -> Result<Self, Self::Error> {
        serde_yaml::from_str::<Config>(&fs::read_to_string(value)?)
            .map_err(|e| Error::new(ErrorKind::Other, format!("{:?}", e)))
    }
}
impl TryFrom<&PathBuf> for Config {
    type Error = Error;
    fn try_from(value: &PathBuf) -> Result<Self, Self::Error> {
        Self::try_from(value.as_path())
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            enabled: true,
            full_node_config: FullNodeConfig::default(),
            wallet_config: WalletConfig::default(),
            simulator_config: SimulatorConfig::default(),
            farmer_config: FarmerConfig::default(),
        }
    }
}

impl Default for WalletConfig {
    fn default() -> Self {
        WalletConfig {
            enabled: true,
            full_node_hostname: "chia-proxy.galactechs.com".to_string(),
            full_node_rpc_port: 443,
            full_node_ssl: None,
        }
    }
}

impl Default for FullNodeConfig {
    fn default() -> Self {
        FullNodeConfig {
            enabled: true,
            full_node_hostname: "chia-proxy.galactechs.com".to_string(),
            full_node_ws_port: 443,
            full_node_rpc_port: 443,
            full_node_ssl: None,
        }
    }
}

impl Default for SimulatorConfig {
    fn default() -> Self {
        SimulatorConfig {
            enabled: true,
            full_node_hostname: "chia-proxy.galactechs.com".to_string(),
            full_node_rpc_port: 443,
        }
    }
}

impl Default for FarmerConfig {
    fn default() -> Self {
        FarmerConfig {
            enabled: true,
            full_node_hostname: "chia-proxy.galactechs.com".to_string(),
            full_node_rpc_port: 443,
            full_node_ssl: None,
        }
    }
}
