use serde::{Deserialize, Serialize};

const fn one() -> i64 {
    1
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct FarmBlockRequest {
    pub address: String,
    #[serde(default = "one")]
    pub blocks: i64,
    #[serde(default)]
    pub guarantee_tx_block: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
pub struct AutoFarmRequest {
    #[serde(rename = "auto_farm", alias = "should_auto_farm")]
    pub auto_farm: bool,
}
