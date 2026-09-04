use super::*;

pub const PATH: &str = "/get_block_spends_with_conditions";
#[portfu::prelude::post("/get_block_spends_with_conditions")]
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
                tokio::task::spawn_blocking(move || {
                    coin_spends_with_conditions_from_generator(&input)
                })
                .await
                .map_err(|error| {
                    RpcError::BadRequest(format!("spends worker panicked: {error:?}"))
                })?
                .map_err(|error| {
                    RpcError::BadRequest(format!("Failed to get spends for block: {error:?}"))
                })?
            };
            let mut arr = Vec::with_capacity(spends.len());
            for (coin_spend, conditions) in spends {
                let mut m = Map::new();
                m.insert("coin_spend".to_string(), to_value(&coin_spend)?);
                m.insert(
                    "conditions".to_string(),
                    Value::from(
                        conditions
                            .iter()
                            .map(condition_json)
                            .collect::<Vec<Value>>(),
                    ),
                );
                arr.push(Value::Object(m));
            }
            obj_with("block_spends_with_conditions", Value::from(arr))
        };
        Ok(Some(out))
    })
    .await
}
