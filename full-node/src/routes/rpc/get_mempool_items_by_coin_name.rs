use super::*;

pub const PATH: &str = "/get_mempool_items_by_coin_name";
#[portfu::prelude::post("/get_mempool_items_by_coin_name", client_trust = "rpc-clients")]
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
            let req: MempoolItemByCoinNameRequest = parse(body)?;
            let mempool = node.mempool.lock().await;
            let items = mempool
                .items_by_fee()
                .iter()
                .filter(|item| item.removals.contains(&req.coin_name))
                .map(|item| mempool_item_json(item))
                .collect::<Vec<_>>();
            envelope("mempool_items", &items)?
        };
        Ok(Some(out))
    })
    .await
}
