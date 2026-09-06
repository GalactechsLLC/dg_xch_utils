use super::*;

pub const PATH: &str = "/get_block";
#[portfu::prelude::post("/get_block", client_trust = "rpc-clients")]
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
            let req: BlockRequest = parse(body)?;
            let block = node
                .store
                .get_block(&req.header_hash)
                .await?
                .ok_or_else(|| {
                    RpcError::BadRequest(format!("Block {} not found", plain_hex(&req.header_hash)))
                })?;
            envelope("block", &block)?
        };
        Ok(Some(out))
    })
    .await
}
