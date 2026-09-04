use super::PROFILING;
use crate::metrics::{HEAP_DUMP_TIMEOUT, profiling};
use crate::server::ActiveNode;
use portfu::prelude::http::StatusCode;
use portfu::prelude::{PortfuError, Response, State, get};
use std::sync::atomic::Ordering;

#[get("/debug/heap", client_trust = "rpc-clients")]
pub async fn debug_heap(active: State<ActiveNode>) -> Result<Response, PortfuError> {
    if !active.0.debug_endpoints() {
        return Ok(Response::from_status_and_message(
            StatusCode::NOT_FOUND,
            "/debug/heap is disabled; start the node with --debug-endpoints to enable it",
        ));
    }
    if PROFILING.swap(true, Ordering::SeqCst) {
        return Ok(Response::from_status_and_message(
            StatusCode::SERVICE_UNAVAILABLE,
            "a debug profile is already running",
        ));
    }
    log::info!("heap-profile dump requested");
    let out = tokio::time::timeout(HEAP_DUMP_TIMEOUT, profiling::dump_heap_profile()).await;
    PROFILING.store(false, Ordering::SeqCst);
    match out {
        Ok(Ok(prof)) => {
            log::info!("heap profile dumped bytes={}", prof.len());
            Ok(Response::from_status_and_message(StatusCode::OK, prof)
                .content_type("application/octet-stream"))
        }
        Ok(Err(e)) => Ok(Response::from_status_and_message(
            StatusCode::INTERNAL_SERVER_ERROR,
            e,
        )),
        Err(_) => Ok(Response::from_status_and_message(
            StatusCode::GATEWAY_TIMEOUT,
            "heap dump timed out",
        )),
    }
}
