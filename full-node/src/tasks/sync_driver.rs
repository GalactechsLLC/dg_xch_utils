use crate::server::{ActiveNode, NodeServices};
use log::info;
use portfu::prelude::{PortfuError, State, task};

/// Drive batch and bulk synchronization.
#[task]
pub async fn sync_driver(
    active: State<ActiveNode>,
    services: State<NodeServices>,
) -> Result<(), PortfuError> {
    info!("sync driver task starting");
    active.0.run_sync_driver(&services.0).await;
    info!("sync driver task stopped");
    Ok(())
}
