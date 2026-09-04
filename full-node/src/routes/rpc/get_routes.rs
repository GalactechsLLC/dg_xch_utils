use super::*;

pub const PATH: &str = "/get_routes";
#[portfu::prelude::post("/get_routes")]
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
        let out = obj_with(
            "routes",
            Value::from(
                route_names()
                    .into_iter()
                    .map(Value::from)
                    .collect::<Vec<_>>(),
            ),
        );
        Ok(Some(out))
    })
    .await
}
