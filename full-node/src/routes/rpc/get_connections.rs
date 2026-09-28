use super::*;

pub const PATH: &str = "/get_connections";
#[portfu::prelude::post("/get_connections", client_trust = "rpc-clients")]
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
            let req: ConnectionsRequest = parse_or_default(body)?;
            let mut connections = Vec::new();
            if let Some(live) = node.live.get() {
                let peers = live.inbound_peers.read().await;
                for (peer_id, peer) in peers.iter() {
                    let peer_type = *peer.node_type.read().await as u8;
                    if req
                        .node_type
                        .is_some_and(|node_type| node_type != peer_type)
                    {
                        continue;
                    }
                    let mut connection = Map::new();
                    connection.insert("type".to_string(), Value::from(peer_type));
                    connection.insert("local_port".to_string(), Value::from(live.local_port));
                    connection.insert("peer_host".to_string(), Value::from(""));
                    connection.insert("peer_port".to_string(), Value::from(0));
                    connection.insert("peer_server_port".to_string(), Value::from(0));
                    connection.insert("node_id".to_string(), Value::from(peer_id.to_string()));
                    connection.insert("creation_time".to_string(), Value::from(0));
                    connection.insert("bytes_read".to_string(), Value::from(0));
                    connection.insert("bytes_written".to_string(), Value::from(0));
                    connection.insert("last_message_time".to_string(), Value::from(0));
                    connections.push(connection);
                }
            }
            obj_with(
                "connections",
                Value::from(
                    connections
                        .into_iter()
                        .map(Value::Object)
                        .collect::<Vec<_>>(),
                ),
            )
        };
        Ok(Some(out))
    })
    .await
}
