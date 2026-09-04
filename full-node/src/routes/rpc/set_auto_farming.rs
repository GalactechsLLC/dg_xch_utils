use super::*;

#[portfu::prelude::post("/set_auto_farming", client_trust = "rpc-clients")]
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
            let Some(sim) = node.sim.get() else {
                return Ok(None);
            };
            let req: AutoFarmRequest = parse(body)?;
            obj_with(
                "auto_farm_enabled",
                Value::from(sim.set_auto_farming(req.auto_farm)),
            )
        };
        Ok(Some(out))
    })
    .await
}
