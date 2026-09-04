use super::*;

pub const PATH: &str = "/get_coin_record_by_name";
#[portfu::prelude::post("/get_coin_record_by_name")]
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
            let req: CoinRecordByNameRequest = parse(body)?;
            let record = node
                .store
                .get_coin_record(&req.name)
                .await?
                .ok_or_else(|| {
                    RpcError::BadRequest(format!("Coin record {} not found", req.name))
                })?;
            envelope("coin_record", &record)?
        };
        Ok(Some(out))
    })
    .await
}
