use crate::blockchain::wallet_type::WalletType;
use dg_xch_macros::ChiaSerial;
use serde::{Deserialize, Serialize};

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct WalletInfo {
    pub data: String,
    pub name: String,
    pub id: u32,
    #[serde(alias = "type")]
    pub wallet_type: WalletType,
}
