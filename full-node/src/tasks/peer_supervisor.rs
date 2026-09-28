use crate::server::NodeServices;
use log::info;
use portfu::prelude::{PortfuError, State, task};

/// Own outbound peer slots and introducer refresh under Portfu's task lifecycle.
#[task]
pub async fn peer_supervisor(server: State<portfu::prelude::Server>) -> Result<(), PortfuError> {
    let Some(services) = super::node_state::<NodeServices>(&server.0).await else {
        return Ok(());
    };
    info!("peer supervisor task starting");
    services.0.run_peer_supervisor().await;
    info!("peer supervisor task stopped");
    Ok(())
}
