use super::*;
use dg_xch_core::protocols::wallet_sync::WalletCoinRecordsRequest;

pub const PATH: &str = "/get_wallet_coin_records";

#[portfu::prelude::post("/get_wallet_coin_records", client_trust = "rpc-clients")]
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
        let request: WalletCoinRecordsRequest = parse(&body)?;
        request.validate().map_err(RpcError::BadRequest)?;
        if !cfg!(feature = "hint") || !node.store.supports_wallet_sync() {
            return Err(RpcError::BadRequest(
                "Backend does not support wallet coin discovery".into(),
            ));
        }
        if !node.synced.load(Ordering::Relaxed) {
            return Err(RpcError::BadRequest(
                "Node is syncing; wallet queries unavailable".into(),
            ));
        }
        let _permit = EXPLORER_WORK
            .try_acquire()
            .map_err(|_| RpcError::BadRequest("Wallet queries busy; retry later".into()))?;
        let response = tokio::time::timeout(Duration::from_secs(6), async {
            let peak = node
                .store
                .get_peak()
                .await?
                .ok_or_else(|| RpcError::BadRequest("Node peak unavailable".into()))?;
            if peak.0 != request.peak_hash {
                return Err(RpcError::BadRequest(
                    "Node checkpoint changed; retry".into(),
                ));
            }
            let response = node.store.wallet_coin_records(&request, peak.1).await?;
            // Reject growth as well as reorgs; callers restart discovery at the new peak.
            if node.store.get_peak().await? != Some(peak) || !node.synced.load(Ordering::Relaxed) {
                return Err(RpcError::BadRequest(
                    "Node checkpoint changed; retry".into(),
                ));
            }
            Ok::<_, RpcError>(response)
        })
        .await
        .map_err(|_| RpcError::BadRequest("Wallet query timed out".into()))??;
        Ok(Some(serde_json::from_value(to_value(&response)?).map_err(
            |error| RpcError::BadRequest(error.to_string()),
        )?))
    })
    .await
}
