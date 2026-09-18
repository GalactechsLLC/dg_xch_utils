use crate::server::NodeServices;
use log::info;
use portfu::prelude::{PortfuError, State, task};

/// Own outbound peer slots and introducer refresh under Portfu's task lifecycle.
#[task]
pub async fn peer_supervisor(services: State<NodeServices>) -> Result<(), PortfuError> {
    info!("peer supervisor task starting");
    services.0.run_peer_supervisor().await;
    info!("peer supervisor task stopped");
    Ok(())
}
