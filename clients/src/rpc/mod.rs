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

fn http_error_message(url: &str, status: reqwest::StatusCode, body: &[u8]) -> String {
    // Report the RPC method without exposing URL credentials, query parameters, or request data.
    let endpoint = reqwest::Url::parse(url)
        .ok()
        .map(|url| url.path().chars().take(256).collect::<String>())
        .unwrap_or_else(|| "<unknown endpoint>".into());
    let mut message = format!("http {status} for RPC {endpoint}");
    if let Ok(value) = serde_json::from_slice::<Value>(body)
        && let Some(detail) = value.get("error").and_then(Value::as_str)
    {
        let detail: String = detail
            .chars()
            .take(1024)
            .map(|character| {
                if character.is_control() {
                    ' '
                } else {
                    character
                }
            })
            .collect();
        if !detail.trim().is_empty() {
            message.push_str(": ");
            message.push_str(detail.trim());
        }
    }
    message
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
    post_inner(client, url, data, additional_headers, false)
        .await?
        .ok_or_else(|| ChiaRpcError {
            error: Some("RPC endpoint unavailable".into()),
            success: false,
        })
}

/// Discover an optional RPC endpoint without treating authentication or server failures as absence.
async fn post_optional<T, S: std::hash::BuildHasher>(
    client: &Client,
    url: &str,
    data: &Map<String, Value>,
    additional_headers: &Option<HashMap<String, String, S>>,
) -> Result<Option<T>, ChiaRpcError>
where
    T: DeserializeOwned,
{
    post_inner(client, url, data, additional_headers, true).await
}

async fn post_inner<T, S: std::hash::BuildHasher>(
    client: &Client,
    url: &str,
    data: &Map<String, Value>,
    additional_headers: &Option<HashMap<String, String, S>>,
    optional_endpoint: bool,
) -> Result<Option<T>, ChiaRpcError>
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
    if optional_endpoint
        && matches!(
            status,
            reqwest::StatusCode::NOT_FOUND | reqwest::StatusCode::METHOD_NOT_ALLOWED
        )
    {
        return Ok(None);
    }
    let body = crate::http::bounded_body(resp, 64 * 1024 * 1024)
        .await
        .map_err(|error| ChiaRpcError {
            error: Some(error.to_string()),
            success: false,
        })?;

    if !status.is_success() {
        return Err(ChiaRpcError {
            error: Some(http_error_message(url, status, &body)),
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
    serde_json::from_value::<T>(val)
        .map(Some)
        .map_err(|e| ChiaRpcError {
            error: Some(format!("response decode error: {e}")),
            success: false,
        })
}

#[cfg(test)]
mod rpc_error_tests {
    use super::{http_error_message, post};
    use reqwest::StatusCode;
    use serde_json::{Map, Value, json};
    use std::collections::hash_map::RandomState;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::time::Duration;

    fn mock_rpc_response(
        status: StatusCode,
        body: &str,
    ) -> (std::net::SocketAddr, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let body = body.to_owned();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut reader = BufReader::new(&stream);
            let mut content_length = 0;
            let mut header_bytes = 0;
            loop {
                let mut line = String::new();
                assert!(reader.read_line(&mut line).unwrap() > 0);
                header_bytes += line.len();
                assert!(header_bytes <= 8192);
                if line == "\r\n" {
                    break;
                }
                if let Some((name, value)) = line.split_once(':')
                    && name.eq_ignore_ascii_case("content-length")
                {
                    content_length = value.trim().parse::<usize>().unwrap();
                }
            }
            assert!(content_length <= 4096);
            reader.read_exact(&mut vec![0; content_length]).unwrap();
            drop(reader);
            write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        });
        (address, server)
    }

    #[tokio::test]
    async fn rejected_rpc_reports_method_status_and_server_error_without_request_secrets() {
        let (address, server) = mock_rpc_response(
            StatusCode::BAD_REQUEST,
            r#"{"success":false,"error":"start_height and end_height must span at most 2000 blocks"}"#,
        );
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap();
        let url = format!(
            "http://dummy-user:dummy-password@{address}/get_coin_records_by_puzzle_hashes?token=dummy-token"
        );
        let mut data = Map::new();
        data.insert("puzzle_hashes".into(), json!(["dummy-wallet-hash"]));
        let error = post::<Value, RandomState>(&client, &url, &data, &None)
            .await
            .unwrap_err();
        server.join().unwrap();
        assert!(!error.success);
        let message = error.error.unwrap();
        assert_eq!(
            message,
            "http 400 Bad Request for RPC /get_coin_records_by_puzzle_hashes: start_height and end_height must span at most 2000 blocks"
        );
        for secret in [
            "dummy-user",
            "dummy-password",
            "dummy-token",
            "dummy-wallet-hash",
        ] {
            assert!(!message.contains(secret));
        }
    }

    #[tokio::test]
    async fn capability_discovery_only_treats_http_404_and_405_as_unsupported() {
        for status in [404, 405, 400, 401, 403, 500] {
            let (address, server) = mock_rpc_response(
                StatusCode::from_u16(status).unwrap(),
                r#"{"success":false,"error":"discovery unavailable"}"#,
            );
            let mut client =
                super::full_node::FullnodeClient::new_simulator("127.0.0.1", address.port(), 3)
                    .unwrap();
            client.client = reqwest::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(3))
                .build()
                .unwrap();
            let response = client.get_rpc_capabilities().await;
            server.join().unwrap();
            if status == 404 || status == 405 {
                assert!(response.unwrap().is_none());
            } else {
                let error = response.unwrap_err().error.unwrap();
                assert!(error.starts_with(&format!("http {status}")));
                assert!(error.contains("/get_rpc_capabilities"));
            }
        }
        for body in [
            "<html>not a capability response</html>",
            r#"{"success":false,"error":"http 404 from another operation"}"#,
            r#"{"success":true}"#,
        ] {
            let (address, server) = mock_rpc_response(StatusCode::OK, body);
            let client =
                super::full_node::FullnodeClient::new_simulator("127.0.0.1", address.port(), 3)
                    .unwrap();
            assert!(client.get_rpc_capabilities().await.is_err());
            server.join().unwrap();
        }
        let (address, server) = mock_rpc_response(
            StatusCode::OK,
            r#"{"success":true,"endpoints":{"/get_wallet_coin_records":1}}"#,
        );
        let client =
            super::full_node::FullnodeClient::new_simulator("127.0.0.1", address.port(), 3)
                .unwrap();
        let capabilities = client.get_rpc_capabilities().await.unwrap().unwrap();
        server.join().unwrap();
        assert_eq!(
            capabilities.endpoints.get("/get_wallet_coin_records"),
            Some(&1)
        );
    }

    #[test]
    fn rejected_rpc_ignores_plain_text_html_and_non_string_error_bodies() {
        for body in [
            b"Bad Request".as_slice(),
            b"<html>private gateway diagnostics</html>".as_slice(),
            br#"{"error":{"private":"details"}}"#.as_slice(),
            br#"{"error":" \n\t "}"#.as_slice(),
        ] {
            assert_eq!(
                http_error_message(
                    "https://example.test/get_coin_records_by_hint?secret=hidden",
                    StatusCode::BAD_REQUEST,
                    body,
                ),
                "http 400 Bad Request for RPC /get_coin_records_by_hint"
            );
        }
    }

    #[test]
    fn rejected_rpc_bounds_untrusted_details_and_endpoint_and_removes_control_characters() {
        let body =
            serde_json::to_vec(&json!({"error": "\u{1b}\n\t".to_owned() + &"é".repeat(5000)}))
                .unwrap();
        let message = http_error_message(
            &format!(
                "https://user:password@example.test/{}?secret=hidden",
                "a".repeat(5000)
            ),
            StatusCode::BAD_REQUEST,
            &body,
        );
        assert!(message.chars().count() <= 256 + 1024 + 32);
        assert!(!message.chars().any(char::is_control));
        assert!(!message.contains("password"));
        assert!(!message.contains("hidden"));
        assert!(message.contains("é"));
        assert_eq!(
            http_error_message("not a URL", StatusCode::BAD_REQUEST, b"Bad Request"),
            "http 400 Bad Request for RPC <unknown endpoint>"
        );
    }
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
