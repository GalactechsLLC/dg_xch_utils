use super::*;

#[portfu::prelude::post("/get_auto_farming", client_trust = "rpc-clients")]
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
            let Some(sim) = node.sim.get() else {
                return Ok(None);
            };
            obj_with("auto_farm_enabled", Value::from(sim.auto_farming()))
        };
        Ok(Some(out))
    })
    .await
}
