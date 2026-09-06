use crate::server::{ActiveNode, NodeServices};
use log::info;
use portfu::prelude::{PortfuError, Server, State, interval};
use std::sync::atomic::Ordering;

/// Propagate Portfu shutdown into the node's protocol services.
#[interval(500)]
pub async fn shutdown(
    active: State<ActiveNode>,
    services: State<NodeServices>,
    server: State<Server>,
) -> Result<(), PortfuError> {
    if !server.0.run.load(Ordering::Relaxed) && active.0.is_running() {
        info!("server shutdown observed; draining the node");
        services.0.begin_shutdown();
        active.0.shutdown().await;
    }
    Ok(())
}
