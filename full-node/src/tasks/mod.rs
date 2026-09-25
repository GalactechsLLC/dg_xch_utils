//! Background work registered with Portfu.

mod peer_supervisor;
mod shutdown;
mod sync_driver;
mod tip_follower;
mod tx_validator;
mod uncompact_scanner;
mod wallet_subscription_reaper;
mod weight_proof_worker;

async fn node_state<Value: Send + Sync + 'static>(
    server: &portfu::prelude::Server,
) -> Option<portfu::prelude::State<Value>> {
    server
        .scoped_state
        .read()
        .await
        .get("default")?
        .get::<std::sync::Arc<Value>>()
        .cloned()
        .map(portfu::prelude::State)
}
