use crate::clients::protocols::pool::{
    GetFarmerRequest, GetFarmerResponse, PoolError, PoolErrorCode, PostFarmerRequest,
    PostFarmerResponse, PostPartialRequest, PostPartialResponse, PutFarmerRequest,
    PutFarmerResponse,
};
use log::{debug, warn};
use reqwest::Client;

#[derive(Default, Debug)]
pub struct PoolClient {
    client: Client,
}
impl PoolClient {
    pub fn new() -> Self {
        Self {
            client: Client::new(),
        }
    }

    pub async fn get_farmer(
        &self,
        url: &str,
        request: GetFarmerRequest,
    ) -> Result<GetFarmerResponse, PoolError> {
        debug!("Sending Request: {:?}", request);
        let query = self.client.get(format!("{}/farmer", url)).query(&request);
        debug!("Sending Built Request: {:?}", query);
        return match query.send().await {
            Ok(resp) => match resp.status() {
                reqwest::StatusCode::OK => match resp.text().await {
                    Ok(body) => {
                        let body_str = body.as_str();
                        match serde_json::from_str(body_str) {
                            Ok(c) => Ok(c),
                            Err(e) => {
                                warn!("Failed to Get Farmer: {:?}, {}", e, body_str);
                                match serde_json::from_str(body_str) {
                                    Ok(e) => Err(e),
                                    Err(e) => {
                                        warn!(
                                            "Failed to Get Farmer, Invalid Json: {:?}, {}",
                                            e, body_str
                                        );
                                        Err(PoolError {
                                            error_code: PoolErrorCode::RequestFailed as u8,
                                            error_message: e.to_string(),
                                        })
                                    }
                                }
                            }
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
                        &status, &text
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
                warn!("Failed to Get Farmer: {:?}", e);
                Err(PoolError {
                    error_code: PoolErrorCode::RequestFailed as u8,
                    error_message: e.to_string(),
                })
            }
        };
    }

    pub async fn post_farmer(
        &self,
        url: &str,
        request: PostFarmerRequest,
    ) -> Result<PostFarmerResponse, PoolError> {
        match Client::new()
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
                            Err(e) => {
                                warn!("Failed to Post Farmer, Invalid Json: {:?}, {}", e, body);
                                Err(PoolError {
                                    error_code: PoolErrorCode::RequestFailed as u8,
                                    error_message: e.to_string(),
                                })
                            }
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
                warn!("Failed to Post Farmer: {:?}", e);
                Err(PoolError {
                    error_code: PoolErrorCode::RequestFailed as u8,
                    error_message: e.to_string(),
                })
            }
        }
    }

    pub async fn put_farmer(
        &self,
        url: &str,
        request: PutFarmerRequest,
    ) -> Result<PutFarmerResponse, PoolError> {
        match Client::new()
            .put(format!("{}/farmer", url))
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
                            Err(e) => {
                                warn!("Failed to Put Farmer, Invalid Json: {:?}, {}", e, body);
                                Err(PoolError {
                                    error_code: PoolErrorCode::RequestFailed as u8,
                                    error_message: e.to_string(),
                                })
                            }
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
                warn!("Failed to Put Farmer: {:?}", e);
                Err(PoolError {
                    error_code: PoolErrorCode::RequestFailed as u8,
                    error_message: e.to_string(),
                })
            }
        }
    }
    pub async fn post_partial(
        &self,
        url: &str,
        request: PostPartialRequest,
    ) -> Result<PostPartialResponse, PoolError> {
        match Client::new()
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
                            Err(e) => {
                                warn!("Failed to Post Partial, Invalid Json: {:?}, {}", e, body);
                                Err(PoolError {
                                    error_code: PoolErrorCode::RequestFailed as u8,
                                    error_message: e.to_string(),
                                })
                            }
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
                warn!("Failed to Post Partial: {:?}", e);
                Err(PoolError {
                    error_code: PoolErrorCode::RequestFailed as u8,
                    error_message: e.to_string(),
                })
            }
        }
    }
}
