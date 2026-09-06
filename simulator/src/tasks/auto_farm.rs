use crate::server::AutoFarmTaskState;
use portfu::prelude::{PortfuError, State, task};
use std::sync::atomic::Ordering;

#[task(scope = "simulator")]
pub async fn auto_farm(state: State<AutoFarmTaskState>) -> Result<(), PortfuError> {
    while state.0.node.run.load(Ordering::Relaxed) {
        tokio::time::sleep(state.0.interval).await;
        if !state.0.enabled.load(Ordering::Relaxed) {
            continue;
        }
        let mut chain = state.0.chain.lock().await;
        if chain
            .farm_next_from_shared_mempool(&state.0.node.mempool, false)
            .await
            .is_ok()
            && let Some(delta) = chain.take_last_delta()
        {
            let _ = state.0.node.notify_new_peak(&delta, None).await;
        }
    }
    Ok(())
}
