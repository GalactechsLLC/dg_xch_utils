use super::*;

pub const PATH: &str = "/get_mempool_item_by_tx_id";
#[portfu::prelude::post("/get_mempool_item_by_tx_id")]
pub async fn route(
    node: State<Node>,
    connection: ConnectionInfo,
    request: &mut Request,
) -> Result<Response, PortfuError> {
    check_access_policy!(
        connection,
        RpcAccessPolicy::PrivateCa | RpcAccessPolicy::Loopback
    );
    serve_portfu(node, request, |node, body| async move {
        let node = node.as_ref();
        let body = body.as_slice();
        let out = {
            let req: MempoolItemByTxIdRequest = parse(body)?;
            let mempool = node.mempool.lock().await;
            let item = mempool
                .get(&req.tx_id)
                .map(mempool_item_json)
                .ok_or_else(|| {
                    RpcError::BadRequest(format!("Tx id {} not in the mempool", req.tx_id))
                })?;
            envelope("mempool_item", &item)?
        };
        Ok(Some(out))
    })
    .await
}
