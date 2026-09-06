use crate::server::ActiveNode;
use portfu::prelude::{PortfuError, State, task};

/// Validate queued peer transactions outside websocket request handling.
#[task]
pub async fn tx_validator(active: State<ActiveNode>) -> Result<(), PortfuError> {
    active.0.run_tx_validator().await;
    Ok(())
}
