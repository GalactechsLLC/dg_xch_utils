use crate::types::blockchain::wallet_type::WalletType;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct WalletInfo {
    pub data: String,
    pub name: String,
    pub id: u32,
    #[serde(alias = "type")]
    pub wallet_type: WalletType,
}
