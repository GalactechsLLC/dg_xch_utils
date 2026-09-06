use crate::server::{ActiveNode, NodeServices};
use portfu::prelude::{PortfuError, State, interval};

/// Remove wallet subscriptions for disconnected peers.
#[interval(30_000)]
pub async fn wallet_subscription_reaper(
    active: State<ActiveNode>,
    services: State<NodeServices>,
) -> Result<(), PortfuError> {
    active.0.reap_wallet_subscriptions(&services.0).await;
    Ok(())
}
