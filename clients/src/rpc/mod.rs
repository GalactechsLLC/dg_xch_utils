pub mod full_node;
pub mod simulator;
pub mod wallet;

use crate::ClientSSLConfig;
use dg_xch_core::protocols::shared::NoCertificateVerification;
use dg_xch_core::ssl::{
    generate_ca_signed_cert_data, load_certs, load_certs_from_bytes, load_private_key,
    load_private_key_from_bytes, CHIA_CA_CRT, CHIA_CA_KEY,
};
use reqwest::{Client, ClientBuilder};
use rustls::ClientConfig;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
use std::cmp::min;
use std::collections::HashMap;
use std::env;
use std::io::{Error, ErrorKind};
use std::sync::Arc;
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
    let (certs, key) = if let Some(ssl_info) = ssl_path {
        (
            load_certs(&ssl_info.ssl_crt_path)?,
            load_private_key(&ssl_info.ssl_key_path)?,
        )
    } else if let (Some(crt), Some(key)) = (
        env::var("PRIVATE_CA_CRT").ok(),
        env::var("PRIVATE_CA_KEY").ok(),
    ) {
        let (cert_bytes, key_bytes) = generate_ca_signed_cert_data(crt.as_bytes(), key.as_bytes())?;
        (
            load_certs_from_bytes(&cert_bytes)?,
            load_private_key_from_bytes(&key_bytes)?,
        )
    } else if let (Some(crt), Some(key)) =
        (env::var("PRIVATE_CRT").ok(), env::var("PRIVATE_KEY").ok())
    {
        (
            load_certs_from_bytes(crt.as_bytes())?,
            load_private_key_from_bytes(key.as_bytes())?,
        )
    } else {
        let (cert_bytes, key_bytes) =
            generate_ca_signed_cert_data(CHIA_CA_CRT.as_bytes(), CHIA_CA_KEY.as_bytes())?;
        (
            load_certs_from_bytes(&cert_bytes)?,
            load_private_key_from_bytes(&key_bytes)?,
        )
    };
    let config = ClientConfig::builder()
        .with_safe_defaults()
        .with_custom_certificate_verifier(Arc::new(NoCertificateVerification {}))
        .with_client_auth_cert(certs, key)
        .map_err(|e| Error::new(ErrorKind::Other, format!("{e:?}")))?;
    ClientBuilder::new()
        .use_preconfigured_tls(config)
        .timeout(Duration::from_secs(timeout))
        .build()
        .map_err(|e| Error::new(ErrorKind::Other, format!("{e:?}")))
}

pub fn get_http_client(timeout: u64) -> Result<Client, Error> {
    ClientBuilder::new()
        .timeout(Duration::from_secs(timeout))
        .build()
        .map_err(|e| Error::new(ErrorKind::Other, format!("{e:?}")))
}

pub async fn post<T, S: std::hash::BuildHasher>(
    client: &Client,
    url: &str,
    data: &Map<String, Value>,
    additional_headers: &Option<HashMap<String, String, S>>,
) -> Result<T, Error>
where
    T: DeserializeOwned,
{
    let mut request_builder = client.post(url);
    if let Some(headers) = additional_headers {
        for (k, v) in headers {
            request_builder = request_builder.header(k, v);
        }
    }
    match request_builder.json(data).send().await {
        Ok(resp) => match resp.status() {
            reqwest::StatusCode::OK => {
                let body = resp
                    .text()
                    .await
                    .map_err(|e| Error::new(ErrorKind::InvalidData, e.to_string()))?;
                serde_json::from_str(body.as_str()).map_err(|e| {
                    Error::new(
                        ErrorKind::InvalidData,
                        format!(
                            "Failed to Parse Json {},\r\n {}",
                            &body[0..min(body.len(), 1024)],
                            e
                        ),
                    )
                })
            }
            _ => Err(Error::new(
                ErrorKind::InvalidData,
                format!("Bad Status Code: {:?}, for URL {:?}", resp.status(), url),
            )),
        },
        Err(err) => Err(Error::new(ErrorKind::InvalidData, format!("{err:?}"))),
    }
}
