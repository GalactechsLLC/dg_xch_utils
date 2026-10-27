use crate::blockchain::spend::Spend;
use crate::blockchain::unsized_bytes::UnsizedBytes;
use serde::{Deserialize, Serialize};

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug, Default)]
pub struct SpendBundleConditions {
    pub spends: Vec<Spend>,
    #[cfg(feature = "bls")]
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub falcon_signatures: Vec<crate::signatures::falcon_condition::FalconCondition>,
    pub reserve_fee: u64,
    pub height_absolute: u32,
    pub seconds_absolute: u64,
    pub before_height_absolute: Option<u32>,
    pub before_seconds_absolute: Option<u64>,
    pub agg_sig_unsafe: Vec<(UnsizedBytes, UnsizedBytes)>,
    pub cost: u64,
    pub removal_amount: u128,
    pub addition_amount: u128,
}
