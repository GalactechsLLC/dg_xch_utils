#[cfg(test)]
mod tests {
    use dg_xch_core::blockchain::blockchain_state::{BlockchainState, MinMempoolFees};
    use dg_xch_core::blockchain::sized_bytes::Bytes32;
    use dg_xch_core::blockchain::sync::Sync;
    use hex::decode;

    fn create_blockchain_state_with_space(space_value: &str) -> String {
        format!(
            r#"
    {{
        "peak": null,
        "genesis_challenge_initialized": true,
        "sync": {{
            "sync_mode": false,
            "sync_progress_height": 0,
            "sync_tip_height": 0,
            "synced": true
        }},
        "difficulty": 1000,
        "sub_slot_iters": 1000,
        "space": {},
        "mempool_size": 10,
        "mempool_cost": 100,
        "mempool_min_fees": {{
            "cost_5000000": 0.1
        }},
        "mempool_max_total_cost": 1000,
        "block_max_cost": 10000,
        "node_id": "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789"
    }}
    "#,
            space_value
        )
    }

    fn create_blockchain_state_with_space_for_f32(space_value: f32) -> String {
        format!(
            r#"
    {{
        "peak": null,
        "genesis_challenge_initialized": true,
        "sync": {{
            "sync_mode": false,
            "sync_progress_height": 0,
            "sync_tip_height": 0,
            "synced": true
        }},
        "difficulty": 1000,
        "sub_slot_iters": 1000,
        "space": {},
        "mempool_size": 10,
        "mempool_cost": 100,
        "mempool_min_fees": {{
            "cost_5000000": 0.1
        }},
        "mempool_max_total_cost": 1000,
        "block_max_cost": 10000,
        "node_id": "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789"
    }}
    "#,
            space_value
        )
    }

    fn create_blockchain_state_with_space_for_f64(space_value: f64) -> String {
        format!(
            r#"
    {{
        "peak": null,
        "genesis_challenge_initialized": true,
        "sync": {{
            "sync_mode": false,
            "sync_progress_height": 0,
            "sync_tip_height": 0,
            "synced": true
        }},
        "difficulty": 1000,
        "sub_slot_iters": 1000,
        "space": {},
        "mempool_size": 10,
        "mempool_cost": 100,
        "mempool_min_fees": {{
            "cost_5000000": 0.1
        }},
        "mempool_max_total_cost": 1000,
        "block_max_cost": 10000,
        "node_id": "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789"
    }}
    "#,
            space_value
        )
    }

    #[test]
    fn test_parse_u128_from_bool_true() {
        let json_data = create_blockchain_state_with_space("true");
        let state: BlockchainState = serde_json::from_str(&json_data).unwrap();
        assert_eq!(state.space, 1u128);
    }

    #[test]
    fn test_parse_u128_from_bool_false() {
        let json_data = create_blockchain_state_with_space("false");
        let state: BlockchainState = serde_json::from_str(&json_data).unwrap();
        assert_eq!(state.space, 0u128);
    }

    #[test]
    fn test_parse_u128_from_i64_max() {
        let max_i64 = i64::MAX;
        let json_data = create_blockchain_state_with_space(&max_i64.to_string());
        let state: BlockchainState = serde_json::from_str(&json_data).unwrap();
        assert_eq!(state.space, max_i64 as u128);
    }

    #[test]
    fn test_parse_u128_from_i64_negative() {
        let json_data = create_blockchain_state_with_space("-1");
        let result: Result<BlockchainState, _> = serde_json::from_str(&json_data);
        assert!(result.is_err(), "Negative numbers should fail for u128");
    }

    #[test]
    fn test_parse_u128_from_u8() {
        let json_data = create_blockchain_state_with_space("255");
        let state: BlockchainState = serde_json::from_str(&json_data).unwrap();
        assert_eq!(state.space, 255u128);
    }

    #[test]
    fn test_parse_u128_from_u16() {
        let json_data = create_blockchain_state_with_space("65535");
        let state: BlockchainState = serde_json::from_str(&json_data).unwrap();
        assert_eq!(state.space, 65535u128);
    }

    #[test]
    fn test_parse_u128_from_u32() {
        let json_data = create_blockchain_state_with_space("4294967295");
        let state: BlockchainState = serde_json::from_str(&json_data).unwrap();
        assert_eq!(state.space, 4294967295u128);
    }

    #[test]
    fn test_parse_u128_from_u64_max() {
        let max_u64 = u64::MAX;
        let json_data = create_blockchain_state_with_space(&max_u64.to_string());
        let state: BlockchainState = serde_json::from_str(&json_data).unwrap();
        assert_eq!(state.space, max_u64 as u128);
    }

    #[test]
    fn test_parse_u128_from_f32() {
        let json_data = create_blockchain_state_with_space_for_f32(1234.67f32);
        let state: BlockchainState = serde_json::from_str(&json_data).unwrap();
        assert_eq!(state.space, 1234u128);
    }

    #[test]
    fn test_parse_u128_from_f64() {
        let json_data = create_blockchain_state_with_space_for_f64(9876543210.12345f64);
        let state: BlockchainState = serde_json::from_str(&json_data).unwrap();
        assert_eq!(state.space, 9876543210u128);
    }

    #[test]
    fn test_parse_u128_from_f64_large() {
        let json_data = create_blockchain_state_with_space("1e20");
        let state: BlockchainState = serde_json::from_str(&json_data).unwrap();
        assert_eq!(state.space, 100000000000000000000u128);
    }

    #[test]
    fn test_parse_u128_from_valid_string() {
        let json_data = create_blockchain_state_with_space("\"12345678901234567890\"");
        let state: BlockchainState = serde_json::from_str(&json_data).unwrap();
        assert_eq!(state.space, 12345678901234567890u128);
    }

    #[test]
    fn test_parse_u128_from_invalid_string() {
        let json_data = create_blockchain_state_with_space("\"not_a_number\"");
        let result: Result<BlockchainState, _> = serde_json::from_str(&json_data);
        assert!(
            result.is_err(),
            "Invalid string should fail deserialization"
        );
    }

    #[test]
    fn test_parse_u128_from_string_with_whitespace() {
        let json_data = create_blockchain_state_with_space("\"  9876543210  \"");
        let result: Result<BlockchainState, _> = serde_json::from_str(&json_data);
        assert!(
            result.is_err(),
            "String with whitespace should fail deserialization"
        );
    }

    // Existing tests for BlockchainState deserialization

    #[test]
    fn test_blockchain_state_deserialize_space_integer() {
        let json_data = create_blockchain_state_with_space("12345678901234567890");
        let state: BlockchainState = serde_json::from_str(&json_data).unwrap();
        assert_eq!(state.space, 12345678901234567890u128);
    }

    #[test]
    fn test_blockchain_state_deserialize_space_string() {
        let value = 12345678901234567890u128;
        let json_data = create_blockchain_state_with_space(&format!("\"{}\"", value));
        let state: BlockchainState = serde_json::from_str(&json_data).unwrap();
        assert_eq!(state.space, value);
    }

    #[test]
    fn test_blockchain_state_deserialize_space_invalid() {
        let json_data = create_blockchain_state_with_space("\"invalid\"");
        let result: Result<BlockchainState, _> = serde_json::from_str(&json_data);
        assert!(result.is_err());
    }

    #[test]
    fn test_blockchain_state_serialize_deserialize() {
        let node_id_bytes =
            decode("abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789").unwrap();
        let node_id = Bytes32::from(node_id_bytes);

        let original_state = BlockchainState {
            peak: None,
            genesis_challenge_initialized: true,
            sync: Sync {
                sync_mode: false,
                sync_progress_height: 0,
                sync_tip_height: 0,
                synced: true,
            },
            difficulty: 1000,
            sub_slot_iters: 1000,
            space: 12345678901234567890u128,
            mempool_size: 0,
            mempool_cost: 0,
            mempool_min_fees: MinMempoolFees { cost_5000000: 0.0 },
            mempool_max_total_cost: 0,
            block_max_cost: 0,
            node_id,
        };

        let serialized = serde_json::to_string(&original_state).unwrap();
        let deserialized_state: BlockchainState = serde_json::from_str(&serialized).unwrap();

        assert_eq!(original_state, deserialized_state);
    }

    #[test]
    fn test_eq_state_min_mempool_fees() {
        // Case 1: Both finite and equal
        let fee1 = MinMempoolFees { cost_5000000: 1.0 };
        let fee2 = MinMempoolFees { cost_5000000: 1.0 };
        assert_eq!(fee1, fee2, "Finite and equal values should be equal");

        // Case 2: Both finite and not equal
        let fee3 = MinMempoolFees { cost_5000000: 2.0 };
        assert_ne!(fee1, fee3, "Finite but unequal values should not be equal");

        // Case 3: Both are NaN
        let fee4 = MinMempoolFees {
            cost_5000000: f64::NAN,
        };
        let fee5 = MinMempoolFees {
            cost_5000000: f64::NAN,
        };
        assert_eq!(fee4, fee5, "Both NaN values should be considered equal");

        // Case 4: One is NaN, one is finite
        assert_ne!(fee1, fee4, "NaN and finite value should not be equal");

        // Case 5: Both are Infinity
        let fee6 = MinMempoolFees {
            cost_5000000: f64::INFINITY,
        };
        let fee7 = MinMempoolFees {
            cost_5000000: f64::INFINITY,
        };
        assert_eq!(
            fee6, fee7,
            "Both Infinity values should be considered equal"
        );

        // Case 6: One is Infinity, one is finite
        assert_ne!(fee1, fee6, "Infinity and finite value should not be equal");

        // Case 7: One is Infinity, the other is NaN
        assert_ne!(fee4, fee6, "Infinity and NaN should not be equal");
    }
}
