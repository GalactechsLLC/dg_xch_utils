use crate::server::{ActiveNode, NodeServices};
use log::info;
use portfu::prelude::{PortfuError, State, task};

/// Follow peer announcements while the node is near the chain tip.
#[task]
pub async fn tip_follower(
    active: State<ActiveNode>,
    services: State<NodeServices>,
) -> Result<(), PortfuError> {
    info!("tip follower task starting");
    active.0.run_tip_follower(&services.0).await;
    info!("tip follower task stopped");
    Ok(())
}
