use crate::server::ActiveNode;
use portfu::prelude::{PortfuError, State, task};

/// Solicit compact VDFs when the optional uncompact mode is enabled.
#[task]
pub async fn uncompact_scanner(active: State<ActiveNode>) -> Result<(), PortfuError> {
    active.0.run_uncompact_scanner().await;
    Ok(())
}
