use crate::server::ActiveNode;
use portfu::prelude::{PortfuError, State, task};

/// Validate queued peer transactions outside websocket request handling.
#[task]
pub async fn tx_validator(server: State<portfu::prelude::Server>) -> Result<(), PortfuError> {
    let Some(active) = super::node_state::<ActiveNode>(&server.0).await else {
        return Ok(());
    };
    active.0.run_tx_validator().await;
    Ok(())
}
