use crate::server::{ActiveNode, NodeServices};
use log::info;
use portfu::prelude::{PortfuError, State, task};

/// Follow peer announcements while the node is near the chain tip.
#[task]
pub async fn tip_follower(server: State<portfu::prelude::Server>) -> Result<(), PortfuError> {
    let Some(active) = super::node_state::<ActiveNode>(&server.0).await else {
        return Ok(());
    };
    let Some(services) = super::node_state::<NodeServices>(&server.0).await else {
        return Ok(());
    };
    info!("tip follower task starting");
    active.0.run_tip_follower(&services.0).await;
    info!("tip follower task stopped");
    Ok(())
}
