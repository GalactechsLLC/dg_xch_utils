use super::{
    RequestMode,
    pool::{DefaultPoolClient, send_request},
};
use dg_xch_core::protocols::pool::PoolError;
use serde::{Serialize, de::DeserializeOwned};
use std::{collections::HashMap, fmt::Debug, io::Error};

pub fn endpoint(base: &str, operation: &str) -> Result<String, Error> {
    super::pool::validate_pool_url(base)?;
    if !matches!(
        operation,
        "farmer" | "farmer/info" | "farmer/update" | "partial" | "web/login"
    ) {
        return Err(Error::other("invalid Falcon pool operation"));
    }
    Ok(format!("{}/falcon/{operation}", base.trim_end_matches('/')))
}

pub fn host(base: &str) -> Result<String, Error> {
    endpoint(base, "farmer")?;
    reqwest::Url::parse(base)
        .map_err(Error::other)?
        .host_str()
        .map(str::to_owned)
        .ok_or_else(|| Error::other("pool URL has no hostname"))
}

impl DefaultPoolClient {
    pub async fn falcon_request<T: Serialize + Debug, R: DeserializeOwned>(
        &self,
        base: &str,
        operation: &str,
        request: T,
    ) -> Result<R, PoolError> {
        let url = endpoint(base, operation)?;
        send_request(
            self.client.post(url),
            operation,
            &None::<HashMap<String, String>>,
            RequestMode::Json(request),
        )
        .await
    }
}
