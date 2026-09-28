use crate::server::{ActiveNode, NodeServices};
use portfu::prelude::{PortfuError, State, interval};

/// Remove wallet subscriptions for disconnected peers.
#[interval(30_000)]
pub async fn wallet_subscription_reaper(
    server: State<portfu::prelude::Server>,
) -> Result<(), PortfuError> {
    let Some(active) = super::node_state::<ActiveNode>(&server.0).await else {
        return Ok(());
    };
    let Some(services) = super::node_state::<NodeServices>(&server.0).await else {
        return Ok(());
    };
    active.0.reap_wallet_subscriptions(&services.0).await;
    Ok(())
}
