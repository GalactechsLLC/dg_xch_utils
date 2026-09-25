use crate::server::ActiveNode;
use portfu::prelude::{PortfuError, State, task};

/// Solicit compact VDFs when the optional uncompact mode is enabled.
#[task]
pub async fn uncompact_scanner(server: State<portfu::prelude::Server>) -> Result<(), PortfuError> {
    let Some(active) = super::node_state::<ActiveNode>(&server.0).await else {
        return Ok(());
    };
    active.0.run_uncompact_scanner().await;
    Ok(())
}
