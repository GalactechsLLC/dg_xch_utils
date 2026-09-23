use crate::api::RequestMode;
use async_trait::async_trait;
use blst::min_pk::{AggregateSignature, SecretKey, Signature};
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::clvm::bls_bindings::sign;
use dg_xch_core::protocols::pool::{
    AuthenticationPayload, GetFarmerRequest, GetFarmerResponse, GetPoolInfoResponse, PoolError,
    PoolErrorCode, PostFarmerRequest, PostFarmerResponse, PostPartialRequest, PostPartialResponse,
    PutFarmerRequest, PutFarmerResponse, get_current_authentication_token,
};
use dg_xch_core::utils::hash_256;
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use log::{debug, warn};
use reqwest::{Client, RequestBuilder};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::collections::HashMap;
use std::fmt::Debug;
use std::io::{Error, ErrorKind};

#[async_trait]
pub trait PoolClient {
    fn from_configured_client(_client: DefaultPoolClient) -> Result<Self, Error>
    where
        Self: Sized,
    {
        Err(Error::other(
            "pool client does not support configured farmer construction",
        ))
    }
    async fn get_farmer<S: std::hash::BuildHasher + Sync + Send + 'static>(
        &self,
        url: &str,
        request: GetFarmerRequest,
        headers: &Option<HashMap<String, String, S>>,
    ) -> Result<GetFarmerResponse, PoolError>;
    async fn post_farmer<S: std::hash::BuildHasher + Sync + Send + 'static>(
        &self,
        url: &str,
        request: PostFarmerRequest,
        headers: &Option<HashMap<String, String, S>>,
    ) -> Result<PostFarmerResponse, PoolError>;
    async fn put_farmer<S: std::hash::BuildHasher + Sync + Send + 'static>(
        &self,
        url: &str,
        request: PutFarmerRequest,
        headers: &Option<HashMap<String, String, S>>,
    ) -> Result<PutFarmerResponse, PoolError>;
    async fn post_partial<S: std::hash::BuildHasher + Sync + Send + 'static>(
        &self,
        url: &str,
        request: PostPartialRequest,
        headers: &Option<HashMap<String, String, S>>,
    ) -> Result<PostPartialResponse, PoolError>;
    async fn get_pool_info(&self, pool_url: &str) -> Result<GetPoolInfoResponse, PoolError>;
}

#[derive(Default, Debug)]
pub struct DefaultPoolClient {
    pub client: Client,
    pub(super) v2_accounts: HashMap<Bytes32, super::pool_v2::V2Account>,
}
impl DefaultPoolClient {
    pub fn new() -> Result<Self, Error> {
        Self::with_ca_certificates(&[])
    }

    pub fn with_ca_certificates(certificates: &[Vec<u8>]) -> Result<Self, Error> {
        let mut builder = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(30));
        for certificate in certificates {
            if certificate.len() > 1024 * 1024 {
                return Err(Error::other("pool CA certificate exceeds 1 MiB"));
            }
            let roots = reqwest::Certificate::from_pem_bundle(certificate).map_err(Error::other)?;
            if roots.is_empty() {
                return Err(Error::other("pool CA file contains no certificates"));
            }
            for root in roots {
                builder = builder.add_root_certificate(root);
            }
        }
        Ok(Self {
            client: builder.build().map_err(Error::other)?,
            v2_accounts: HashMap::new(),
        })
    }
}
#[async_trait]
impl PoolClient for DefaultPoolClient {
    fn from_configured_client(client: DefaultPoolClient) -> Result<Self, Error> {
        Ok(client)
    }
    async fn get_farmer<S: std::hash::BuildHasher + Sync + Send + 'static>(
        &self,
        url: &str,
        request: GetFarmerRequest,
        headers: &Option<HashMap<String, String, S>>,
    ) -> Result<GetFarmerResponse, PoolError> {
        if let Some(account) = self.v2_account(url, request.launcher_id)? {
            let token = self.v2_token(request.launcher_id, account).await?;
            let result = self
                .get_farmer_v2(
                    &account.base,
                    dg_xch_core::protocols::pool_v2::GetFarmerRequest {
                        launcher_id: request.launcher_id,
                        authentication_token: 0,
                        authentication_token_v2: token,
                        signature: None,
                    },
                )
                .await;
            return self.v2_result(account, result).await;
        }
        send_request(
            self.client.get(format!("{url}/farmer")),
            "get_farmer",
            headers,
            RequestMode::Query(request),
        )
        .await
    }

    async fn post_farmer<S: std::hash::BuildHasher + Sync + Send + 'static>(
        &self,
        url: &str,
        request: PostFarmerRequest,
        headers: &Option<HashMap<String, String, S>>,
    ) -> Result<PostFarmerResponse, PoolError> {
        if let Some(account) = self.v2_account(url, request.payload.launcher_id)? {
            let mut payload = request.payload;
            if payload.authentication_public_key != account.key.sk_to_pk().to_bytes().into() {
                return Err(Error::other(
                    "v2 registration key differs from configured synthetic key",
                )
                .into());
            }
            payload.authentication_token = 0;
            let key = dg_xch_keys::derive_path_unhardened(&account.key, vec![12381])?;
            let signature = sign(
                &key,
                &hash_256(payload.to_bytes(ChiaProtocolVersion::Chia0_0_37)?),
            )
            .to_bytes()
            .into();
            return self
                .post_farmer_v2(&account.base, PostFarmerRequest { payload, signature })
                .await;
        }
        send_request(
            self.client.post(format!("{url}/farmer")),
            "post_farmer",
            headers,
            RequestMode::Json(request),
        )
        .await
    }

    async fn put_farmer<S: std::hash::BuildHasher + Sync + Send + 'static>(
        &self,
        url: &str,
        request: PutFarmerRequest,
        headers: &Option<HashMap<String, String, S>>,
    ) -> Result<PutFarmerResponse, PoolError> {
        if let Some(account) = self.v2_account(url, request.payload.launcher_id)? {
            let token = self.v2_token(request.payload.launcher_id, account).await?;
            let mut farmer = request.payload;
            farmer.authentication_token = 0;
            let result = self
                .put_farmer_v2(
                    &account.base,
                    dg_xch_core::protocols::pool_v2::PutFarmerRequest {
                        payload: dg_xch_core::protocols::pool_v2::UpdateFarmerPayload {
                            farmer,
                            authentication_token_v2: token,
                        },
                        signature: None,
                    },
                )
                .await;
            return self.v2_result(account, result).await;
        }
        send_request(
            self.client.put(format!("{url}/farmer")),
            "put_farmer",
            headers,
            RequestMode::Json(request),
        )
        .await
    }

    async fn post_partial<S: std::hash::BuildHasher + Sync + Send + 'static>(
        &self,
        url: &str,
        request: PostPartialRequest,
        headers: &Option<HashMap<String, String, S>>,
    ) -> Result<PostPartialResponse, PoolError> {
        if let Some(account) = self.v2_account(url, request.payload.launcher_id)? {
            if request.payload.authentication_token != 0 {
                return Err(
                    Error::other("v2 partial must be signed with a zero legacy token").into(),
                );
            }
            let token = self.v2_token(request.payload.launcher_id, account).await?;
            let headers: HashMap<String, String> = headers
                .as_ref()
                .map(|headers| {
                    headers
                        .iter()
                        .map(|(key, value)| (key.clone(), value.clone()))
                        .collect()
                })
                .unwrap_or_default();
            let result = self
                .post_partial_v2(
                    &account.base,
                    dg_xch_core::protocols::pool_v2::PostPartialRequest {
                        payload: request.payload,
                        aggregate_signature: request.aggregate_signature,
                        authentication_token_v2: token,
                    },
                    &headers,
                )
                .await;
            return self.v2_result(account, result).await;
        }
        send_request(
            self.client.post(format!("{url}/partial")),
            "post_partial",
            headers,
            RequestMode::Json(request),
        )
        .await
    }
    async fn get_pool_info(&self, pool_url: &str) -> Result<GetPoolInfoResponse, PoolError> {
        send_request(
            self.client.get(format!("{pool_url}/pool_info")),
            "get_pool_info",
            &None::<HashMap<String, String>>,
            RequestMode::<()>::Send,
        )
        .await
    }
}

pub(super) async fn send_request<
    T: Serialize + Debug,
    R: DeserializeOwned,
    S: std::hash::BuildHasher + Sync + Send + 'static,
>(
    mut request_builder: RequestBuilder,
    method: &str,
    headers: &Option<HashMap<String, String, S>>,
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
            let request = request.map_err(|e| PoolError {
                error_code: PoolErrorCode::RequestFailed as u8,
                error_message: e.to_string(),
            })?;
            debug!("Sending pool request: {method}");
            client.execute(request)
        }
        RequestMode::Query(t) => {
            let (client, request) = request_builder.query(&t).build_split();
            let request = request.map_err(|e| PoolError {
                error_code: PoolErrorCode::RequestFailed as u8,
                error_message: e.to_string(),
            })?;
            debug!("Sending pool request: {method}");
            client.execute(request)
        }
        RequestMode::Send => {
            let (client, request) = request_builder.build_split();
            let request = request.map_err(|e| PoolError {
                error_code: PoolErrorCode::RequestFailed as u8,
                error_message: e.to_string(),
            })?;
            debug!("Sending pool request: {method}");
            client.execute(request)
        }
    };
    match future.await {
        Ok(resp) => {
            if resp.status() == reqwest::StatusCode::OK {
                match crate::http::bounded_body(resp, 1024 * 1024).await {
                    Ok(body) => match serde_json::from_slice::<PoolError>(&body) {
                        Ok(e) => Err(e),
                        Err(_) => match serde_json::from_slice(&body) {
                            Ok(r) => Ok(r),
                            Err(e) => {
                                warn!("Failed to parse {method} response, Invalid Json: {e:?}");
                                Err(PoolError {
                                    error_code: PoolErrorCode::RequestFailed as u8,
                                    error_message: e.to_string(),
                                })
                            }
                        },
                    },
                    Err(e) => {
                        warn!("Failed to {method}, Invalid Body: {e:?}");
                        Err(PoolError {
                            error_code: PoolErrorCode::RequestFailed as u8,
                            error_message: e.to_string(),
                        })
                    }
                }
            } else {
                let status = resp.status();
                warn!("Failed to {method}, Bad Status Code: {status:?}");
                Err(PoolError {
                    error_code: PoolErrorCode::RequestFailed as u8,
                    error_message: format!("Failed to {method}, Bad Status Code: {status:?}"),
                })
            }
        }
        Err(e) => {
            let e = e.without_url();
            warn!("Failed to {method}: {e:?}");
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
            ids.push(',');
        }
        ids.push_str(&hex::encode(launcher_id));
    }
    Ok(format!(
        "{target_pool}/login?launcher_id={ids}&authentication_token={}&signature={}",
        parts.auth_token, parts.aggregate_signature
    ))
}

pub async fn create_pool_login_parts(
    target_pool: &str,
    keys_and_launcher_ids: &[(SecretKey, Bytes32)],
) -> Result<PoolLoginParts, Error> {
    let pool_client = DefaultPoolClient::new()?;
    let pool_info = pool_client
        .get_pool_info(target_pool)
        .await
        .map_err(|e| Error::other(format!("{e:?}")))?;
    let current_auth_token =
        get_current_authentication_token(pool_info.authentication_token_timeout)?;
    let mut sigs = vec![];
    for (sec_key, launcher_id) in keys_and_launcher_ids {
        let payload = AuthenticationPayload {
            method_name: String::from("get_login"),
            launcher_id: *launcher_id,
            target_puzzle_hash: pool_info.target_puzzle_hash,
            authentication_token: current_auth_token,
        };
        let to_sign = hash_256(&payload.to_bytes(ChiaProtocolVersion::default())?);
        let sig = sign(sec_key, &to_sign);
        sigs.push(sig);
    }
    if sigs.is_empty() {
        Err(Error::new(
            ErrorKind::NotFound,
            "No Launcher IDs with Keys found",
        ))
    } else {
        let aggregate_signature =
            AggregateSignature::aggregate(sigs.iter().collect::<Vec<&Signature>>().as_ref(), true)
                .map_err(|e| {
                    Error::new(
                        ErrorKind::InvalidInput,
                        format!("Failed to calculate signature: {e:?}"),
                    )
                })?;
        Ok(PoolLoginParts {
            auth_token: current_auth_token,
            aggregate_signature: hex::encode(aggregate_signature.to_signature().to_bytes()),
        })
    }
}
