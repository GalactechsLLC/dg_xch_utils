use super::*;

pub const PATH: &str = "/get_all_mempool_items";
#[portfu::prelude::post("/get_all_mempool_items", client_trust = "rpc-clients")]
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
        let out = {
            // The map is keyed by PLAIN-hex tx id.
            let mempool = node.mempool.lock().await;
            let mut m = Map::new();
            for item in mempool
                .items_by_fee()
                .iter()
                .map(|item| mempool_item_json(item))
            {
                m.insert(plain_hex(&item.spend_bundle_name), to_value(&item)?);
            }
            obj_with("mempool_items", Value::Object(m))
        };
        Ok(Some(out))
    })
    .await
}
