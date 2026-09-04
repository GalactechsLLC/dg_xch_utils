use super::*;

pub const PATH: &str = "/get_network_info";
#[portfu::prelude::post("/get_network_info")]
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
            let name = node.live.get().map_or_else(
                || {
                    if node.constants.genesis_challenge == MAINNET.genesis_challenge {
                        "mainnet".to_string()
                    } else {
                        "testnet".to_string()
                    }
                },
                |live| live.network_id.clone(),
            );
            let prefix = if name == "mainnet" { "xch" } else { "txch" };
            let mut m = Map::new();
            m.insert("network_name".to_string(), Value::from(name));
            m.insert("network_prefix".to_string(), Value::from(prefix));
            m.insert(
                "genesis_challenge".to_string(),
                Value::from(plain_hex(&node.constants.genesis_challenge)),
            );
            m
        };
        Ok(Some(out))
    })
    .await
}
