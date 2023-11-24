use crate::protocols::pool::{
    GetFarmerRequest, GetFarmerResponse, GetPoolInfoResponse, PoolError, PoolErrorCode,
    PostFarmerRequest, PostFarmerResponse, PostPartialRequest, PostPartialResponse,
    PutFarmerRequest, PutFarmerResponse,
};
use async_trait::async_trait;
use reqwest::{Client, RequestBuilder};
use std::collections::HashMap;
use log::warn;
use serde::de::DeserializeOwned;
use serde::Serialize;
use crate::api::{RequestMode};

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
        send_request(self.client.get(format!("{}/farmer", url)), "get_farmer", headers, RequestMode::Query(request)).await
    }

    async fn post_farmer(
        &self,
        url: &str,
        request: PostFarmerRequest,
        headers: &Option<HashMap<String, String>>,
    ) -> Result<PostFarmerResponse, PoolError> {
        send_request(self.client.post(format!("{}/farmer", url)), "post_farmer", headers, RequestMode::Json(request)).await
    }

    async fn put_farmer(
        &self,
        url: &str,
        request: PutFarmerRequest,
        headers: &Option<HashMap<String, String>>,
    ) -> Result<PutFarmerResponse, PoolError> {
        send_request(self.client.put(format!("{}/farmer", url)), "put_farmer", headers, RequestMode::Json(request)).await
    }

    async fn post_partial(
        &self,
        url: &str,
        request: PostPartialRequest,
        headers: &Option<HashMap<String, String>>,
    ) -> Result<PostPartialResponse, PoolError> {
        send_request(self.client.post(format!("{}/partial", url)), "post_partial", headers, RequestMode::Json(request)).await
    }
    async fn get_pool_info(&self, pool_url: &str) -> Result<GetPoolInfoResponse, PoolError> {
        send_request(self.client.get(format!("{}/pool_info", pool_url)), "get_pool_info", &None, RequestMode::<()>::Send).await
    }
}

async fn send_request<T: Serialize, R: DeserializeOwned>(mut request_builder: RequestBuilder, method: &str, headers: &Option<HashMap<String, String>>, mode: RequestMode<T>) -> Result<R, PoolError> {
    if let Some(headers) = headers {
        for (k, v) in headers {
            request_builder = request_builder.header(k, v);
        }
    }
    let future = match mode {
        RequestMode::Json(t) => {
            request_builder.json(&t).send()
        }
        RequestMode::Query(t) => {
            request_builder.query(&t).send()
        }
        RequestMode::Send => {
            request_builder.send()
        }
    };
    match future.await {
        Ok(resp) => match resp.status() {
            reqwest::StatusCode::OK => match resp.text().await {
                Ok(body) => match serde_json::from_str::<PoolError>(body.as_str()) {
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