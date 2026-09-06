use super::*;

pub const PATH: &str = "/get_aggsig_additional_data";
#[portfu::prelude::post("/get_aggsig_additional_data", client_trust = "rpc-clients")]
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
        let out = obj_with(
            "additional_data",
            Value::from(plain_hex(&node.constants.agg_sig_me_additional_data)),
        );
        Ok(Some(out))
    })
    .await
}
