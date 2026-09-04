use super::*;

pub const PATH: &str = "/get_block_spends";
#[portfu::prelude::post("/get_block_spends")]
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
            let spends = if block.transactions_generator.is_none() {
                Vec::new()
            } else {
                let input =
                    generator_input_for_block(node.store.as_ref(), &node.constants, &block).await?;
                tokio::task::spawn_blocking(move || coin_spends_from_generator(&input))
                    .await
                    .map_err(|error| {
                        RpcError::BadRequest(format!("spends worker panicked: {error:?}"))
                    })?
                    .map_err(|error| {
                        RpcError::BadRequest(format!("Failed to get spends for block: {error:?}"))
                    })?
            };
            envelope("block_spends", &spends)?
        };
        Ok(Some(out))
    })
    .await
}
