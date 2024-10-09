use crate::api::RequestMode;
use async_trait::async_trait;
use blst::min_pk::{AggregateSignature, SecretKey, Signature};
use dg_xch_core::blockchain::sized_bytes::{Bytes32, SizedBytes};
use dg_xch_core::clvm::bls_bindings::sign;
use dg_xch_core::protocols::pool::{
    get_current_authentication_token, AuthenticationPayload, GetFarmerRequest, GetFarmerResponse,
    GetPoolInfoResponse, PoolError, PoolErrorCode, PostFarmerRequest, PostFarmerResponse,
    PostPartialRequest, PostPartialResponse, PutFarmerRequest, PutFarmerResponse,
};
use dg_xch_serialize::{hash_256, ChiaProtocolVersion, ChiaSerialize};
use log::{debug, info, warn};
use reqwest::{Client, RequestBuilder};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::collections::HashMap;
use std::fmt::Debug;
use std::io::{Error, ErrorKind};

#[async_trait]
pub trait PoolClient {
    async fn get_farmer(
        &self,
        url: &str,
        request: GetFarmerRequest,
        headers: &Option<HashMap<String, String>>,
    ) -> Result<GetFarmerResponse, PoolError>;
    async fn post_farmer(
        &self,
        url: &str,
        request: PostFarmerRequest,
        headers: &Option<HashMap<String, String>>,
    ) -> Result<PostFarmerResponse, PoolError>;
    async fn put_farmer(
        &self,
        url: &str,
        request: PutFarmerRequest,
        headers: &Option<HashMap<String, String>>,
    ) -> Result<PutFarmerResponse, PoolError>;
    async fn post_partial(
        &self,
        url: &str,
        request: PostPartialRequest,
        headers: &Option<HashMap<String, String>>,
    ) -> Result<PostPartialResponse, PoolError>;
    async fn get_pool_info(&self, pool_url: &str) -> Result<GetPoolInfoResponse, PoolError>;
}

#[derive(Default, Debug)]
pub struct DefaultPoolClient {
    pub client: Client,
}
impl DefaultPoolClient {
    pub fn new() -> Self {
        Self {
            client: Client::builder()
                .danger_accept_invalid_certs(true)
                .build()
                .unwrap_or_default(),
        }
    }
}
#[async_trait]
impl PoolClient for DefaultPoolClient {
    async fn get_farmer(
        &self,
        url: &str,
        request: GetFarmerRequest,
        headers: &Option<HashMap<String, String>>,
    ) -> Result<GetFarmerResponse, PoolError> {
        send_request(
            self.client.get(format!("{}/farmer", url)),
            "get_farmer",
            headers,
            RequestMode::Query(request),
        )
        .await
    }

    async fn post_farmer(
        &self,
        url: &str,
        request: PostFarmerRequest,
        headers: &Option<HashMap<String, String>>,
    ) -> Result<PostFarmerResponse, PoolError> {
        send_request(
            self.client.post(format!("{}/farmer", url)),
            "post_farmer",
            headers,
            RequestMode::Json(request),
        )
        .await
    }

    async fn put_farmer(
        &self,
        url: &str,
        request: PutFarmerRequest,
        headers: &Option<HashMap<String, String>>,
    ) -> Result<PutFarmerResponse, PoolError> {
        send_request(
            self.client.put(format!("{}/farmer", url)),
            "put_farmer",
            headers,
            RequestMode::Json(request),
        )
        .await
    }

    async fn post_partial(
        &self,
        url: &str,
        request: PostPartialRequest,
        headers: &Option<HashMap<String, String>>,
    ) -> Result<PostPartialResponse, PoolError> {
        send_request(
            self.client.post(format!("{}/partial", url)),
            "post_partial",
            headers,
            RequestMode::Json(request),
        )
        .await
    }
    async fn get_pool_info(&self, pool_url: &str) -> Result<GetPoolInfoResponse, PoolError> {
        send_request(
            self.client.get(format!("{}/pool_info", pool_url)),
            "get_pool_info",
            &None,
            RequestMode::<()>::Send,
        )
        .await
    }
}

async fn send_request<T: Serialize + Debug, R: DeserializeOwned>(
    mut request_builder: RequestBuilder,
    method: &str,
    headers: &Option<HashMap<String, String>>,
    mode: RequestMode<T>,
) -> Result<R, PoolError> {
    if let Some(headers) = headers {
        for (k, v) in headers {
            request_builder = request_builder.header(k, v);
        }
    }

    let future = match mode {
        RequestMode::Json(t) => {
            let (client, request) = request_builder.json(&t).build_split();
            let request = request.map_err(|e| {
                PoolError {
                    error_code: PoolErrorCode::RequestFailed as u8,
                    error_message: e.to_string(),
                }
            })?;
            debug!("Sending request {request:?}");
            debug!("Request Data {t:?}");
            client.execute(request)
        },
        RequestMode::Query(t) => {
            let (client, request) = request_builder.query(&t).build_split();
            let request = request.map_err(|e| {
                PoolError {
                    error_code: PoolErrorCode::RequestFailed as u8,
                    error_message: e.to_string(),
                }
            })?;
            debug!("Sending request {request:?}");
            debug!("Request Data {t:?}");
            client.execute(request)
        },
        RequestMode::Send => {
            let (client, request) = request_builder.build_split();
            let request = request.map_err(|e| {
                PoolError {
                    error_code: PoolErrorCode::RequestFailed as u8,
                    error_message: e.to_string(),
                }
            })?;
            debug!("Sending request {request:?}");
            client.execute(request)
        },
    };
    match future.await {
        Ok(resp) => match resp.status() {
            reqwest::StatusCode::OK => match resp.text().await {
                Ok(body) => {
                    debug!("Got Response from Pool: {body}");
                    match serde_json::from_str::<PoolError>(body.as_str()) {
                        Ok(e) => Err(e),
                        Err(_) => match serde_json::from_str(&body) {
                            Ok(r) => Ok(r),
                            Err(e) => {
                                warn!(
                                "Failed to parse {method} response, Invalid Json: {:?}, {}",
                                e, body
                            );
                                Err(PoolError {
                                    error_code: PoolErrorCode::RequestFailed as u8,
                                    error_message: e.to_string(),
                                })
                            }
                        },
                    }
                },
                Err(e) => {
                    warn!("Failed to {method}, Invalid Body: {:?}", e);
                    Err(PoolError {
                        error_code: PoolErrorCode::RequestFailed as u8,
                        error_message: e.to_string(),
                    })
                }
            },
            _ => {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                warn!(
                    "Failed to {method}, Bad Status Code: {:?}, {}",
                    &status, &text
                );
                Err(PoolError {
                    error_code: PoolErrorCode::RequestFailed as u8,
                    error_message: format!(
                        "Failed to {method}, Bad Status Code: {:?}, {}",
                        status, text
                    ),
                })
            }
        },
        Err(e) => {
            warn!("Failed to {method}: {:?}", e);
            Err(PoolError {
                error_code: PoolErrorCode::RequestFailed as u8,
                error_message: e.to_string(),
            })
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize, Debug)]
pub struct PoolLoginParts {
    pub auth_token: u64,
    pub aggregate_signature: String,
}

pub async fn create_pool_login_url(
    target_pool: &str,
    keys_and_launcher_ids: &[(SecretKey, Bytes32)],
) -> Result<String, Error> {
    let parts = create_pool_login_parts(target_pool, keys_and_launcher_ids).await?;
    let mut ids = String::new();
    for (index, (_, launcher_id)) in keys_and_launcher_ids.iter().enumerate() {
        if index != 0 {
            ids.push(',')
        }
        ids.push_str(&hex::encode(launcher_id.as_slice()));
    }
    Ok(format!(
        "{target_pool}/login?launcher_id={ids}&authentication_token={}&signature={})",
        parts.auth_token, parts.aggregate_signature
    ))
}

pub async fn create_pool_login_parts(
    target_pool: &str,
    keys_and_launcher_ids: &[(SecretKey, Bytes32)],
) -> Result<PoolLoginParts, Error> {
    let pool_client = DefaultPoolClient::new();
    let pool_info = pool_client
        .get_pool_info(target_pool)
        .await
        .map_err(|e| Error::new(ErrorKind::Other, format!("{:?}", e)))?;
    let current_auth_token =
        get_current_authentication_token(pool_info.authentication_token_timeout);
    let mut sigs = vec![];
    for (sec_key, launcher_id) in keys_and_launcher_ids {
        let payload = AuthenticationPayload {
            method_name: String::from("get_login"),
            launcher_id: *launcher_id,
            target_puzzle_hash: pool_info.target_puzzle_hash,
            authentication_token: current_auth_token,
        };
        let to_sign = hash_256(payload.to_bytes(ChiaProtocolVersion::default()));
        let sig = sign(sec_key, &to_sign);
        sigs.push(sig);
    }
    if !sigs.is_empty() {
        let aggregate_signature =
            AggregateSignature::aggregate(sigs.iter().collect::<Vec<&Signature>>().as_ref(), true)
                .map_err(|e| {
                    Error::new(
                        ErrorKind::InvalidInput,
                        format!("Failed to calculate signature: {:?}", e),
                    )
                })?;
        Ok(PoolLoginParts {
            auth_token: current_auth_token,
            aggregate_signature: hex::encode(aggregate_signature.to_signature().to_bytes()),
        })
    } else {
        Err(Error::new(
            ErrorKind::NotFound,
            "No Launcher IDs with Keys found",
        ))
    }
}
