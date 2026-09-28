use crate::server::{ActiveNode, NodeServices};
use log::info;
use portfu::prelude::{PortfuError, State, interval};
use std::sync::atomic::Ordering;

/// Propagate Portfu shutdown into the node's protocol services.
#[interval(500)]
pub async fn shutdown(server: State<portfu::prelude::Server>) -> Result<(), PortfuError> {
    let Some(active) = super::node_state::<ActiveNode>(&server.0).await else {
        return Ok(());
    };
    let Some(services) = super::node_state::<NodeServices>(&server.0).await else {
        return Ok(());
    };
    if !server.0.run.load(Ordering::Relaxed) && active.0.is_running() {
        info!("server shutdown observed; draining the node");
        services.0.begin_shutdown();
        active.0.shutdown().await;
    }
    Ok(())
}
