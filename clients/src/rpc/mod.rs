pub mod full_node;
pub mod rpc_generator;
pub mod simulator;
pub mod wallet;

use crate::ClientSSLConfig;
use dg_xch_core::ssl::load_ssl_cert_and_key;
use reqwest::{Client, ClientBuilder};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::io::Error;
use std::path::Path;
use std::time::Duration;

fn _version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
fn _pkg_name() -> &'static str {
    env!("CARGO_PKG_NAME")
}

#[must_use]
pub fn version() -> String {
    format!("{}: {}", _pkg_name(), _version())
}

#[test]
fn test_version() {
    println!("{}", version());
}

#[must_use]
pub fn get_url(host: &str, port: u16, request_uri: &str) -> String {
    format!("https://{host}:{port}/{request_uri}")
}

#[must_use]
pub fn get_insecure_url(host: &str, port: u16, request_uri: &str) -> String {
    format!("http://{host}:{port}/{request_uri}")
}

pub fn get_client(ssl_path: &Option<ClientSSLConfig>, timeout: u64) -> Result<Client, Error> {
    get_client_builder(ssl_path, timeout)?
        .build()
        .map_err(Error::other)
}

pub fn get_client_builder(
    ssl_path: &Option<ClientSSLConfig>,
    timeout: u64,
) -> Result<ClientBuilder, Error> {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let mut builder = ClientBuilder::new()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(timeout));
    if let Some(ssl) = ssl_path {
        let (certificate, private_key) =
            load_ssl_cert_and_key(Path::new(&ssl.ssl_crt_path), Path::new(&ssl.ssl_key_path))?;
        let private_key = zeroize::Zeroizing::new(private_key);
        let mut identity = zeroize::Zeroizing::new(certificate);
        let additional = private_key
            .len()
            .checked_add(1)
            .ok_or_else(|| Error::other("TLS identity length overflow"))?;
        identity.try_reserve(additional).map_err(Error::other)?;
        identity.push(b'\n');
        identity.extend_from_slice(&private_key);
        builder = builder.identity(reqwest::Identity::from_pem(&identity).map_err(Error::other)?);
        let authorities = std::fs::read(&ssl.ssl_ca_crt_path)?;
        builder = builder.tls_built_in_root_certs(false);
        for certificate in
            reqwest::Certificate::from_pem_bundle(&authorities).map_err(Error::other)?
        {
            builder = builder.add_root_certificate(certificate);
        }
    }
    Ok(builder)
}

pub fn get_http_client(timeout: u64) -> Result<Client, Error> {
    ClientBuilder::new()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(timeout))
        .build()
        .map_err(|e| Error::other(format!("{e:?}")))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChiaRpcError {
    pub error: Option<String>,
    pub success: bool,
}

impl From<ChiaRpcError> for Error {
    fn from(error: ChiaRpcError) -> Self {
        Error::other(format!(
            "Success: {}, Message: {}",
            error.success,
            error.error.unwrap_or_default()
        ))
    }
}

pub async fn post<T, S: std::hash::BuildHasher>(
    client: &Client,
    url: &str,
    data: &Map<String, Value>,
    additional_headers: &Option<HashMap<String, String, S>>,
) -> Result<T, ChiaRpcError>
where
    T: DeserializeOwned,
{
    // Build request + headers
    let mut rb = client.post(url);
    if let Some(h) = additional_headers {
        for (k, v) in h {
            rb = rb.header(k, v);
        }
    }

    // Send
    let resp = rb.json(data).send().await.map_err(|e| ChiaRpcError {
        error: Some(format!("request error: {e}")),
        success: false,
    })?;

    let status = resp.status();
    let body = crate::http::bounded_body(resp, 64 * 1024 * 1024)
        .await
        .map_err(|error| ChiaRpcError {
            error: Some(error.to_string()),
            success: false,
        })?;

    if !status.is_success() {
        return Err(ChiaRpcError {
            error: Some(format!("http {status}")),
            success: false,
        });
    }

    // Parse once
    let val: Value = serde_json::from_slice(&body).map_err(|e| ChiaRpcError {
        error: Some(format!("json parse error: {e}")),
        success: false,
    })?;

    // Respect RPC envelope: success must be true for Ok(...)
    if let Some(success) = val.get("success").and_then(|b| b.as_bool())
        && !success
    {
        // Extract server error if present
        let err_msg = val
            .get("error")
            .and_then(|e| e.as_str())
            .map(|message| message.chars().take(1024).collect())
            .unwrap_or_else(|| "server returned success=false".into());
        return Err(ChiaRpcError {
            error: Some(err_msg),
            success: false,
        });
    }

    // Now decode into T. If this fails while success==true, it's a decode error (still an error).
    serde_json::from_value::<T>(val).map_err(|e| ChiaRpcError {
        error: Some(format!("response decode error: {e}")),
        success: false,
    })
}

#[cfg(all(test, unix))]
mod identity_file_tests {
    use super::{ClientSSLConfig, get_client};
    use dg_xch_core::constants::{CHIA_CA_CRT, CHIA_CA_KEY};
    use std::io::{ErrorKind, Write};
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    use std::path::PathBuf;

    struct TestIdentity {
        directory: PathBuf,
        config: ClientSSLConfig,
    }

    impl TestIdentity {
        fn new() -> Self {
            let directory = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
            std::fs::create_dir(&directory).unwrap();
            let certificate = directory.join("identity.crt");
            let private_key = directory.join("identity.key");
            std::fs::write(&certificate, CHIA_CA_CRT.as_bytes()).unwrap();
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&private_key)
                .unwrap()
                .write_all(CHIA_CA_KEY.as_bytes())
                .unwrap();
            Self {
                config: ClientSSLConfig {
                    ssl_crt_path: certificate.to_string_lossy().into_owned(),
                    ssl_key_path: private_key.to_string_lossy().into_owned(),
                    ssl_ca_crt_path: certificate.to_string_lossy().into_owned(),
                },
                directory,
            }
        }
    }

    impl Drop for TestIdentity {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }

    #[test]
    fn rpc_accepts_restrictive_matching_identity_files() {
        let identity = TestIdentity::new();
        let result = get_client(&Some(identity.config.clone()), 1);
        assert!(
            result.is_ok(),
            "RPC identity configuration failed: {result:?}"
        );
    }

    #[test]
    fn rpc_rejects_permissive_identity_key_files() {
        let identity = TestIdentity::new();
        std::fs::set_permissions(
            &identity.config.ssl_key_path,
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        let error = get_client(&Some(identity.config.clone()), 1).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
    }

    #[test]
    fn rpc_rejects_symlinked_identity_key_files() {
        let identity = TestIdentity::new();
        let original = identity.directory.join("original.key");
        std::fs::rename(&identity.config.ssl_key_path, &original).unwrap();
        std::os::unix::fs::symlink(&original, &identity.config.ssl_key_path).unwrap();
        assert!(get_client(&Some(identity.config.clone()), 1).is_err());
        assert_eq!(std::fs::read(original).unwrap(), CHIA_CA_KEY.as_bytes());
    }
}
