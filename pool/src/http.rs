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
            info.protocol_version = 1;
            return Ok(Response::json(info));
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
