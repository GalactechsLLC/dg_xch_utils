use super::*;

pub const PATH: &str = "/healthz";
#[portfu::prelude::post("/healthz", client_trust = "rpc-clients")]
pub async fn route(
    node: State<Node>,
    connection: ConnectionInfo,
    request: &mut Request,
) -> Result<Response, PortfuError> {
    check_access_policy!(
        connection,
        RpcAccessPolicy::PrivateCa | RpcAccessPolicy::Loopback
    );
    serve_portfu(node, request, |_rpc, _body| async move {
        let out = Map::new();
        Ok(Some(out))
    })
    .await
}
