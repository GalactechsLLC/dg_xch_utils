use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum TXStatus {
    SUCCESS = 1,
    PENDING = 2,
    FAILED = 3,
}
