use crate::farmer::config::Config;
use crate::farmer::{CA_PRIVATE_CRT, PRIVATE_CRT, PRIVATE_KEY};
use dg_xch_clients::ClientSSLConfig;
use dg_xch_clients::rpc::full_node::FullnodeClient;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::ssl::load_certs_from_bytes;
use dg_xch_core::utils::hash_256;
use directories::ProjectDirs;
use std::collections::HashMap;
use std::io::{Error, ErrorKind};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;

pub fn ensure_farmer_tls(root: &Path) -> Result<(), Error> {
    use crate::farmer::{CA_PUBLIC_CRT, HARVESTER_CRT, PUBLIC_CRT, PUBLIC_KEY};
    use dg_xch_core::ssl::{create_all_ssl, load_certs, load_private_key};
    let certificates = [
        CA_PRIVATE_CRT,
        CA_PUBLIC_CRT,
        PRIVATE_CRT,
        PUBLIC_CRT,
        HARVESTER_CRT,
    ];
    let keys = [PRIVATE_KEY, PUBLIC_KEY, "harvester/private_harvester.key"];
    let identity_present = certificates
        .iter()
        .chain(keys.iter())
        .any(|relative| root.join(relative).exists());
    if !identity_present {
        create_all_ssl(root, false)?;
    }
    for relative in certificates {
        let path = root.join(relative);
        let certificates = load_certs(
            path.to_str()
                .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "TLS path is not UTF-8"))?,
        )?;
        if certificates.is_empty() {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!("empty TLS certificate: {}", path.display()),
            ));
        }
    }
    for relative in keys {
        let path = root.join(relative);
        load_private_key(
            path.to_str()
                .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "TLS path is not UTF-8"))?,
        )?;
    }
    Ok(())
}

pub fn get_ssl_root_path<C: Clone>(config: &Config<C>) -> Result<PathBuf, Error> {
    match &config.ssl_root_path {
        Some(path) => Ok(PathBuf::from(path)),
        None => ProjectDirs::from("com", "Galactechs", "dg_xch")
            .map(|paths| paths.config_dir().join("ssl"))
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::NotFound,
                    "user configuration directory is unavailable",
                )
            }),
    }
}

pub fn rpc_client_from_config<C: Clone>(
    config: &Config<C>,
    headers: &Option<HashMap<String, String>>,
) -> Result<Arc<FullnodeClient>, Error> {
    let root = get_ssl_root_path(config)?;
    Ok(Arc::new(FullnodeClient::new_verified(
        &config.fullnode_rpc_host,
        config.fullnode_rpc_port,
        30,
        Some(ClientSSLConfig {
            ssl_crt_path: root.join(PRIVATE_CRT).to_string_lossy().into_owned(),
            ssl_key_path: root.join(PRIVATE_KEY).to_string_lossy().into_owned(),
            ssl_ca_crt_path: root.join(CA_PRIVATE_CRT).to_string_lossy().into_owned(),
        }),
        headers,
    )?))
}

pub async fn load_client_id<C: Clone>(config: Arc<RwLock<Config<C>>>) -> Result<Bytes32, Error> {
    let root = get_ssl_root_path(&*config.read().await)?;
    let certificates = load_certs_from_bytes(
        &tokio::fs::read(root.join(Path::new(crate::farmer::HARVESTER_CRT))).await?,
    )?;
    let certificate = certificates
        .first()
        .ok_or_else(|| Error::new(ErrorKind::InvalidData, "harvester certificate is empty"))?;
    Ok(Bytes32::const_new(hash_256(certificate)))
}
