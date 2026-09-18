use super::*;

pub const PATH: &str = "/get_network_space";
#[portfu::prelude::post("/get_network_space", client_trust = "rpc-clients")]
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
            let req: NetworkSpaceRequest = parse(body)?;
            if req.newer_block_header_hash == req.older_block_header_hash {
                return Err(RpcError::BadRequest(
                    "New and old must not be the same".to_string(),
                ));
            }
            let newer = node
                .store
                .get_block_record(&req.newer_block_header_hash)
                .await?
                .ok_or_else(|| {
                    RpcError::BadRequest(format!(
                        "Newer block {} not found",
                        plain_hex(&req.newer_block_header_hash)
                    ))
                })?;
            let older = node
                .store
                .get_block_record(&req.older_block_header_hash)
                .await?
                .ok_or_else(|| {
                    RpcError::BadRequest(format!(
                        "Older block {} not found",
                        plain_hex(&req.older_block_header_hash)
                    ))
                })?;
            let space =
                network_space_between(&node.constants, &older, &newer).ok_or_else(|| {
                    RpcError::BadRequest("blocks carry no iteration delta".to_string())
                })?;
            obj_with("space", json_u128(space))
        };
        Ok(Some(out))
    })
    .await
}
