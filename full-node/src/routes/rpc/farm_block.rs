use super::*;

#[portfu::prelude::post("/farm_block")]
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
            let req: FarmBlockRequest = parse(body)?;
            let blocks = u32::try_from(req.blocks.max(0)).unwrap_or(u32::MAX);
            sim.farm_block(&req.address, blocks, req.guarantee_tx_block)
                .await
                .map_err(RpcError::BadRequest)?;
            Map::new()
        };
        Ok(Some(out))
    })
    .await
}
