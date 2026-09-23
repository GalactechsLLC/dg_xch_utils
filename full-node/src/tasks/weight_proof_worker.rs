use crate::server::ActiveNode;
use portfu::prelude::{PortfuError, State, task};

/// Build and return queued weight proofs outside websocket request handling.
#[task]
pub async fn weight_proof_worker(
    server: State<portfu::prelude::Server>,
) -> Result<(), PortfuError> {
    let Some(active) = super::node_state::<ActiveNode>(&server.0).await else {
        return Ok(());
    };
    active.0.run_weight_proof_worker().await;
    Ok(())
}
