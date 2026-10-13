//! Bounded query contracts for gateways and explorers. Amount totals are decimal strings.
use crate::blockchain::{coin_record::CoinRecord, sized_bytes::Bytes32};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoinPageRequest {
    #[serde(default)]
    pub puzzle_hashes: Vec<Bytes32>,
    #[serde(default)]
    pub parent_ids: Vec<Bytes32>,
    pub peak_hash: Bytes32,
    pub offset: u32,
    pub limit: u32,
    pub spent: Option<bool>,
}
impl CoinPageRequest {
    pub fn validate(&self) -> Result<(), &'static str> {
        if (self.puzzle_hashes.is_empty() == self.parent_ids.is_empty())
            || self.puzzle_hashes.len() + self.parent_ids.len() > 8192
            || self.limit == 0
            || self.limit > 500
            || self.offset > 20_000
        {
            return Err(
                "Expected exactly one selector of 1..8192 puzzle hashes or parent ids, limit 1..500, and offset <=20000",
            );
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoinSummaryRequest {
    pub puzzle_hash: Bytes32,
    pub peak_hash: Bytes32,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CoinSummary {
    pub balance: String,
    pub received: String,
    pub spent: String,
    pub coin_count: u64,
    pub unspent_count: u64,
    pub first_seen: Option<u64>,
    pub last_received: Option<u64>,
    pub last_activity_height: Option<u32>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CoinPageResponse {
    pub success: bool,
    pub peak_hash: Bytes32,
    pub height: u32,
    pub coin_records: Vec<CoinRecord>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CoinSummaryResponse {
    pub success: bool,
    pub peak_hash: Bytes32,
    pub height: u32,
    pub summary: CoinSummary,
}
