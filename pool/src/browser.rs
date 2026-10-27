//! Browser sessions carry no authority to spend coins or change payout addresses.
use crate::{
    service::{PoolService, now},
    store::DashboardQuery,
};
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use portfu::prelude::{
    Request, Response,
    http::{HeaderValue, StatusCode},
};
use serde::Deserialize;
use serde_json::json;

const COOKIE: &str = "__Host-dgx_pool";
fn error(status: StatusCode, message: &str) -> Response {
    let mut response = Response::json(json!({"error":message}));
    *response.status_mut() = status;
    response
}
fn cookie(request: &Request) -> Option<&str> {
    let mut values = request
        .headers()
        .get("cookie")?
        .to_str()
        .ok()?
        .split(';')
        .filter_map(|part| part.trim().split_once('='))
        .filter(|(name, _)| *name == COOKIE)
        .map(|(_, value)| value);
    let value = values.next()?;
    if values.next().is_some() {
        None
    } else {
        Some(value)
    }
}
fn same_origin(request: &Request) -> bool {
    let host = request.headers().get("host").and_then(|v| v.to_str().ok());
    let origin = request
        .headers()
        .get("origin")
        .and_then(|v| v.to_str().ok());
    host.zip(origin)
        .is_some_and(|(host, origin)| origin == format!("https://{host}"))
}
fn set_cookie(response: &mut Response, value: &str, age: u64) -> Result<(), std::io::Error> {
    response.headers_mut().insert(
        "set-cookie",
        HeaderValue::from_str(&format!(
            "{COOKIE}={value}; Path=/; Secure; HttpOnly; SameSite=Strict; Max-Age={age}"
        ))
        .map_err(std::io::Error::other)?,
    );
    Ok(())
}

pub async fn dispatch(pool: &PoolService, request: &Request, body: &[u8]) -> Response {
    match handle(pool, request, body).await {
        Ok(response) => response,
        Err(error_value) => {
            log::warn!("Pool dashboard request failed: {error_value}");
            error(
                StatusCode::BAD_REQUEST,
                "Request could not be completed. Check your filters or generate a fresh login link.",
            )
        }
    }
}
async fn handle(
    pool: &PoolService,
    request: &Request,
    body: &[u8],
) -> Result<Response, std::io::Error> {
    let path = request.uri().path();
    let method = request.method().as_str();
    let now = now().map_err(|_| std::io::Error::other("clock unavailable"))?;
    if method == "POST" && !same_origin(request) {
        return Ok(error(
            StatusCode::FORBIDDEN,
            "Same-origin browser request required",
        ));
    }
    if method == "POST" && path == "/api/session" {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Exchange {
            ticket: String,
        }
        let exchange: Exchange = serde_json::from_slice(body).map_err(std::io::Error::other)?;
        let result = pool
            .store
            .lock()
            .await
            .exchange_browser_ticket(&exchange.ticket, now)
            .await;
        let Ok((token, session)) = result else {
            return Ok(error(
                StatusCode::UNAUTHORIZED,
                "Login link is expired or already used",
            ));
        };
        let mut response = Response::json(json!({"csrf":session.csrf,"expires":session.expires}));
        set_cookie(&mut response, &token, session.expires.saturating_sub(now))?;
        return Ok(response);
    }
    let session = match cookie(request) {
        Some(value) => pool
            .store
            .lock()
            .await
            .browser_session(value, now)
            .await
            .ok()
            .flatten(),
        None => None,
    };
    if path == "/api/account" || path.starts_with("/api/account/") || path == "/api/logout" {
        let Some(session) = session.as_ref() else {
            return Ok(error(StatusCode::UNAUTHORIZED, "PlotNFT login required"));
        };
        if method == "POST"
            && request
                .headers()
                .get("x-csrf-token")
                .and_then(|v| v.to_str().ok())
                != Some(session.csrf.as_str())
        {
            return Ok(error(
                StatusCode::FORBIDDEN,
                "Invalid session verification token",
            ));
        }
        if method == "GET" && path == "/api/account" {
            return Ok(Response::json(
                json!({"csrf":session.csrf,"expires":session.expires,"farmers":pool.store.lock().await.account_farmers(session).await?}),
            ));
        }
        if method == "POST" && path == "/api/logout" {
            if let Some(value) = cookie(request) {
                pool.store.lock().await.logout_browser(value).await?;
            }
            let mut response = Response::json(json!({"logged_out":true}));
            set_cookie(&mut response, "", 0)?;
            return Ok(response);
        }
        if method == "POST" && path == "/api/account/name" {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Name {
                launcher_id: Bytes32,
                name: String,
            }
            let name: Name = serde_json::from_slice(body).map_err(std::io::Error::other)?;
            pool.store
                .lock()
                .await
                .name_account_farmer(session, name.launcher_id, &name.name)
                .await?;
            return Ok(Response::json(json!({"saved":true})));
        }
    }
    if method == "GET" {
        let kind = path.strip_prefix("/api/").unwrap_or("");
        if matches!(
            kind,
            "overview"
                | "farmers"
                | "leaderboards"
                | "blocks"
                | "payouts"
                | "account/statistics"
                | "account/partials"
                | "account/harvesters"
                | "account/blocks"
                | "account/payouts"
        ) {
            let query: DashboardQuery =
                serde_urlencoded::from_str(request.uri().query().unwrap_or_default())
                    .map_err(std::io::Error::other)?;
            let value = pool
                .store
                .lock()
                .await
                .dashboard(kind, &query, session.as_ref(), now)
                .await?;
            return Ok(Response::json(value));
        }
    }
    Ok(error(StatusCode::NOT_FOUND, "Unknown dashboard endpoint"))
}

pub fn secure(mut response: Response) -> Response {
    for (name, value) in [
        ("cache-control", "no-store"),
        ("referrer-policy", "no-referrer"),
        ("x-content-type-options", "nosniff"),
        ("x-frame-options", "DENY"),
        (
            "content-security-policy",
            "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'",
        ),
    ] {
        response
            .headers_mut()
            .insert(name, HeaderValue::from_static(value));
    }
    response
}

impl PoolService {
    async fn verify_bls_browser_owner(
        &self,
        launcher: Bytes32,
        authorization: &dg_xch_core::protocols::pool_web::BlsAuthorization,
        operation: &str,
        proof: &dg_xch_core::protocols::pool_web::BrowserProof,
    ) -> Result<(), dg_xch_core::protocols::pool::PoolError> {
        use crate::service::{failure, storage_error};
        use dg_xch_core::protocols::{pool::PoolErrorCode, pool_web};
        let message = pool_web::bls_message(
            self.chain.genesis(),
            self.info.target_puzzle_hash,
            operation,
            proof,
        )
        .map_err(|e| failure(PoolErrorCode::InvalidSignature, e.to_string()))?;
        crate::authentication::verify(authorization.public_key, &message, authorization.signature)
            .map_err(|e| failure(PoolErrorCode::InvalidSignature, e.to_string()))?;
        let farmer = self
            .store
            .lock()
            .await
            .farmer(launcher)
            .await
            .map_err(storage_error)?
            .ok_or_else(|| failure(PoolErrorCode::FarmerNotKnown, "farmer is not registered"))?;
        if farmer.owner_public_key != authorization.public_key
            || farmer.version == crate::store::PoolVersion::Falcon
        {
            return Err(failure(
                PoolErrorCode::InvalidSignature,
                "PlotNFT owner does not match registration",
            ));
        }
        let membership = self
            .chain
            .membership(farmer.version, launcher, farmer.authentication_public_key)
            .await
            .map_err(|e| failure(PoolErrorCode::InvalidSingleton, e.to_string()))?;
        if membership.owner_public_key != farmer.owner_public_key
            || membership.contract_puzzle_hash != farmer.contract_puzzle_hash
        {
            return Err(failure(
                PoolErrorCode::InvalidSingleton,
                "pool membership changed",
            ));
        }
        Ok(())
    }

    pub async fn browser_login_bls(
        &self,
        request: dg_xch_core::protocols::pool_web::BlsBrowserAuthorization,
    ) -> Result<
        dg_xch_core::protocols::pool_web::BrowserTicket,
        dg_xch_core::protocols::pool::PoolError,
    > {
        use crate::service::failure;
        use dg_xch_core::protocols::{
            pool::PoolErrorCode,
            pool_web::{self, BrowserAction},
        };
        if !self.chain.signature_profile().is_bls() {
            return Err(failure(
                PoolErrorCode::RequestFailed,
                "BLS browser authentication is unavailable on this chain",
            ));
        }
        let proof = &request.proof;
        pool_web::timestamp(proof.timestamp, now()?)
            .map_err(|e| failure(PoolErrorCode::InvalidAuthenticationToken, e.to_string()))?;
        self.verify_bls_browser_owner(proof.launcher_id, &request.owner, "web_login", proof)
            .await?;
        match (
            &proof.action,
            proof.linked_launcher_id,
            &request.linked_authorization,
        ) {
            (BrowserAction::Link, Some(other), Some(authorization))
                if other != proof.launcher_id =>
            {
                self.verify_bls_browser_owner(other, authorization, "web_link", proof)
                    .await?;
            }
            (BrowserAction::Login | BrowserAction::Unlink, None, None) => {}
            _ => {
                return Err(failure(
                    PoolErrorCode::InvalidSignature,
                    "both PlotNFT owners must authorize linking",
                ));
            }
        }
        self.store
            .lock()
            .await
            .issue_browser_ticket(proof, now()?)
            .await
            .map_err(|e| failure(PoolErrorCode::InvalidAuthenticationToken, e.to_string()))
    }
}
