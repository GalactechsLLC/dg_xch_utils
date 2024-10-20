use dg_xch_macros::ChiaSerial;
use serde::{Deserialize, Serialize};

#[derive(ChiaSerial, Copy, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum TXStatus {
    SUCCESS = 1,
    PENDING = 2,
    FAILED = 3,
}
impl From<u8> for TXStatus {
    fn from(value: u8) -> Self {
        match value {
            1 => TXStatus::SUCCESS,
            2 => TXStatus::PENDING,
            _ => TXStatus::FAILED,
        }
    }
}
