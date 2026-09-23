pub mod farmer;
pub mod full_node;
pub mod pool;
pub mod pool_v2;
pub mod responses;
pub mod simulator;
pub mod wallet;

use serde::Serialize;

pub(crate) enum RequestMode<T: Serialize> {
    Json(T),
    Query(T),
    Send,
}
