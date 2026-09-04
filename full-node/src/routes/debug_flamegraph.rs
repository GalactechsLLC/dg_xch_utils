use super::PROFILING;
use crate::metrics::{FLAMEGRAPH_SECONDS, FLAMEGRAPH_TIMEOUT, profiling};
use crate::server::ActiveNode;
use portfu::prelude::http::StatusCode;
use portfu::prelude::{PortfuError, Response, State, get};
use std::sync::atomic::Ordering;

#[get("/debug/flamegraph")]
pub async fn debug_flamegraph(active: State<ActiveNode>) -> Result<Response, PortfuError> {
    if !active.0.debug_endpoints() {
        return Ok(Response::from_status_and_message(
            StatusCode::NOT_FOUND,
            "/debug/flamegraph is disabled; start the node with --debug-endpoints to enable it",
        ));
    }
    if PROFILING.swap(true, Ordering::SeqCst) {
        return Ok(Response::from_status_and_message(
            StatusCode::SERVICE_UNAVAILABLE,
            "a debug profile is already running",
        ));
    }
    log::info!("flamegraph profiling started (~{FLAMEGRAPH_SECONDS}s)");
    let out = tokio::time::timeout(
        FLAMEGRAPH_TIMEOUT,
        profiling::sample_flamegraph(FLAMEGRAPH_SECONDS),
    )
    .await;
    PROFILING.store(false, Ordering::SeqCst);
    match out {
        Ok(Ok(svg)) => Ok(
            Response::from_status_and_message(StatusCode::OK, svg).content_type("image/svg+xml")
        ),
        Ok(Err(e)) => Ok(Response::from_status_and_message(
            StatusCode::INTERNAL_SERVER_ERROR,
            e,
        )),
        Err(_) => Ok(Response::from_status_and_message(
            StatusCode::GATEWAY_TIMEOUT,
            "flamegraph timed out",
        )),
    }
}
