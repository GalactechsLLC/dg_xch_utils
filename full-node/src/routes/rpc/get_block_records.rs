use super::*;

pub const PATH: &str = "/get_block_records";
#[portfu::prelude::post("/get_block_records", client_trust = "rpc-clients")]
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
            let req: BlockRecordsRequest = parse(body)?;
            if req.end.saturating_sub(req.start) > MAX_BLOCK_RECORDS_PER_REQUEST {
                return Err(RpcError::BadRequest(format!(
                    "block record range {}..{} exceeds the {MAX_BLOCK_RECORDS_PER_REQUEST}-record cap",
                    req.start, req.end
                )));
            }
            let Some((_, peak_height)) = node.store.get_peak().await? else {
                return Err(RpcError::BadRequest("Peak is None".to_string()));
            };
            let mut records = Vec::new();
            for height in req.start..req.end {
                if height > peak_height {
                    break;
                }
                records.push(
                    node.store
                        .get_block_record_by_height(height)
                        .await?
                        .ok_or_else(|| {
                            RpcError::BadRequest(format!("Height not in blockchain: {height}"))
                        })?,
                );
            }
            envelope("block_records", &records)?
        };
        Ok(Some(out))
    })
    .await
}
