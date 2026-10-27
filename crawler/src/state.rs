pub use dg_xch_servers::discovery::{Peer, Point, Summary, Totals};
use std::time::{SystemTime, UNIX_EPOCH};
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}
