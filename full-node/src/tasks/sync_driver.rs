use crate::server::{ActiveNode, NodeServices};
use log::info;
use portfu::prelude::{PortfuError, State, task};

/// Drive batch and bulk synchronization.
#[task]
pub async fn sync_driver(server: State<portfu::prelude::Server>) -> Result<(), PortfuError> {
    let Some(active) = super::node_state::<ActiveNode>(&server.0).await else {
        return Ok(());
    };
    let Some(services) = super::node_state::<NodeServices>(&server.0).await else {
        return Ok(());
    };
    info!("sync driver task starting");
    active.0.run_sync_driver(&services.0).await;
    info!("sync driver task stopped");
    Ok(())
}
