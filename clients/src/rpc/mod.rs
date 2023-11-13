pub mod full_node;
pub mod wallet;

use crate::protocols::shared::NoCertificateVerification;
use crate::protocols::shared::{load_certs, load_private_key};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::{header, Client, ClientBuilder};
use rustls::ClientConfig;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::io::{Error, ErrorKind};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

fn _version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
fn _pkg_name() -> &'static str {
    env!("CARGO_PKG_NAME")
}

pub fn version() -> String {
    format!("{}: {}", _pkg_name(), _version())
}

#[test]
fn test_version() {
    println!("{}", version());
}

pub fn get_url(host: &str, port: u16, request_uri: &str) -> String {
    format!(
        "https://{host}:{port}/{request_uri}",
        host = host,
        port = port,
        request_uri = request_uri
    )
}

pub fn get_client(ssl_path: Option<String>) -> Result<Client, Error> {
    if let Some(ssl_path) = ssl_path {
        let certs = load_certs(&format!("{}/{}", ssl_path, "/daemon/private_daemon.crt"))?;
        let key = load_private_key(&format!("{}/{}", ssl_path, "/daemon/private_daemon.key"))?;
        let config = ClientConfig::builder()
            .with_safe_defaults()
            .with_custom_certificate_verifier(Arc::new(NoCertificateVerification {}))
            .with_client_auth_cert(certs, key)
            .map_err(|e| Error::new(ErrorKind::Other, format!("{:?}", e)))?;
        ClientBuilder::new()
            .use_preconfigured_tls(config)
            .timeout(Duration::from_secs(300))
            .build()
            .map_err(|e| Error::new(ErrorKind::Other, format!("{:?}", e)))
    } else {
        ClientBuilder::new()
            .danger_accept_invalid_certs(true)
            .timeout(Duration::from_secs(300))
            .build()
            .map_err(|e| Error::new(ErrorKind::Other, format!("{:?}", e)))
    }
}

pub async fn post<T>(
    client: &Client,
    url: &str,
    data: &Map<String, Value>,
    additional_headers: &Option<HashMap<String, String>>,
) -> Result<T, Error>
where
    T: DeserializeOwned,
{
    let mut header_map = HeaderMap::new();
    if let Some(m) = additional_headers {
        for (k, v) in m {
            header_map.insert(
                HeaderName::from_str(k).map_err(|e| {
                    Error::new(
                        ErrorKind::InvalidData,
                        format!("Failed to Parse Header Name {},\r\n {}", k, e),
                    )
                })?,
                HeaderValue::from_str(v).map_err(|e| {
                    Error::new(
                        ErrorKind::InvalidData,
                        format!("Failed to Parse Header value {},\r\n {}", v, e),
                    )
                })?,
            );
        }
    }
    match client.post(url).headers(header_map).json(data).send().await {
        Ok(resp) => match resp.status() {
            reqwest::StatusCode::OK => {
                let body = resp
                    .text()
                    .await
                    .map_err(|e| Error::new(ErrorKind::InvalidData, e.to_string()))?;
                serde_json::from_str(body.as_str()).map_err(|e| {
                    Error::new(
                        ErrorKind::InvalidData,
                        format!("Failed to Parse Json {},\r\n {}", body, e),
                    )
                })
            }
            _ => Err(Error::new(
                ErrorKind::InvalidData,
                format!("Bad Status Code: {:?}, for URL {:?}", resp.status(), url),
            )),
        },
        Err(err) => Err(Error::new(ErrorKind::InvalidData, format!("{:?}", err))),
    }
}
