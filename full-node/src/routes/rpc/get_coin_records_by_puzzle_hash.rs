use super::*;

pub const PATH: &str = "/get_coin_records_by_puzzle_hash";
#[portfu::prelude::post("/get_coin_records_by_puzzle_hash", client_trust = "rpc-clients")]
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
            let req: CoinRecordsByPuzzleHashRequest = parse(body)?;
            let window = CoinQueryWindow::from_options(
                req.include_spent_coins,
                req.start_height,
                req.end_height,
            );
            let records = if window.include_spent_coins {
                let states = node
                    .store
                    .get_coin_states_by_puzzle_hashes(
                        std::slice::from_ref(&req.puzzle_hash),
                        0,
                        true,
                        dg_xch_stores::traits::MAX_COIN_STATES,
                    )
                    .await?;
                let names = states
                    .iter()
                    .map(|state| state.coin.name())
                    .collect::<Vec<_>>();
                let mut records = Vec::new();
                for chunk in names.chunks(900) {
                    records.extend(node.store.get_coin_records(chunk).await?);
                }
                records
            } else {
                node.store
                    .get_unspent_by_puzzle_hash(&req.puzzle_hash)
                    .await?
            };
            let records = apply_coin_query_window(window, records);
            envelope("coin_records", &records)?
        };
        Ok(Some(out))
    })
    .await
}
