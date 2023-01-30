use crate::types::blockchain::coin_spend::CoinSpend;
use crate::types::blockchain::sized_bytes::Bytes96;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SpendBundle {
    pub coin_spends: Vec<CoinSpend>,
    pub aggregated_signature: Bytes96,
}
