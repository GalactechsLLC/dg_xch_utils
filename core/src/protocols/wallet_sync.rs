//! Versioned RPC capabilities and bounded wallet coin discovery at one chain checkpoint.
use crate::blockchain::{coin_record::CoinRecord, sized_bytes::Bytes32};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RpcCapabilities {
    pub success: bool,
    /// Absolute RPC paths mapped to the supported request/response contract version.
    pub endpoints: BTreeMap<String, u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WalletCoinRecordsRequest {
    #[serde(default)]
    pub puzzle_hashes: Vec<Bytes32>,
    #[serde(default)]
    pub hints: Vec<Bytes32>,
    pub peak_hash: Bytes32,
    pub cursor: Option<Bytes32>,
    pub limit: u32,
}

impl WalletCoinRecordsRequest {
    pub fn validate(&self) -> Result<(), String> {
        let selectors = self.puzzle_hashes.len().saturating_add(self.hints.len());
        if !(1..=100).contains(&selectors) || !(1..=500).contains(&self.limit) {
            return Err(
                "Expected 1..100 combined puzzle hashes and hints, and limit 1..500".into(),
            );
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WalletCoinRecordsResponse {
    pub success: bool,
    pub peak_hash: Bytes32,
    pub height: u32,
    /// Unique coin records ordered by coin ID, strictly after the request cursor.
    pub coin_records: Vec<CoinRecord>,
    /// The last returned coin ID only when another page exists.
    pub next_cursor: Option<Bytes32>,
}

#[cfg(test)]
mod tests {
    use super::WalletCoinRecordsRequest;
    use crate::blockchain::sized_bytes::Bytes32;

    #[test]
    fn wallet_selectors_are_bounded_together_and_optional_arrays_default_empty() {
        let mut request: WalletCoinRecordsRequest = serde_json::from_value(serde_json::json!({
            "hints": [Bytes32::default()], "peak_hash": Bytes32::default(), "limit": 500,
        }))
        .unwrap();
        assert!(request.puzzle_hashes.is_empty());
        assert_eq!(request.cursor, None);
        assert!(request.validate().is_ok());
        request.puzzle_hashes = vec![Bytes32::default(); 99];
        assert!(request.validate().is_ok());
        request.hints.push(Bytes32::default());
        assert!(request.validate().is_err());
        request.puzzle_hashes.clear();
        request.hints.clear();
        assert!(request.validate().is_err());
        request.puzzle_hashes.push(Bytes32::default());
        for limit in [0, 501, u32::MAX] {
            request.limit = limit;
            assert!(request.validate().is_err());
        }
        request.limit = 1;
        assert!(request.validate().is_ok());
    }
}
