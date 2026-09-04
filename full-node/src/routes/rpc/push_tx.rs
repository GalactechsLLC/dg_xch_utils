use super::*;

pub const PATH: &str = "/push_tx";
#[portfu::prelude::post("/push_tx", client_trust = "rpc-clients")]
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
            let req: PushTxRequest = parse(body)?;
            let admission = crate::tx_admission::admit_spend_bundle(
                node.store.as_ref(),
                &node.mempool,
                &node.constants,
                &node.tx_announce,
                req.spend_bundle,
            )
            .await
            .map_err(|error| match error {
                crate::tx_admission::TxAdmissionError::Validation(error) => {
                    RpcError::BadRequest(format!("invalid spend bundle: {error:?}"))
                }
                crate::tx_admission::TxAdmissionError::Mempool(error) => RpcError::Mempool(error),
                crate::tx_admission::TxAdmissionError::Store(error) => RpcError::Store(error),
                crate::tx_admission::TxAdmissionError::Corrupt(error) => RpcError::Corrupt(error),
            });
            match admission {
                Ok(_name) => obj_with("status", Value::from("SUCCESS")),
                // A PENDING-classed rejection answers {"status": "PENDING"}; only FAILED
                // errors.
                Err(RpcError::Mempool(m)) => {
                    let (status, err_name) = m.ack();
                    if status == TXStatus::PENDING {
                        obj_with("status", Value::from("PENDING"))
                    } else {
                        return Err(RpcError::BadRequest(format!(
                            "Failed to include transaction, error {err_name}"
                        )));
                    }
                }
                Err(e) => return Err(e),
            }
        };
        Ok(Some(out))
    })
    .await
}
