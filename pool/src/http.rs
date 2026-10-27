use crate::service::PoolService;
use crate::store::PoolVersion;
use dg_xch_core::protocols::{pool::PoolError, pool_v2};
use portfu::prelude::http::StatusCode;
use portfu::prelude::{PortfuError, Request, Response, Service, ServiceBuilder, ServiceTrait};
use serde::Serialize;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

struct PoolHttp(Arc<PoolService>);

pub fn service(pool: Arc<PoolService>) -> Service {
    ServiceBuilder::new("/*")
        .name("reference-pool")
        .handler(Arc::new(PoolHttp(pool)))
        .build()
}

fn result<T: Serialize>(result: Result<T, PoolError>) -> Response {
    match result {
        Ok(value) => Response::json(value),
        Err(error) => Response::json(error),
    }
}

fn invalid(message: &str) -> Response {
    result::<()>(Err(PoolError {
        error_code: 16,
        error_message: message.into(),
    }))
}

impl PoolHttp {
    async fn dispatch(&self, request: &mut Request) -> Result<Response, PortfuError> {
        let pool = &self.0;
        let path = request.uri().path().to_owned();
        let method = request.method().as_str().to_owned();
        if matches!(method.as_str(), "GET" | "HEAD") {
            if let Some(asset) = dg_xch_servers::web_theme::asset(&path) {
                return Ok(asset);
            }
            let asset = match path.as_str() {
                "/" | "/farmers" | "/leaderboards" | "/blocks" | "/payouts" | "/pool-status"
                | "/join" | "/fees" | "/login" | "/account" | "/api-help" => Some((
                    include_str!("../static/index.html"),
                    "text/html; charset=utf-8",
                )),
                "/pool.js" => Some((
                    include_str!("../static/pool.js"),
                    "text/javascript; charset=utf-8",
                )),
                "/pool.css" => Some((
                    include_str!("../static/pool.css"),
                    "text/css; charset=utf-8",
                )),
                _ => None,
            };
            if let Some((body, content_type)) = asset {
                return Ok(crate::browser::secure(
                    Response::ok(body).content_type(content_type),
                ));
            }
        }
        if matches!(method.as_str(), "GET" | "HEAD")
            && matches!(path.as_str(), "/health" | "/metrics")
        {
            let snapshot = pool.store.lock().await.stats().await;
            return Ok(match snapshot {
                Ok(_) if path == "/health" => {
                    Response::ok("pool database available").content_type("text/plain")
                }
                Ok(stats) => {
                    let text = dg_xch_servers::observability::gauges(
                        "chia_pool",
                        &[
                            (
                                "registered_farmers",
                                "Registered farmers in the pool database",
                                stats.farmers as f64,
                            ),
                            (
                                "accepted_partials",
                                "Retained accepted partials",
                                stats.accepted_partials as f64,
                            ),
                            (
                                "reward_claims",
                                "Retained reward claims",
                                stats.reward_claims as f64,
                            ),
                            (
                                "signed_payouts",
                                "Signed payout batches",
                                stats.signed_payouts as f64,
                            ),
                            (
                                "confirmed_payouts",
                                "Confirmed payout batches",
                                stats.confirmed_payouts as f64,
                            ),
                            ("storage_healthy", "Pool database readable", 1.0),
                        ],
                    )
                    .map_err(PortfuError::Io)?;
                    Response::ok(text).content_type("text/plain; version=0.0.4; charset=utf-8")
                }
                Err(_) => Response::from_status_and_message(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "pool database unavailable",
                ),
            });
        }
        if path.starts_with("/v2/") && !pool.enable_v2 {
            return Ok(Response::from_status_and_message(
                StatusCode::NOT_FOUND,
                "pooling v2 is disabled",
            ));
        }
        if request
            .uri()
            .query()
            .is_some_and(|query| query.len() > 4096)
        {
            return Ok(invalid("query exceeds size limit"));
        }
        let query = request.uri().query().unwrap_or_default().to_owned();
        if method == "GET" && path == "/pool_stats" {
            return Ok(match pool.store.lock().await.stats().await {
                Ok(stats) => Response::json(stats),
                Err(_) => Response::from_status_and_message(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "pool statistics unavailable",
                ),
            });
        }
        if method == "GET" && path == "/pool_info" {
            let mut info = pool.info.clone();
            info.protocol_version = if pool.chain.signature_profile().is_bls() {
                1
            } else {
                3
            };
            let mut info = serde_json::to_value(info)
                .map_err(|error| PortfuError::Io(std::io::Error::other(error)))?;
            info["currency_code"] = serde_json::json!(pool.chain.currency_code());
            info["chain_profile"] = serde_json::json!(pool.chain.profile_label());
            return Ok(Response::json(info));
        }
        if !pool.chain.signature_profile().is_bls()
            && (path.starts_with("/v2/") || matches!(path.as_str(), "/farmer" | "/partial"))
        {
            return Ok(invalid(
                "BLS pool protocol is disabled on this Falcon chain; use /falcon",
            ));
        }
        if method == "GET" && path == "/v2/pool_info" {
            let mut info = pool.info.clone();
            info.protocol_version = 2;
            info.fee = pool.fee_basis_points.to_string();
            return Ok(Response::json(pool_v2::PoolInfo {
                info,
                pool_memoization: pool.pool_memoization.clone(),
            }));
        }
        let body = if method == "POST" || method == "PUT" {
            request
                .consume_body_bytes_limited(128 * 1024, Duration::from_secs(5))
                .await?
                .to_vec()
        } else {
            Vec::new()
        };
        if path.starts_with("/api/") {
            return Ok(crate::browser::secure(
                crate::browser::dispatch(pool, request, &body).await,
            ));
        }
        macro_rules! from_body {
            () => {
                match serde_json::from_slice(&body) {
                    Ok(value) => value,
                    Err(_) => return Ok(invalid("invalid request body")),
                }
            };
        }
        macro_rules! from_query {
            () => {
                match serde_urlencoded::from_str(&query) {
                    Ok(value) => value,
                    Err(_) => return Ok(invalid("invalid query parameters")),
                }
            };
        }
        Ok(match (method.as_str(), path.as_str()) {
            ("POST", "/web/login") if pool.chain.signature_profile().is_bls() => {
                crate::browser::secure(result(pool.browser_login_bls(from_body!()).await))
            }
            ("POST", "/web/login" | "/falcon/web/login") => {
                crate::browser::secure(result(pool.browser_login(from_body!()).await))
            }
            ("POST", "/falcon/farmer") => result(pool.register_falcon(from_body!()).await),
            ("POST", "/falcon/farmer/info") => result(pool.get_falcon(from_body!()).await),
            ("POST", "/falcon/farmer/update") => result(pool.update_falcon(from_body!()).await),
            ("POST", "/falcon/partial") => result(pool.partial_falcon(from_body!()).await),
            ("GET", "/farmer") => result(pool.get_v1(from_query!()).await),
            ("POST", "/farmer") => result(pool.register(PoolVersion::V1, from_body!()).await),
            ("PUT", "/farmer") => result(pool.put_v1(from_body!()).await),
            ("POST", "/partial") => result(pool.partial_v1(from_body!()).await),
            ("GET", "/v2/auth") => result(pool.authenticate(from_query!()).await),
            ("GET", "/v2/farmer") => result(pool.get_v2(from_query!()).await),
            ("POST", "/v2/farmer") => result(pool.register(PoolVersion::V2, from_body!()).await),
            ("PUT", "/v2/farmer") => result(pool.put_v2(from_body!()).await),
            ("POST", "/v2/partial") => result(pool.partial_v2(from_body!()).await),
            _ => Response::from_status_and_message(StatusCode::NOT_FOUND, "unknown pool endpoint"),
        })
    }
}

impl ServiceTrait for PoolHttp {
    fn name(&self) -> &str {
        "reference-pool"
    }

    fn serve<'a>(
        &'a self,
        request: &'a mut Request,
    ) -> Pin<Box<dyn Future<Output = Result<Response, PortfuError>> + Send + 'a>> {
        Box::pin(async move {
            if matches!(request.method().as_str(), "GET" | "HEAD")
                && matches!(request.uri().path(), "/metrics" | "/health")
            {
                return tokio::time::timeout(Duration::from_secs(5), self.dispatch(request))
                    .await
                    .unwrap_or_else(|_| {
                        Ok(Response::from_status_and_message(
                            StatusCode::SERVICE_UNAVAILABLE,
                            "pool database check timed out",
                        ))
                    });
            }
            let Ok(_permit) = self.0.requests.try_acquire() else {
                return Ok(Response::from_status_and_message(
                    StatusCode::TOO_MANY_REQUESTS,
                    "pool is busy; retry later",
                ));
            };
            tokio::time::timeout(Duration::from_secs(30), self.dispatch(request))
                .await
                .unwrap_or_else(|_| {
                    Ok(Response::from_status_and_message(
                        StatusCode::GATEWAY_TIMEOUT,
                        "pool request timed out",
                    ))
                })
        })
    }
}
