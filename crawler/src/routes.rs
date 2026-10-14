use crate::{App, metrics as exposition};
use portfu::prelude::{PortfuError, Query, Response, State, http::StatusCode};
fn response(body: impl Into<String>, content_type: &'static str) -> Response {
    let mut response =
        Response::from_status_and_message(StatusCode::OK, body.into()).content_type(content_type);
    response.headers_mut().insert(
        "cache-control",
        portfu::prelude::http::HeaderValue::from_static("no-store"),
    );
    response.headers_mut().insert(
        "x-content-type-options",
        portfu::prelude::http::HeaderValue::from_static("nosniff"),
    );
    response.headers_mut().insert("content-security-policy", portfu::prelude::http::HeaderValue::from_static("default-src 'self'; script-src 'self'; style-src 'self'; connect-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'"));
    response
}
fn json(value: &impl serde::Serialize) -> Result<Response, PortfuError> {
    serde_json::to_string(value)
        .map(|body| response(body, "application/json"))
        .map_err(|e| PortfuError::Io(std::io::Error::other(e)))
}
pub async fn index() -> Result<Response, PortfuError> {
    Ok(response(
        include_str!("../static/index.html"),
        "text/html; charset=utf-8",
    ))
}
pub async fn javascript() -> Result<Response, PortfuError> {
    Ok(response(
        include_str!("../static/app.js"),
        "text/javascript; charset=utf-8",
    ))
}
pub async fn stylesheet() -> Result<Response, PortfuError> {
    Ok(response(
        include_str!("../static/style.css"),
        "text/css; charset=utf-8",
    ))
}
pub async fn stats(app: State<App>) -> Result<Response, PortfuError> {
    json(&app.0.summary().await.map_err(PortfuError::Io)?)
}
use dg_xch_servers::discovery::{Page, VersionPage, VersionQuery};
#[derive(serde::Serialize)]
struct VersionResponse {
    #[serde(flatten)]
    page: VersionPage,
    recent_window_seconds: u64,
}
pub async fn versions(
    app: State<App>,
    query: Query<VersionQuery>,
) -> Result<Response, PortfuError> {
    match app
        .0
        .store
        .version_page(app.0.config.recent_seconds, &query.0)
        .await
    {
        Ok(page) => json(&VersionResponse {
            page,
            recent_window_seconds: app.0.config.recent_seconds,
        }),
        Err(error) if error.kind() == std::io::ErrorKind::InvalidInput => Ok(
            Response::from_status_and_message(StatusCode::BAD_REQUEST, "invalid version page"),
        ),
        Err(error) => Err(PortfuError::Io(error)),
    }
}
pub async fn diagnostics(app: State<App>) -> Result<Response, PortfuError> {
    json(&app.0.store.diagnostics().await.map_err(PortfuError::Io)?)
}
pub async fn peers(app: State<App>, page: Query<Page>) -> Result<Response, PortfuError> {
    match app.0.store.page(&page.0).await {
        Ok(data) => json(&data),
        Err(error) if error.kind() == std::io::ErrorKind::InvalidInput => Ok(
            Response::from_status_and_message(StatusCode::BAD_REQUEST, "invalid page cursor"),
        ),
        Err(error) => Err(PortfuError::Io(error)),
    }
}
pub async fn history(app: State<App>) -> Result<Response, PortfuError> {
    json(&app.0.store.history().await.map_err(PortfuError::Io)?)
}
pub async fn metrics(app: State<App>) -> Result<Response, PortfuError> {
    let text = exposition::render(&app.0.summary().await.map_err(PortfuError::Io)?)
        .map_err(PortfuError::Io)?;
    Ok(response(text, "text/plain; version=0.0.4; charset=utf-8"))
}
pub async fn ready(app: State<App>) -> Result<Response, PortfuError> {
    let ready = app.0.live() && app.0.store.healthy().await;
    Ok(Response::from_status_and_message(
        if ready {
            StatusCode::OK
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        },
        if ready {
            "ready"
        } else {
            "backend or worker unavailable"
        },
    )
    .content_type("text/plain"))
}
pub async fn health(app: State<App>) -> Result<Response, PortfuError> {
    let healthy = app.0.live();
    Ok(Response::from_status_and_message(
        if healthy {
            StatusCode::OK
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        },
        if healthy { "ok" } else { "worker stalled" },
    )
    .content_type("text/plain"))
}

// Register this service explicitly rather than through process-wide endpoint inventory.
// The crawler owns its listener, including 404s, when linked beside the full node.
pub fn service(app: std::sync::Arc<App>) -> portfu::prelude::Service {
    portfu::prelude::ServiceBuilder::new("/*")
        .name("crawler")
        .handler(std::sync::Arc::new(Router(app)))
        .build()
}
struct Router(std::sync::Arc<App>);
impl portfu::prelude::ServiceTrait for Router {
    fn name(&self) -> &str {
        "crawler"
    }
    fn serve<'a>(
        &'a self,
        request: &'a mut portfu::prelude::Request,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Response, PortfuError>> + Send + 'a>,
    > {
        Box::pin(async move {
            use portfu::prelude::{FromRequest, IntoResponse, http::Method};
            if !matches!(*request.method(), Method::GET | Method::HEAD) {
                let mut result = Response::from_status_and_message(
                    StatusCode::METHOD_NOT_ALLOWED,
                    "read-only crawler endpoint",
                );
                result.headers_mut().insert(
                    "allow",
                    portfu::prelude::http::HeaderValue::from_static("GET, HEAD"),
                );
                return Ok(result);
            }
            if let Some(asset) = dg_xch_servers::web_theme::asset(request.uri().path()) {
                return Ok(asset);
            }
            let result = match request.uri().path() {
                "/" => index().await,
                "/app.js" => javascript().await,
                "/style.css" => stylesheet().await,
                "/api/stats" => stats(State(self.0.clone())).await,
                "/api/peers" => {
                    match <Query<Page> as FromRequest<portfu::prelude::Request>>::try_from(request)
                        .await
                    {
                        Ok(page) => peers(State(self.0.clone()), page).await,
                        Err(error) => Err(error),
                    }
                }
                "/api/history" => history(State(self.0.clone())).await,
                "/api/diagnostics" => diagnostics(State(self.0.clone())).await,
                "/api/versions" => match <Query<VersionQuery> as FromRequest<
                    portfu::prelude::Request,
                >>::try_from(request)
                .await
                {
                    Ok(query) => versions(State(self.0.clone()), query).await,
                    Err(error) => Err(error),
                },
                "/metrics" => metrics(State(self.0.clone())).await,
                "/health" => health(State(self.0.clone())).await,
                "/ready" => ready(State(self.0.clone())).await,
                _ => Ok(Response::not_found("unknown crawler endpoint")),
            };
            Ok(match result {
                Ok(response) => response,
                Err(error) => error.into_response(),
            })
        })
    }
}
