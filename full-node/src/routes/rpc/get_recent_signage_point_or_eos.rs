use super::*;

pub const PATH: &str = "/get_recent_signage_point_or_eos";
#[portfu::prelude::post("/get_recent_signage_point_or_eos")]
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
            let req: RecentSignagePointorEOSRequest = parse_or_default(body)?;
            let live = node.live.get();
            if let Some(sp_hash) = req.sp_hash.as_ref() {
                let signage_point = live
                    .ok_or_else(|| sp_not_in_cache(sp_hash))?
                    .slot_state
                    .lock()
                    .await
                    .get_signage_point(sp_hash)
                    .ok_or_else(|| sp_not_in_cache(sp_hash))?;
                let mut response = Map::new();
                response.insert("signage_point".to_string(), to_value(&signage_point)?);
                response.insert("time_received".to_string(), Value::from(0.0f64));
                response.insert("reverted".to_string(), Value::from(false));
                response
            } else {
                let challenge_hash = req.challenge_hash.as_ref().ok_or_else(|| {
                    RpcError::BadRequest("sp_hash or challenge_hash required".to_string())
                })?;
                let eos = live
                    .ok_or_else(|| eos_not_in_cache(challenge_hash))?
                    .slot_state
                    .lock()
                    .await
                    .get_sub_slot(challenge_hash)
                    .map(|(eos, _, _)| eos.clone())
                    .ok_or_else(|| eos_not_in_cache(challenge_hash))?;
                let mut response = Map::new();
                response.insert("eos".to_string(), to_value(&eos)?);
                response.insert("time_received".to_string(), Value::from(0.0f64));
                response.insert("reverted".to_string(), Value::from(false));
                response
            }
        };
        Ok(Some(out))
    })
    .await
}
