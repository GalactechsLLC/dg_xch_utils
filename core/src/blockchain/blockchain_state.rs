use crate::blockchain::block_record::BlockRecord;
use crate::blockchain::sized_bytes::Bytes32;
use crate::blockchain::sync::Sync;
use dg_xch_macros::ChiaSerial;
use num_traits::FromPrimitive;
use serde::de::{Error, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use std::fmt::Formatter;
use std::marker::PhantomData;
use std::str::FromStr;

#[derive(ChiaSerial, Copy, Clone, Serialize, Deserialize, Debug)]
pub struct MinMempoolFees {
    pub cost_5000000: f64,
}

impl PartialEq for MinMempoolFees {
    fn eq(&self, other: &Self) -> bool {
        if self.cost_5000000.is_infinite() && other.cost_5000000.is_infinite() {
            true
        } else if self.cost_5000000.is_infinite() || other.cost_5000000.is_infinite() {
            false
        } else if self.cost_5000000.is_nan() && other.cost_5000000.is_nan() {
            true
        } else if self.cost_5000000.is_nan() || other.cost_5000000.is_nan() {
            false
        } else {
            self.cost_5000000 == other.cost_5000000
        }
    }
}

impl Eq for MinMempoolFees {}

pub fn parse_u128<'de, D>(d: D) -> Result<u128, D::Error>
where
    D: Deserializer<'de>,
{
    struct PU128(PhantomData<fn() -> u128>);
    impl Visitor<'_> for PU128 {
        type Value = u128;

        fn expecting(&self, formatter: &mut Formatter) -> std::fmt::Result {
            formatter.write_str("string or number")
        }

        fn visit_bool<E>(self, v: bool) -> Result<Self::Value, E>
        where
            E: Error,
        {
            u128::from_u8(u8::from(v)).ok_or_else(|| Error::custom("Invalid Value for bool"))
        }

        fn visit_i8<E>(self, v: i8) -> Result<Self::Value, E>
        where
            E: Error,
        {
            u128::from_i8(v).ok_or_else(|| Error::custom("Invalid Value for i8"))
        }

        fn visit_i16<E>(self, v: i16) -> Result<Self::Value, E>
        where
            E: Error,
        {
            u128::from_i16(v).ok_or_else(|| Error::custom("Invalid Value for i16"))
        }

        fn visit_i32<E>(self, v: i32) -> Result<Self::Value, E>
        where
            E: Error,
        {
            u128::from_i32(v).ok_or_else(|| Error::custom("Invalid Value for i32"))
        }

        fn visit_i64<E>(self, v: i64) -> Result<Self::Value, E>
        where
            E: Error,
        {
            u128::from_i64(v).ok_or_else(|| Error::custom("Invalid Value for i64"))
        }

        fn visit_u8<E>(self, v: u8) -> Result<Self::Value, E>
        where
            E: Error,
        {
            u128::from_u8(v).ok_or_else(|| Error::custom("Invalid Value for u8"))
        }

        fn visit_u16<E>(self, v: u16) -> Result<Self::Value, E>
        where
            E: Error,
        {
            u128::from_u16(v).ok_or_else(|| Error::custom("Invalid Value for u16"))
        }

        fn visit_u32<E>(self, v: u32) -> Result<Self::Value, E>
        where
            E: Error,
        {
            u128::from_u32(v).ok_or_else(|| Error::custom("Invalid Value for u32"))
        }

        fn visit_u64<E>(self, v: u64) -> Result<Self::Value, E>
        where
            E: Error,
        {
            u128::from_u64(v).ok_or_else(|| Error::custom("Invalid Value for u64"))
        }

        fn visit_f32<E>(self, v: f32) -> Result<Self::Value, E>
        where
            E: Error,
        {
            u128::from_f32(v).ok_or_else(|| Error::custom("Invalid Value for f32"))
        }

        fn visit_f64<E>(self, v: f64) -> Result<Self::Value, E>
        where
            E: Error,
        {
            u128::from_f64(v).ok_or_else(|| Error::custom("Invalid Value for f64"))
        }

        fn visit_char<E>(self, v: char) -> Result<Self::Value, E>
        where
            E: Error,
        {
            u128::from_str(&v.to_string()).map_err(|_| Error::custom("Invalid Value for char"))
        }

        fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
        where
            E: Error,
        {
            u128::from_str(v).map_err(|_| Error::custom("Invalid Value for str"))
        }

        fn visit_string<E>(self, v: String) -> Result<Self::Value, E>
        where
            E: Error,
        {
            u128::from_str(&v).map_err(|_| Error::custom("Invalid Value for string"))
        }
    }
    d.deserialize_any(PU128(PhantomData))
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct BlockchainState {
    pub peak: Option<BlockRecord>,
    pub genesis_challenge_initialized: bool,
    pub sync: Sync,
    pub difficulty: u64,
    pub sub_slot_iters: u64,
    #[serde(deserialize_with = "parse_u128")]
    pub space: u128,
    pub mempool_size: u64,
    pub mempool_cost: u64,
    pub mempool_min_fees: MinMempoolFees,
    pub mempool_max_total_cost: u64,
    pub block_max_cost: u64,
    pub node_id: Bytes32,
}
