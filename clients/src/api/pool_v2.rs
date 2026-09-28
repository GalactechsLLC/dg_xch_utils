use super::RequestMode;
use super::pool::{DefaultPoolClient, send_request};
use blst::min_pk::SecretKey;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::clvm::bls_bindings::sign;
use dg_xch_core::protocols::{pool::*, pool_v2::*};
use dg_xch_keys::derive_path_unhardened;
use std::collections::HashMap;
use std::io::Error;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;

pub(super) struct V2Account {
    pub url: String,
    pub base: String,
    pub target: Bytes32,
    pub key: SecretKey,
    pub token: Mutex<Option<GetAuthResponse>>,
}

impl std::fmt::Debug for V2Account {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("V2Account")
            .field("url", &self.url)
            .finish_non_exhaustive()
    }
}

fn endpoint(base: &str, endpoint: &str) -> Result<String, PoolError> {
    let url = reqwest::Url::parse(base).map_err(|_| Error::other("invalid pool URL"))?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(Error::other(
            "pooling v2 requires an HTTPS URL without credentials, query or fragment",
        )
        .into());
    }
    Ok(format!("{}/v2/{endpoint}", base.trim_end_matches('/')))
}

pub fn login_request(
    launcher_id: Bytes32,
    target: Bytes32,
    timestamp: u64,
    synthetic_key: &SecretKey,
) -> Result<GetAuthRequest, Error> {
    let key = derive_path_unhardened(synthetic_key, vec![12381])?;
    let mut message = Vec::with_capacity(72);
    message.extend_from_slice(&timestamp.to_be_bytes());
    message.extend_from_slice(launcher_id.as_ref());
    message.extend_from_slice(target.as_ref());
    Ok(GetAuthRequest {
        launcher_id,
        timestamp,
        signature: sign(&key, &message).to_bytes().into(),
    })
}

impl DefaultPoolClient {
    pub fn add_v2_account(
        &mut self,
        url: &str,
        launcher: Bytes32,
        target: Bytes32,
        key: SecretKey,
    ) -> Result<(), Error> {
        let normalized = url.trim_end_matches('/');
        let base = normalized
            .strip_suffix("/v2")
            .ok_or_else(|| Error::other("v2 pool URL must end in /v2"))?;
        endpoint(base, "auth").map_err(|error| Error::other(error.error_message))?;
        if self.v2_accounts.contains_key(&launcher) {
            return Err(Error::other("duplicate v2 pool launcher"));
        }
        self.v2_accounts.insert(
            launcher,
            V2Account {
                url: normalized.into(),
                base: base.into(),
                target,
                key,
                token: Mutex::new(None),
            },
        );
        Ok(())
    }

    pub(super) fn v2_account(
        &self,
        url: &str,
        launcher: Bytes32,
    ) -> Result<Option<&V2Account>, PoolError> {
        let account = self.v2_accounts.get(&launcher);
        if account.is_some_and(|account| account.url != url.trim_end_matches('/')) {
            return Err(Error::other("v2 pool account URL mismatch").into());
        }
        Ok(account)
    }

    pub(super) async fn v2_token(
        &self,
        launcher: Bytes32,
        account: &V2Account,
    ) -> Result<String, PoolError> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(Error::other)?
            .as_secs();
        let mut cached = account.token.lock().await;
        if let Some(token) = cached
            .as_ref()
            .filter(|token| token.expiration.saturating_sub(now) > 30)
        {
            return Ok(token.authentication_token.clone());
        }
        let request = login_request(launcher, account.target, now, &account.key)?;
        let token = self.authenticate_v2(&account.base, request).await?;
        if token.expiration <= now
            || token.authentication_token.is_empty()
            || token.authentication_token.len() > 4096
        {
            return Err(Error::other("pool returned an invalid v2 authentication token").into());
        }
        let value = token.authentication_token.clone();
        *cached = Some(token);
        Ok(value)
    }

    pub(super) async fn v2_result<Response>(
        &self,
        account: &V2Account,
        response: Result<Response, PoolError>,
    ) -> Result<Response, PoolError> {
        if response
            .as_ref()
            .is_err_and(|error| error.error_code == PoolErrorCode::InvalidAuthenticationToken as u8)
        {
            *account.token.lock().await = None;
        }
        response
    }

    pub async fn get_pool_info_v2(&self, base: &str) -> Result<PoolInfo, PoolError> {
        send_request(
            self.client.get(endpoint(base, "pool_info")?),
            "pool_info_v2",
            &None::<HashMap<String, String>>,
            RequestMode::<()>::Send,
        )
        .await
    }

    pub async fn authenticate_v2(
        &self,
        base: &str,
        request: GetAuthRequest,
    ) -> Result<GetAuthResponse, PoolError> {
        send_request(
            self.client.get(endpoint(base, "auth")?),
            "auth_v2",
            &None::<HashMap<String, String>>,
            RequestMode::Query(request),
        )
        .await
    }

    pub async fn get_farmer_v2(
        &self,
        base: &str,
        request: dg_xch_core::protocols::pool_v2::GetFarmerRequest,
    ) -> Result<GetFarmerResponse, PoolError> {
        send_request(
            self.client.get(endpoint(base, "farmer")?),
            "get_farmer_v2",
            &None::<HashMap<String, String>>,
            RequestMode::Query(request),
        )
        .await
    }

    pub async fn post_farmer_v2(
        &self,
        base: &str,
        request: PostFarmerRequest,
    ) -> Result<PostFarmerResponse, PoolError> {
        send_request(
            self.client.post(endpoint(base, "farmer")?),
            "post_farmer_v2",
            &None::<HashMap<String, String>>,
            RequestMode::Json(request),
        )
        .await
    }

    pub async fn put_farmer_v2(
        &self,
        base: &str,
        request: dg_xch_core::protocols::pool_v2::PutFarmerRequest,
    ) -> Result<PutFarmerResponse, PoolError> {
        send_request(
            self.client.put(endpoint(base, "farmer")?),
            "put_farmer_v2",
            &None::<HashMap<String, String>>,
            RequestMode::Json(request),
        )
        .await
    }

    pub async fn post_partial_v2(
        &self,
        base: &str,
        request: dg_xch_core::protocols::pool_v2::PostPartialRequest,
        headers: &HashMap<String, String>,
    ) -> Result<PostPartialResponse, PoolError> {
        send_request(
            self.client.post(endpoint(base, "partial")?),
            "post_partial_v2",
            &Some(headers.clone()),
            RequestMode::Json(request),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_require_secure_unambiguous_urls() {
        assert_eq!(
            endpoint("https://pool.example/", "auth").unwrap(),
            "https://pool.example/v2/auth"
        );
        for url in [
            "http://pool.example",
            "https://user@pool.example",
            "https://pool.example?key=value",
            "https://pool.example#fragment",
        ] {
            assert!(endpoint(url, "auth").is_err());
        }
    }
}
