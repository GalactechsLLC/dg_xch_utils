use super::*;

pub const PATH: &str = "/get_unfinished_block_headers";
#[portfu::prelude::post("/get_unfinished_block_headers")]
pub async fn route(
    node: State<Node>,
    connection: ConnectionInfo,
    request: &mut Request,
) -> Result<Response, PortfuError> {
    check_access_policy!(
        connection,
        RpcAccessPolicy::PrivateCa | RpcAccessPolicy::Loopback
    );
    serve_portfu(node, request, |node, _body| async move {
        let node = node.as_ref();
        let headers = if let Some(live) = node.live.get() {
            if let Some((_, peak_height)) = node.store.get_peak().await? {
                let cache = live.unfinished.lock().await;
                cache
                    .blocks_at_height(peak_height)
                    .into_iter()
                    .map(|block| UnfinishedHeaderBlock {
                        finished_sub_slots: block.finished_sub_slots.clone(),
                        reward_chain_block: block.reward_chain_block.clone(),
                        challenge_chain_sp_proof: block.challenge_chain_sp_proof.clone(),
                        reward_chain_sp_proof: block.reward_chain_sp_proof.clone(),
                        foliage: block.foliage,
                        foliage_transaction_block: block.foliage_transaction_block,
                        transactions_filter: UnsizedBytes::new(Vec::new()),
                    })
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };
        let out = envelope("headers", &headers)?;
        Ok(Some(out))
    })
    .await
}
