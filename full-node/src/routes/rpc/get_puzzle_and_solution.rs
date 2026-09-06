use super::*;

pub const PATH: &str = "/get_puzzle_and_solution";
#[portfu::prelude::post("/get_puzzle_and_solution", client_trust = "rpc-clients")]
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
            let req: PuzzleAndSolutionRequest = parse(body)?;
            let spend = puzzle_and_solution_coin_spend(
                node.store.as_ref(),
                &node.constants,
                &req.coin_id,
                req.height,
            )
            .await?;
            envelope("coin_solution", &spend)?
        };
        Ok(Some(out))
    })
    .await
}
