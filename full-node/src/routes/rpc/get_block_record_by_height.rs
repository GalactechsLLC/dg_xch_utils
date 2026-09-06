use super::*;

pub const PATH: &str = "/get_block_record_by_height";
#[portfu::prelude::post("/get_block_record_by_height", client_trust = "rpc-clients")]
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
            let req: BlockRecordByHeightRequest = parse(body)?;
            let peak_height = node.store.get_peak().await?.map(|(_, height)| height);
            if peak_height.is_none_or(|height| req.height > height) {
                return Err(RpcError::BadRequest(format!(
                    "Block height {} not found in chain",
                    req.height
                )));
            }
            let record = node
                .store
                .get_block_record_by_height(req.height)
                .await?
                .ok_or_else(|| {
                    RpcError::BadRequest(format!("Block hash {} not found in chain", req.height))
                })?;
            envelope("block_record", &record)?
        };
        Ok(Some(out))
    })
    .await
}
