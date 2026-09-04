use super::*;

pub const PATH: &str = "/get_block_record";
#[portfu::prelude::post("/get_block_record")]
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
            let record = node
                .store
                .get_block_record(&req.header_hash)
                .await?
                .ok_or_else(|| {
                    RpcError::BadRequest(format!(
                        "Block {} does not exist",
                        plain_hex(&req.header_hash)
                    ))
                })?;
            envelope("block_record", &record)?
        };
        Ok(Some(out))
    })
    .await
}
