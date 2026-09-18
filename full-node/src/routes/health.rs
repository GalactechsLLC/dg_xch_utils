use crate::server::ActiveNode;
use portfu::prelude::http::StatusCode;
use portfu::prelude::{PortfuError, Response, State, get};

#[get("/health")]
pub async fn health(active: State<ActiveNode>) -> Result<Response, PortfuError> {
    let (status, body) = active.0.health_check().await;
    let code = if status.starts_with("200") {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    Ok(Response::from_status_and_message(code, body).content_type("text/plain; charset=utf-8"))
}
