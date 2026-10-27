use super::*;
use dg_xch_core::protocols::wallet_sync::RpcCapabilities;

pub const PATH: &str = "/get_rpc_capabilities";

fn capabilities(wallet_sync: bool, explorer: bool) -> RpcCapabilities {
    let mut endpoints = std::collections::BTreeMap::from([(PATH.into(), 1)]);
    if wallet_sync {
        endpoints.insert(super::get_wallet_coin_records::PATH.into(), 1);
    }
    if explorer {
        endpoints.insert("/get_coin_records_page".into(), 1);
        endpoints.insert("/get_coin_summary".into(), 1);
    }
    RpcCapabilities {
        success: true,
        endpoints,
    }
}

#[portfu::prelude::post("/get_rpc_capabilities", client_trust = "rpc-clients")]
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
        let _: Map<String, Value> = parse(&body)?;
        let response = capabilities(
            cfg!(feature = "hint") && node.store.supports_wallet_sync(),
            cfg!(feature = "coin-index") && node.store.supports_explorer_queries(),
        );
        Ok(Some(serde_json::from_value(to_value(&response)?).map_err(
            |error| RpcError::BadRequest(error.to_string()),
        )?))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::capabilities;

    #[test]
    fn capabilities_only_advertise_available_query_contracts() {
        for (wallet, explorer) in [(false, false), (true, false), (false, true), (true, true)] {
            let response = capabilities(wallet, explorer);
            assert!(response.success);
            assert_eq!(response.endpoints.get("/get_rpc_capabilities"), Some(&1));
            assert_eq!(
                response.endpoints.contains_key("/get_wallet_coin_records"),
                wallet
            );
            assert_eq!(
                response.endpoints.contains_key("/get_coin_records_page"),
                explorer
            );
            assert_eq!(
                response.endpoints.contains_key("/get_coin_summary"),
                explorer
            );
        }
    }
}
