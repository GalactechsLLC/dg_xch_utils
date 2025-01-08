#[cfg(test)]
mod tests {
    use dg_xch_core::blockchain::coin::Coin;
    use dg_xch_core::blockchain::sized_bytes::Bytes32;
    use std::hash::{Hash, Hasher};

    #[test]
    fn test_coin_id_zero_amount() {
        let coin = Coin {
            parent_coin_info: Bytes32::from([0u8; 32]),
            puzzle_hash: Bytes32::from([0u8; 32]),
            amount: 0,
        };
        let coin_id = coin.coin_id();
        // Ensure coin_id is computed and not zero
        assert_ne!(coin_id, Bytes32::from([0u8; 32]));
    }

    #[test]
    fn test_coin_id_small_amount() {
        let coin = Coin {
            parent_coin_info: Bytes32::from([1u8; 32]),
            puzzle_hash: Bytes32::from([2u8; 32]),
            amount: 1,
        };
        let coin_id = coin.coin_id();
        assert_ne!(coin_id, Bytes32::from([0u8; 32]));
    }

    #[test]
    fn test_coin_id_large_amount() {
        let coin = Coin {
            parent_coin_info: Bytes32::from([3u8; 32]),
            puzzle_hash: Bytes32::from([4u8; 32]),
            amount: 0x8000_0000_0000_0000,
        };
        let coin_id = coin.coin_id();
        assert_ne!(coin_id, Bytes32::from([0u8; 32]));
    }

    #[test]
    fn test_coin_id_max_amount() {
        let coin = Coin {
            parent_coin_info: Bytes32::from([5u8; 32]),
            puzzle_hash: Bytes32::from([6u8; 32]),
            amount: u64::MAX,
        };
        let coin_id = coin.coin_id();
        assert_ne!(coin_id, Bytes32::from([0u8; 32]));
    }

    #[test]
    fn test_coin_id_various_amounts() {
        let parent_coin_info = Bytes32::from([7u8; 32]);
        let puzzle_hash = Bytes32::from([8u8; 32]);

        // Amounts to cover different branches and leading zeros
        let amounts = [
            0u64,
            1u64,
            0xFFu64,                // 255
            0xFFFFu64,              // 65535
            0xFFFFFFu64,            // 16777215
            0xFFFFFFFFu64,          // 4294967295
            0xFFFFFFFFFFu64,        // 1099511627775
            0xFFFFFFFFFFFFu64,      // 281474976710655
            0xFFFFFFFFFFFFFFu64,    // 72057594037927935
            0x7FFFFFFFFFFFFFFF_u64, // Max 63-bit value
            0x8000000000000000_u64, // Min value triggering other branch
            u64::MAX,
        ];

        for &amount in &amounts {
            let coin = Coin {
                parent_coin_info,
                puzzle_hash,
                amount,
            };
            let coin_id = coin.coin_id();
            assert_ne!(coin_id, Bytes32::from([0u8; 32]));
        }
    }

    #[test]
    fn test_coin_hash_trait() {
        use std::collections::hash_map::DefaultHasher;

        let coin = Coin {
            parent_coin_info: Bytes32::from([9u8; 32]),
            puzzle_hash: Bytes32::from([10u8; 32]),
            amount: 123456789,
        };
        let mut hasher = DefaultHasher::new();
        coin.hash(&mut hasher);
        let _hash_value = hasher.finish();
        // Just ensuring the hash function runs without panic
    }
}
