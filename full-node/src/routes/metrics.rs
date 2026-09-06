use crate::server::ActiveNode;
use portfu::prelude::{PortfuError, State, get};

#[get("/metrics")]
pub async fn metrics(active: State<ActiveNode>) -> Result<String, PortfuError> {
    Ok(active.0.metrics_text().await)
}
