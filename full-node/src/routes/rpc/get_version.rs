use super::*;

pub const PATH: &str = "/get_version";
#[portfu::prelude::post("/get_version")]
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
        let out = obj_with("version", Value::from(env!("CARGO_PKG_VERSION")));
        Ok(Some(out))
    })
    .await
}
