pub mod full_node;
pub mod pool;
pub mod responses;
pub mod wallet;

use serde::Serialize;

pub(crate) enum RequestMode<T: Serialize> {
    Json(T),
    Query(T),
    Send,
}
