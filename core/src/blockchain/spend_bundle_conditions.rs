use crate::blockchain::spend::Spend;
use dg_xch_macros::ChiaSerial;
use serde::{Deserialize, Serialize};

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct SpendBundleConditions {
    pub spends: Vec<Spend>,
    pub reserve_fee: u64,
    pub height_absolute: u32,
    pub seconds_absolute: u64,
    pub before_height_absolute: Option<u32>,
    pub before_seconds_absolute: Option<u32>,
    pub agg_sig_unsafe: Vec<(Vec<u8>, Vec<u8>)>,
    pub cost: u64,
    pub removal_amount: u64,
    pub addition_amount: u64,
}
