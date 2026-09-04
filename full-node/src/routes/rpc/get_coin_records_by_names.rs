use super::*;

pub const PATH: &str = "/get_coin_records_by_names";
#[portfu::prelude::post("/get_coin_records_by_names")]
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
            let req: CoinRecordByNamesRequest = parse(body)?;
            let window = CoinQueryWindow::from_options(
                req.include_spent_coins,
                req.start_height,
                req.end_height,
            );
            check_id_cap(req.names.len())?;
            let records =
                apply_coin_query_window(window, node.store.get_coin_records(&req.names).await?);
            envelope("coin_records", &records)?
        };
        Ok(Some(out))
    })
    .await
}
