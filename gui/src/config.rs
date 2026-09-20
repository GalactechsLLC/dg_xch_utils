use dg_xch_clients::ClientSSLConfig;
use dg_xch_clients::rpc::full_node::FullnodeClient;
use dg_xch_core::consensus::chain_definition::ChainDefinition;
use dg_xch_core::consensus::constants::{ChiaNetwork, ConsensusConstants};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::io::{Error, ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;

#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Theme {
    #[default]
    Midnight,
    Daylight,
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum GpuBackend {
    #[default]
    Auto,
    Cuda,
    Vulkan,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub version: u32,
    pub theme: Theme,
    pub node_host: String,
    pub node_port: u16,
    pub network: String,
    pub genesis_header_hash: String,
    pub chain_definition_path: String,
    pub certificate: String,
    pub private_key: String,
    pub certificate_authority: String,
    pub farmer_config: String,
    pub farmer_ws_host: String,
    pub farmer_ws_port: u16,
    pub farmer_ssl_root: String,
    pub farmer_payout_address: String,
    pub cuda_executable: String,
    pub cuda_device: usize,
    pub gpu_backend: GpuBackend,
    pub vulkan_device: usize,
    pub plot_directories: Vec<PathBuf>,
    pub poll_seconds: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: 1,
            theme: Theme::Midnight,
            node_host: "localhost".into(),
            node_port: 8555,
            network: "mainnet".into(),
            genesis_header_hash: String::new(),
            chain_definition_path: String::new(),
            certificate: String::new(),
            private_key: String::new(),
            certificate_authority: String::new(),
            farmer_config: String::new(),
            cuda_executable: String::new(),
            cuda_device: 0,
            gpu_backend: GpuBackend::Auto,
            vulkan_device: 0,
            plot_directories: Vec::new(),
            poll_seconds: 15,
            farmer_ws_host: "localhost".into(),
            farmer_ws_port: 8444,
            farmer_ssl_root: String::new(),
            farmer_payout_address: String::new(),
        }
    }
}

#[derive(Clone)]
pub struct AppPaths {
    pub config: PathBuf,
    pub data: PathBuf,
}

impl AppPaths {
    pub fn discover() -> Result<Self, Error> {
        let paths = ProjectDirs::from("com", "Galactechs", "dg_xch")
            .ok_or_else(|| Error::new(ErrorKind::NotFound, "cannot locate user directories"))?;
        Ok(Self {
            config: paths.config_dir().to_owned(),
            data: paths.data_local_dir().to_owned(),
        })
    }

    pub fn accounts(&self) -> PathBuf {
        self.data.join("accounts")
    }

    pub fn load(&self) -> Result<Settings, Error> {
        let file = match std::fs::File::open(self.config.join("desktop.json")) {
            Ok(file) => file,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Settings::default()),
            Err(error) => return Err(error),
        };
        let mut bytes = Vec::new();
        file.take(1_048_577).read_to_end(&mut bytes)?;
        if bytes.len() > 1_048_576 {
            return Err(Error::other("settings file is too large"));
        }
        let settings: Settings = serde_json::from_slice(&bytes).map_err(Error::other)?;
        settings.validate()?;
        Ok(settings)
    }

    pub fn save(&self, settings: &Settings) -> Result<(), Error> {
        settings.validate()?;
        std::fs::create_dir_all(&self.config)?;
        let mut output = tempfile::NamedTempFile::new_in(&self.config)?;
        output.write_all(&serde_json::to_vec_pretty(settings).map_err(Error::other)?)?;
        output.as_file().sync_all()?;
        output
            .persist(self.config.join("desktop.json"))
            .map_err(|error| error.error)?;
        Ok(())
    }
}

impl Settings {
    pub fn validate(&self) -> Result<(), Error> {
        if self.version != 1
            || self.node_port == 0
            || self.node_host.is_empty()
            || self.node_host.contains(['/', '@', '?', '#'])
            || !(5..=300).contains(&self.poll_seconds)
        {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "invalid settings version, host, port or polling interval",
            ));
        }
        self.constants()?;
        Ok(())
    }

    pub fn constants(&self) -> Result<ConsensusConstants, Error> {
        if self.chain_definition_path.is_empty() {
            ChiaNetwork::from_str(&self.network)
                .map(ConsensusConstants::from)
                .map_err(Error::other)
        } else {
            let definition: ChainDefinition = serde_json::from_reader(std::fs::File::open(
                Path::new(&self.chain_definition_path),
            )?)
            .map_err(Error::other)?;
            if definition.network_id != self.network {
                return Err(Error::other(
                    "chain definition does not match selected network",
                ));
            }
            definition.constants().map_err(Error::other)
        }
    }

    pub fn client(&self) -> Result<FullnodeClient, Error> {
        self.validate()?;
        let supplied = [
            &self.certificate,
            &self.private_key,
            &self.certificate_authority,
        ]
        .iter()
        .filter(|value| !value.is_empty())
        .count();
        let ssl = match supplied {
            0 => None,
            3 => Some(ClientSSLConfig {
                ssl_crt_path: self.certificate.clone(),
                ssl_key_path: self.private_key.clone(),
                ssl_ca_crt_path: self.certificate_authority.clone(),
            }),
            _ => return Err(Error::other("provide all three TLS paths or none")),
        };
        FullnodeClient::new_verified(&self.node_host, self.node_port, 15, ssl, &None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_and_fail_closed() {
        let directory = tempfile::tempdir().unwrap();
        let paths = AppPaths {
            config: directory.path().join("config"),
            data: directory.path().join("data"),
        };
        let mut settings = paths.load().unwrap();
        settings.theme = Theme::Daylight;
        paths.save(&settings).unwrap();
        assert!(paths.load().unwrap().theme == Theme::Daylight);
        settings.network = "typo".into();
        assert!(paths.save(&settings).is_err());
        std::fs::write(paths.config.join("desktop.json"), b"broken").unwrap();
        assert!(paths.load().is_err());
    }

    #[test]
    fn gpu_defaults_to_auto_and_preserves_explicit_legacy_selection() {
        assert!(Settings::default().gpu_backend == GpuBackend::Auto);
        let settings: Settings = serde_json::from_str("{}").unwrap();
        assert!(settings.gpu_backend == GpuBackend::Auto);
        let settings: Settings =
            serde_json::from_str(r#"{"gpu_backend":"Cuda","cuda_device":2,"vulkan_device":1}"#)
                .unwrap();
        assert!(settings.gpu_backend == GpuBackend::Cuda);
        assert_eq!(settings.cuda_device, 2);
        assert_eq!(settings.vulkan_device, 1);
    }
}
