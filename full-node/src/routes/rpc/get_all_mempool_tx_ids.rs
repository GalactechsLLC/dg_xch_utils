use super::*;

pub const PATH: &str = "/get_all_mempool_tx_ids";
#[portfu::prelude::post("/get_all_mempool_tx_ids")]
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
        let mempool = node.mempool.lock().await;
        let tx_ids = mempool
            .items_by_fee()
            .iter()
            .map(|item| item.name)
            .collect::<Vec<_>>();
        let out = envelope("tx_ids", &tx_ids)?;
        Ok(Some(out))
    })
    .await
}
