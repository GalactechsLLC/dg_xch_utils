use super::*;

pub const PATH: &str = "/get_blocks";
#[portfu::prelude::post("/get_blocks", client_trust = "rpc-clients")]
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
            let req: BlocksRequest = parse(body)?;
            if req.end.saturating_sub(req.start) > MAX_BLOCKS_PER_REQUEST {
                return Err(RpcError::BadRequest(format!(
                    "block range {}..{} exceeds the {MAX_BLOCKS_PER_REQUEST}-block cap",
                    req.start, req.end
                )));
            }
            let mut blocks = Vec::new();
            for height in req.start..req.end {
                let Some(record) = node.store.get_block_record_by_height(height).await? else {
                    continue;
                };
                if let Some(block) = node.store.get_block(&record.header_hash).await? {
                    blocks.push((block, record.header_hash));
                }
            }
            let mut arr = Vec::with_capacity(blocks.len());
            for (block, header_hash) in blocks {
                let mut v = to_value(&block)?;
                if !req.exclude_header_hash
                    && let Value::Object(obj) = &mut v
                {
                    // The wire convention injects PLAIN hex here.
                    obj.insert(
                        "header_hash".to_string(),
                        Value::from(plain_hex(&header_hash)),
                    );
                }
                arr.push(v);
            }
            obj_with("blocks", Value::from(arr))
        };
        Ok(Some(out))
    })
    .await
}
