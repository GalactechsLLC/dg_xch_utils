use crate::server::ActiveNode;
use portfu::prelude::{PortfuError, State, task};

/// Build and return queued weight proofs outside websocket request handling.
#[task]
pub async fn weight_proof_worker(active: State<ActiveNode>) -> Result<(), PortfuError> {
    active.0.run_weight_proof_worker().await;
    Ok(())
}
