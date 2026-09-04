use super::*;

pub const PATH: &str = "/get_additions_and_removals";
#[portfu::prelude::post("/get_additions_and_removals")]
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
            let req: AdditionsAndRemovalsRequest = parse(body)?;
            let record = node
                .store
                .get_block_record(&req.header_hash)
                .await?
                .ok_or_else(|| {
                    RpcError::BadRequest(format!("Block {} not found", plain_hex(&req.header_hash)))
                })?;
            let confirmed = node.store.get_block_record_by_height(record.height).await?;
            if confirmed.map(|record| record.header_hash) != Some(req.header_hash) {
                return Err(RpcError::BadRequest(format!(
                    "Block at {} is no longer in the blockchain (it's in a fork)",
                    plain_hex(&req.header_hash)
                )));
            }
            let additions = node.store.get_coins_added_at_height(record.height).await?;
            let removals = node
                .store
                .get_coins_removed_at_height(record.height)
                .await?;
            match to_value(&AdditionsAndRemovals {
                additions,
                removals,
            })? {
                Value::Object(response) => response,
                _ => Map::new(),
            }
        };
        Ok(Some(out))
    })
    .await
}
