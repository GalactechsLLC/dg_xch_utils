use crate::protocols::pool::{
    GetFarmerRequest, GetFarmerResponse, GetPoolInfoResponse, PoolError, PoolErrorCode,
    PostFarmerRequest, PostFarmerResponse, PostPartialRequest, PostPartialResponse,
    PutFarmerRequest, PutFarmerResponse,
};
use async_trait::async_trait;
use log::warn;
use reqwest::Client;

#[async_trait]
pub trait PoolClient {
    async fn get_farmer(
        &self,
        url: &str,
        request: GetFarmerRequest,
    ) -> Result<GetFarmerResponse, PoolError>;
    async fn post_farmer(
        &self,
        url: &str,
        request: PostFarmerRequest,
    ) -> Result<PostFarmerResponse, PoolError>;
    async fn put_farmer(
        &self,
        url: &str,
        request: PutFarmerRequest,
    ) -> Result<PutFarmerResponse, PoolError>;
    async fn post_partial(
        &self,
        url: &str,
        request: PostPartialRequest,
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
    ) -> Result<GetFarmerResponse, PoolError> {
        match self
            .client
            .get(format!("{}/farmer", url))
            .query(&request)
            .send()
            .await
        {
            Ok(resp) => match resp.status() {
                reqwest::StatusCode::OK => match resp.text().await {
                    Ok(body) => {
                        let body_str = body.as_str();
                        match serde_json::from_str(body_str) {
                            Ok(c) => Ok(c),
                            Err(_) => match serde_json::from_str(body_str) {
                                Ok(e) => {
                                    warn!("Failed to Get Farmer: {:?}", e);
                                    Err(e)
                                }
                                Err(e) => {
                                    warn!(
                                        "Failed to parse farmer Error, Invalid Json: {:?}, {}",
                                        e, body_str
                                    );
                                    Err(PoolError {
                                        error_code: PoolErrorCode::RequestFailed as u8,
                                        error_message: e.to_string(),
                                    })
                                }
                            },
                        }
                    }
                    Err(e) => {
                        warn!("Failed to Get Farmer, Invalid Body: {:?}", e);
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
                        "Failed to Get Farmer, Bad Status Code: {:?}, {}",
                        status, &text
                    );
                    Err(PoolError {
                        error_code: PoolErrorCode::RequestFailed as u8,
                        error_message: format!(
                            "Failed to Get Farmer, Bad Status Code: {:?}, {}",
                            status, text
                        ),
                    })
                }
            },
            Err(e) => {
                warn!("Failed to send Get Farmer: {:?}", e);
                Err(PoolError {
                    error_code: PoolErrorCode::RequestFailed as u8,
                    error_message: e.to_string(),
                })
            }
        }
    }

    async fn post_farmer(
        &self,
        url: &str,
        request: PostFarmerRequest,
    ) -> Result<PostFarmerResponse, PoolError> {
        match self
            .client
            .post(format!("{}/farmer", url))
            .json(&request)
            .send()
            .await
        {
            Ok(resp) => match resp.status() {
                reqwest::StatusCode::OK => match resp.text().await {
                    Ok(body) => match serde_json::from_str(body.as_str()) {
                        Ok(c) => Ok(c),
                        Err(_) => match serde_json::from_str(body.as_str()) {
                            Ok(e) => Err(e),
                            Err(_) => match serde_json::from_str(&body) {
                                Ok(e) => {
                                    warn!("Failed to Post Farmer: {:?}", e);
                                    Err(e)
                                }
                                Err(e) => {
                                    warn!(
                                        "Failed to parse farmer Error, Invalid Json: {:?}, {}",
                                        e, body
                                    );
                                    Err(PoolError {
                                        error_code: PoolErrorCode::RequestFailed as u8,
                                        error_message: e.to_string(),
                                    })
                                }
                            },
                        },
                    },
                    Err(e) => {
                        warn!("Failed to Post Farmer, Invalid Body: {:?}", e);
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
                        "Failed to Post Farmer, Bad Status Code: {:?}, {}",
                        &status, &text
                    );
                    Err(PoolError {
                        error_code: PoolErrorCode::RequestFailed as u8,
                        error_message: format!(
                            "Failed to Post Farmer, Bad Status Code: {:?}, {}",
                            status, text
                        ),
                    })
                }
            },
            Err(e) => {
                warn!("Failed to send Post Farmer: {:?}", e);
                Err(PoolError {
                    error_code: PoolErrorCode::RequestFailed as u8,
                    error_message: e.to_string(),
                })
            }
        }
    }

    async fn put_farmer(
        &self,
        url: &str,
        request: PutFarmerRequest,
    ) -> Result<PutFarmerResponse, PoolError> {
        match self
            .client
            .put(format!("{}/farmer", url))
            .json(&request)
            .send()
            .await
        {
            Ok(resp) => match resp.status() {
                reqwest::StatusCode::OK => match resp.text().await {
                    Ok(body) => match serde_json::from_str(body.as_str()) {
                        Ok(c) => Ok(c),
                        Err(_) => match serde_json::from_str(&body) {
                            Ok(e) => Err(e),
                            Err(_) => match serde_json::from_str(&body) {
                                Ok(e) => {
                                    warn!("Failed to Put Farmer: {:?}", e);
                                    Err(e)
                                }
                                Err(e) => {
                                    warn!(
                                        "Failed to parse farmer Error, Invalid Json: {:?}, {}",
                                        e, body
                                    );
                                    Err(PoolError {
                                        error_code: PoolErrorCode::RequestFailed as u8,
                                        error_message: e.to_string(),
                                    })
                                }
                            },
                        },
                    },
                    Err(e) => {
                        warn!("Failed to Put Farmer, Invalid Body: {:?}", e);
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
                        "Failed to Put Farmer, Bad Status Code: {:?}, {}",
                        &status, &text
                    );
                    Err(PoolError {
                        error_code: PoolErrorCode::RequestFailed as u8,
                        error_message: format!(
                            "Failed to Put Farmer, Bad Status Code: {:?}, {}",
                            status, text
                        ),
                    })
                }
            },
            Err(e) => {
                warn!("Failed to send Put Farmer: {:?}", e);
                Err(PoolError {
                    error_code: PoolErrorCode::RequestFailed as u8,
                    error_message: e.to_string(),
                })
            }
        }
    }
    async fn post_partial(
        &self,
        url: &str,
        request: PostPartialRequest,
    ) -> Result<PostPartialResponse, PoolError> {
        match self
            .client
            .post(format!("{}/partial", url))
            .json(&request)
            .send()
            .await
        {
            Ok(resp) => match resp.status() {
                reqwest::StatusCode::OK => match resp.text().await {
                    Ok(body) => match serde_json::from_str(body.as_str()) {
                        Ok(c) => Ok(c),
                        Err(_) => match serde_json::from_str(body.as_str()) {
                            Ok(e) => Err(e),
                            Err(_) => match serde_json::from_str(&body) {
                                Ok(e) => {
                                    warn!("Failed to Post Partial: {:?}", e);
                                    Err(e)
                                }
                                Err(e) => {
                                    warn!(
                                        "Failed to parse partial Error, Invalid Json: {:?}, {}",
                                        e, body
                                    );
                                    Err(PoolError {
                                        error_code: PoolErrorCode::RequestFailed as u8,
                                        error_message: e.to_string(),
                                    })
                                }
                            },
                        },
                    },
                    Err(e) => {
                        warn!("Failed to Post Partial, Invalid Body: {:?}", e);
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
                        "Failed to Post Partial, Bad Status Code: {:?}, {}",
                        &status, &text
                    );
                    Err(PoolError {
                        error_code: PoolErrorCode::RequestFailed as u8,
                        error_message: format!(
                            "Failed to Post Partial, Bad Status Code: {:?}, {}",
                            status, text
                        ),
                    })
                }
            },
            Err(e) => {
                warn!("Failed to send Post Partial: {:?}", e);
                Err(PoolError {
                    error_code: PoolErrorCode::RequestFailed as u8,
                    error_message: e.to_string(),
                })
            }
        }
    }
    async fn get_pool_info(&self, pool_url: &str) -> Result<GetPoolInfoResponse, PoolError> {
        let resp = self
            .client
            .get(format!("{}/pool_info", pool_url))
            .send()
            .await
            .map_err(|e| {
                warn!("Failed to load Pool Info: {:?}", e);
                PoolError {
                    error_code: PoolErrorCode::RequestFailed as u8,
                    error_message: e.to_string(),
                }
            })?;
        match resp.status() {
            reqwest::StatusCode::OK => {
                let body = resp.text().await.map_err(|e| {
                    warn!("Failed to load Pool Info, Invalid Body: {:?}", e);
                    PoolError {
                        error_code: PoolErrorCode::RequestFailed as u8,
                        error_message: e.to_string(),
                    }
                })?;
                match serde_json::from_str(body.as_str()) {
                    Ok(i) => Ok(i),
                    Err(_) => match serde_json::from_str(&body) {
                        Ok(e) => {
                            warn!("Failed to load Pool Info: {:?}", e);
                            Err(e)
                        }
                        Err(e) => {
                            warn!(
                                "Failed to parse pool info Error, Invalid Json: {:?}, {}",
                                e, body
                            );
                            Err(PoolError {
                                error_code: PoolErrorCode::RequestFailed as u8,
                                error_message: e.to_string(),
                            })
                        }
                    },
                }
            }
            _ => {
                let err = format!(
                    "Failed to load Pool Info, Bad Status Code: {:?}, {}",
                    resp.status(),
                    resp.text().await.unwrap_or_default()
                );
                warn!("{err}");
                Err(PoolError {
                    error_code: PoolErrorCode::RequestFailed as u8,
                    error_message: err,
                })
            }
        }
    }
}
