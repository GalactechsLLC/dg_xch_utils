use crate::server::NodeServices;
use log::info;
use portfu::prelude::{PortfuError, State, task};

/// Run the Chia P2P listener under Portfu's task lifecycle.
#[task]
pub async fn peer_listener(services: State<NodeServices>) -> Result<(), PortfuError> {
    info!("peer listener task starting");
    services
        .0
        .run_peer_listener()
        .await
        .map_err(|e| PortfuError::Internal(format!("peer listener failed: {e}")))
}
