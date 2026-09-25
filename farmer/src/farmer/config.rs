use blst::min_pk::SecretKey;
use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes48};
use dg_xch_core::config::PoolWalletConfig;
use dg_xch_core::consensus::chain_definition::{ChainDefinition, ChainSelection};
use dg_xch_core::consensus::constants::ConsensusConstants;
use dg_xch_keys::parse_payout_address;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::Error;
use std::path::{Path, PathBuf};

const fn default_true() -> bool {
    true
}
const fn default_metrics_port() -> u16 {
    8080
}
const fn default_none<T>() -> Option<T> {
    None
}

#[derive(Default, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FarmingInfo {
    pub farmer_secret_key: Bytes32,
    pub launcher_id: Option<Bytes32>,
    pub pool_secret_key: Option<Bytes32>,
    pub owner_secret_key: Option<Bytes32>,
    pub auth_secret_key: Option<Bytes32>,
}

impl std::fmt::Debug for FarmingInfo {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FarmingInfo")
            .field("launcher_id", &self.launcher_id)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MetricsConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_metrics_port")]
    pub port: u16,
}
impl Default for MetricsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            port: 8080,
        }
    }
}

#[derive(Default, Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DruidGardenHarvesterConfig {
    #[serde(default = "Vec::new")]
    pub plot_directories: Vec<String>,
}

#[derive(Default, Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HarvesterConfig<C = ()> {
    #[serde(default)]
    pub plot_directories: Vec<String>,
    #[serde(default = "default_none")]
    #[serde(rename = "pos1", alias = "druid_garden")]
    pub druid_garden: Option<DruidGardenHarvesterConfig>,
    #[serde(default)]
    pub pos2: Option<Pos2HarvesterConfig>,
    #[serde(default)]
    pub bladebit: Option<Pos1HarvesterConfig>,
    #[serde(default)]
    pub gigahorse: Option<GigahorseHarvesterConfig>,
    #[serde(default = "default_none")]
    pub custom_config: Option<C>,
}

impl<C> HarvesterConfig<C> {
    pub fn directories(&self) -> Vec<std::path::PathBuf> {
        let mut directories = self.plot_directories.clone();
        if let Some(config) = &self.druid_garden {
            directories.extend(config.plot_directories.iter().cloned());
        }
        if let Some(config) = &self.bladebit {
            directories.extend(config.plot_directories.iter().cloned());
        }
        if let Some(config) = &self.pos2 {
            directories.extend(config.plot_directories.iter().cloned());
        }
        if let Some(config) = &self.gigahorse {
            directories.extend(config.plot_directories.iter().cloned());
        }
        directories.sort();
        directories.dedup();
        directories
            .into_iter()
            .map(std::path::PathBuf::from)
            .collect()
    }
}

pub type Pos1HarvesterConfig = DruidGardenHarvesterConfig;

#[derive(Default, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GigahorseBackend {
    #[default]
    Cpu,
    Cuda,
    Vulkan,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GigahorseHarvesterConfig {
    pub plot_directories: Vec<String>,
    pub backend: GigahorseBackend,
    pub device: usize,
    pub memory_mib: u64,
    pub threads: usize,
    pub deadline_ms: u64,
}

impl Default for GigahorseHarvesterConfig {
    fn default() -> Self {
        Self {
            plot_directories: Vec::new(),
            backend: GigahorseBackend::Cpu,
            device: 0,
            memory_mib: 12_288,
            threads: std::thread::available_parallelism().map_or(4, |count| count.get().min(16)),
            deadline_ms: 20_000,
        }
    }
}

impl GigahorseHarvesterConfig {
    pub fn validate(&self) -> Result<(), Error> {
        if !(1..=256).contains(&self.threads)
            || !(1..=120_000).contains(&self.deadline_ms)
            || self.memory_mib < 256
            || self.memory_mib.checked_mul(1024 * 1024).is_none()
            || (self.backend == GigahorseBackend::Cpu && self.device != 0)
        {
            return Err(Error::other(
                "invalid GigaHorse resource limits or CPU device",
            ));
        }
        Ok(())
    }
}

#[derive(Default, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pos2Backend {
    #[default]
    Cpu,
    Auto,
    Cuda,
    Vulkan,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Pos2HarvesterConfig {
    pub plot_directories: Vec<String>,
    pub backend: Pos2Backend,
    pub device: usize,
    pub cuda_helper: Option<PathBuf>,
    pub memory_mib: u64,
    pub max_entries: usize,
    pub max_work: u64,
    pub search_hashes: u64,
    pub max_qualities: usize,
    pub deadline_ms: u64,
    pub parallelism: usize,
}

impl Default for Pos2HarvesterConfig {
    fn default() -> Self {
        Self {
            plot_directories: Vec::new(),
            backend: Pos2Backend::Cpu,
            device: 0,
            cuda_helper: None,
            memory_mib: 1024,
            max_entries: 4_194_304,
            max_work: 2_000_000_000,
            search_hashes: 100_000_000,
            max_qualities: 32,
            deadline_ms: 20_000,
            parallelism: 1,
        }
    }
}

impl Pos2HarvesterConfig {
    pub fn validate(&self) -> Result<(), Error> {
        if self.memory_mib == 0
            || self.memory_mib.checked_mul(1024 * 1024).is_none()
            || self.max_entries == 0
            || self.max_work == 0
            || self.search_hashes == 0
            || !(1..=1024).contains(&self.max_qualities)
            || !(1..=120_000).contains(&self.deadline_ms)
            || !(1..=32).contains(&self.parallelism)
            || (self.backend == Pos2Backend::Cpu && self.device != 0)
        {
            return Err(Error::other(
                "invalid PoS2 farming resource limits or CPU device",
            ));
        }
        if matches!(self.backend, Pos2Backend::Cuda)
            && self
                .cuda_helper
                .as_ref()
                .is_none_or(|path| !path.is_absolute())
        {
            return Err(Error::other(
                "CUDA farming requires an absolute cuda_helper path",
            ));
        }
        if self
            .cuda_helper
            .as_ref()
            .is_some_and(|path| !path.is_absolute())
        {
            return Err(Error::other(
                "cuda_helper must be an absolute executable path",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Config<C = ()>
where
    C: Clone,
{
    pub selected_network: String,
    #[serde(default)]
    pub chain_definition: Option<ChainDefinition>,
    pub ssl_root_path: Option<String>,
    pub fullnode_ws_host: String,
    pub fullnode_ws_port: u16,
    pub fullnode_rpc_host: String,
    pub fullnode_rpc_port: u16,
    pub farmer_info: Vec<FarmingInfo>,
    pub pool_info: Vec<PoolWalletConfig>,
    #[serde(default)]
    pub pool_ca_certificates: Vec<PathBuf>,
    pub payout_address: String,
    pub harvester_configs: HarvesterConfig<C>,
    pub metrics: Option<MetricsConfig>,
}
impl<C: Clone + Serialize> Config<C> {
    pub fn validate_keys(&self) -> Result<(), Error> {
        if let Some(config) = &self.harvester_configs.pos2 {
            config.validate()?;
        }
        if let Some(config) = &self.harvester_configs.gigahorse {
            config.validate()?;
        }
        for info in &self.farmer_info {
            for key in [
                Some(info.farmer_secret_key),
                info.pool_secret_key,
                info.owner_secret_key,
                info.auth_secret_key,
            ]
            .into_iter()
            .flatten()
            {
                SecretKey::from_bytes(key.as_ref())
                    .map_err(|_| Error::other("invalid farmer secret key"))?;
            }
        }
        Ok(())
    }

    pub fn save_as_yaml<P: AsRef<Path>>(&self, path: P) -> Result<(), Error> {
        use std::io::Write;
        let path = path.as_ref();
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let mut output = tempfile::NamedTempFile::new_in(parent)?;
        output.write_all(
            serde_yaml::to_string(self)
                .map_err(Error::other)?
                .as_bytes(),
        )?;
        output.as_file().sync_all()?;
        output.persist(path).map_err(|error| error.error)?;
        Ok(())
    }
    pub fn is_ready(&self) -> bool {
        self.constants().is_ok()
            && !self.fullnode_ws_host.is_empty()
            && !self.fullnode_rpc_host.is_empty()
            && self.fullnode_ws_port != 0
            && self.fullnode_rpc_port != 0
            && !self.farmer_info.is_empty()
            && parse_payout_address(&self.payout_address).is_ok()
            && self.pool_info.iter().all(|c| {
                self.farmer_info
                    .iter()
                    .any(|f| f.launcher_id == Some(c.launcher_id))
            })
    }
    pub fn merge(&mut self, other: Self) {
        self.farmer_info.extend(other.farmer_info);
        self.pool_info.extend(other.pool_info);
        if self.payout_address.is_empty()
            && !other.payout_address.is_empty()
            && parse_payout_address(&other.payout_address).is_ok()
        {
            self.payout_address = other.payout_address;
        }
    }
}

impl<C: Clone> Default for Config<C> {
    fn default() -> Self {
        Config {
            selected_network: "mainnet".to_string(),
            chain_definition: None,
            ssl_root_path: None,
            fullnode_rpc_host: "localhost".to_string(),
            fullnode_rpc_port: 8555,
            fullnode_ws_host: "localhost".to_string(),
            fullnode_ws_port: 8444,
            farmer_info: vec![],
            pool_info: vec![],
            pool_ca_certificates: vec![],
            payout_address: "".to_string(),
            harvester_configs: HarvesterConfig {
                plot_directories: Vec::new(),
                druid_garden: Some(DruidGardenHarvesterConfig::default()),
                pos2: None,
                bladebit: None,
                gigahorse: None,
                custom_config: None,
            },
            metrics: Some(MetricsConfig {
                enabled: true,
                port: 8080,
            }),
        }
    }
}

impl<C: Clone> Config<C> {
    pub fn pool_client(&self) -> Result<dg_xch_clients::api::pool::DefaultPoolClient, Error> {
        use std::io::Read;
        if self.pool_ca_certificates.len() > 16 {
            return Err(Error::other("at most 16 custom pool CA files are allowed"));
        }
        let certificates = self
            .pool_ca_certificates
            .iter()
            .map(|path| {
                let mut bytes = Vec::new();
                fs::File::open(path)?
                    .take(1024 * 1024 + 1)
                    .read_to_end(&mut bytes)?;
                Ok(bytes)
            })
            .collect::<Result<Vec<_>, Error>>()?;
        let mut client =
            dg_xch_clients::api::pool::DefaultPoolClient::with_ca_certificates(&certificates)?;
        for pool in &self.pool_info {
            if pool.pooling_version == dg_xch_core::protocols::pool::PoolVersion::V2 {
                let keys = self
                    .farmer_info
                    .iter()
                    .find(|keys| keys.launcher_id == Some(pool.launcher_id))
                    .ok_or_else(|| {
                        Error::other("v2 pool requires explicit launcher-linked keys")
                    })?;
                let bytes = keys.auth_secret_key.ok_or_else(|| {
                    Error::other("v2 pool requires its synthetic authentication key")
                })?;
                if keys.owner_secret_key != Some(bytes) {
                    return Err(Error::other(
                        "v2 owner and authentication keys must be the same synthetic PlotNFT key",
                    ));
                }
                let key = SecretKey::from_bytes(bytes.as_ref())
                    .map_err(|_| Error::other("invalid v2 authentication key"))?;
                if key.sk_to_pk().to_bytes() != pool.owner_public_key.as_ref() {
                    return Err(Error::other(
                        "v2 PlotNFT public key does not match the authentication key",
                    ));
                }
                client.add_v2_account(
                    &pool.pool_url,
                    pool.launcher_id,
                    pool.target_puzzle_hash,
                    key,
                )?;
            }
        }
        Ok(client)
    }

    pub fn constants(&self) -> Result<ConsensusConstants, Error> {
        ChainSelection::from_config(&self.selected_network, self.chain_definition.as_ref())
            .and_then(|chain| chain.constants())
            .map_err(Error::other)
    }

    pub fn network_id(&self) -> Result<String, Error> {
        ChainSelection::from_config(&self.selected_network, self.chain_definition.as_ref())
            .and_then(|chain| chain.handshake_network_id())
            .map_err(Error::other)
    }
}
impl<C: for<'a> Deserialize<'a> + Clone> TryFrom<&Path> for Config<C> {
    type Error = Error;
    fn try_from(value: &Path) -> Result<Self, Self::Error> {
        serde_yaml::from_str::<Config<C>>(&fs::read_to_string(value)?)
            .map_err(|e| Error::other(format!("{e:?}")))
    }
}
impl<C: for<'a> Deserialize<'a> + Clone> TryFrom<&PathBuf> for Config<C> {
    type Error = Error;
    fn try_from(value: &PathBuf) -> Result<Config<C>, Self::Error> {
        Config::<C>::try_from(value.as_path())
    }
}

pub async fn load_keys<C: Clone>(
    config: &Config<C>,
) -> (
    HashMap<Bytes48, SecretKey>,
    HashMap<Bytes48, SecretKey>,
    HashMap<Bytes48, SecretKey>,
    HashMap<Bytes48, SecretKey>,
) {
    let mut farmer_secret_keys = HashMap::default();
    let mut owner_secret_keys = HashMap::default();
    let mut auth_secret_keys = HashMap::default();
    let mut pool_secret_keys = HashMap::default();
    for farmer_info in config.farmer_info.iter() {
        let f_sk: SecretKey = farmer_info.farmer_secret_key.into();
        farmer_secret_keys.insert(f_sk.sk_to_pk().to_bytes().into(), f_sk.clone());
        if let Some(pk) = farmer_info.pool_secret_key {
            let sec_key: SecretKey = pk.into();
            pool_secret_keys.insert(sec_key.sk_to_pk().to_bytes().into(), sec_key.clone());
        }
        if let Some(pk) = farmer_info.owner_secret_key {
            let sec_key: SecretKey = pk.into();
            owner_secret_keys.insert(sec_key.sk_to_pk().to_bytes().into(), sec_key.clone());
            if let Some(pk2) = farmer_info.auth_secret_key {
                let a_sec_key: SecretKey = pk2.into();
                auth_secret_keys.insert(sec_key.sk_to_pk().to_bytes().into(), a_sec_key.clone());
            }
        }
    }
    (
        farmer_secret_keys,
        owner_secret_keys,
        auth_secret_keys,
        pool_secret_keys,
    )
}

#[cfg(test)]
mod harvester_config_tests {
    use super::*;

    #[test]
    fn common_and_legacy_directories_merge_without_changing_compute_settings() {
        let config: HarvesterConfig = serde_yaml::from_str(
            "plot_directories: [/shared]\ndruid_garden:\n  plot_directories: [/old, /shared]\nbladebit:\n  plot_directories: [/bladebit]\npos2:\n  plot_directories: [/next]\ngigahorse:\n  plot_directories: [/gh]\n  backend: vulkan\n  device: 2\n",
        ).unwrap();
        assert_eq!(
            config.directories(),
            ["/bladebit", "/gh", "/next", "/old", "/shared"].map(std::path::PathBuf::from)
        );
        assert_eq!(
            config.gigahorse.as_ref().unwrap().backend,
            GigahorseBackend::Vulkan
        );
        assert_eq!(config.gigahorse.as_ref().unwrap().device, 2);
        let serialized = serde_yaml::to_string(&config).unwrap();
        let decoded: HarvesterConfig = serde_yaml::from_str(&serialized).unwrap();
        assert_eq!(decoded, config);
        assert!(serialized.contains("pos1:"));
        assert!(!serialized.contains("druid_garden:"));
    }

    #[test]
    fn common_directories_do_not_require_backend_blocks() {
        let config: HarvesterConfig = serde_yaml::from_str("plot_directories: [/plots]\n").unwrap();
        assert_eq!(config.directories(), [std::path::PathBuf::from("/plots")]);
        assert_eq!(
            config.gigahorse.unwrap_or_default().backend,
            GigahorseBackend::Cpu
        );
        let legacy: HarvesterConfig =
            serde_yaml::from_str("pos1:\n  plot_directories: [/old]\n").unwrap();
        assert_eq!(legacy.directories(), [std::path::PathBuf::from("/old")]);
    }
}
