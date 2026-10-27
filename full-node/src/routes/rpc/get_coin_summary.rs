use super::*;
use dg_xch_core::protocols::explorer::CoinSummaryRequest;
pub const PATH: &str = "/get_coin_summary";
#[portfu::prelude::post("/get_coin_summary", client_trust = "rpc-clients")]
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
        let req: CoinSummaryRequest = parse(&body)?;

        if !node.synced.load(Ordering::Relaxed) {
            return Err(RpcError::BadRequest(
                "Node is syncing; explorer queries unavailable".into(),
            ));
        }
        let _permit = EXPLORER_WORK
            .try_acquire()
            .map_err(|_| RpcError::BadRequest("Explorer queries busy; retry later".into()))?;
        let peak = node
            .store
            .get_peak()
            .await?
            .ok_or_else(|| RpcError::BadRequest("Node peak unavailable".into()))?;
        if peak.0 != req.peak_hash {
            return Err(RpcError::BadRequest(
                "Node checkpoint changed; retry".into(),
            ));
        }
        let height = peak.1;
        let value = tokio::time::timeout(Duration::from_secs(6), async {
            Ok::<_, RpcError>(node.store.coin_summary(&req.puzzle_hash, height).await?)
        })
        .await
        .map_err(|_| RpcError::BadRequest("Explorer query timed out".into()))??;
        if node.store.get_peak().await? != Some(peak) {
            return Err(RpcError::BadRequest(
                "Node checkpoint changed; retry".into(),
            ));
        }
        let mut out = envelope("summary", &value)?;
        out.insert("peak_hash".into(), to_value(&peak.0)?);
        out.insert("height".into(), Value::from(height));
        Ok(Some(out))
    })
    .await
}
