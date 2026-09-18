use dg_xch_core::blockchain::coin::Coin;
use dg_xch_core::blockchain::coin_record::CoinRecord;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::traits::SizedBytes;

fn rec(tag: u8) -> CoinRecord {
    CoinRecord {
        coin: Coin {
            parent_coin_info: Bytes32::from([tag; 32]),
            puzzle_hash: Bytes32::from([0x11u8; 32]),
            amount: u64::from(tag),
        },
        confirmed_block_index: 0,
        spent_block_index: 0,
        coinbase: false,
        timestamp: 0,
        spent: false,
    }
}

// The whole point of the sort: after it, the batch is key-contiguous, so the fixed-size
// chunks the apply cuts are runs of adjacent pkey values (shared btree paths, few distinct
// leaf pages) instead of one random descent per row.
#[test]
fn addition_batches_are_key_sorted_with_names_paired() {
    let additions: Vec<CoinRecord> = (0u8..32).map(rec).collect();
    let sorted = crate::sort_additions_by_name(&additions);
    assert_eq!(sorted.len(), additions.len());
    for w in sorted.windows(2) {
        assert!(
            w[0].0.bytes() <= w[1].0.bytes(),
            "chunks must be cut from a key-contiguous batch"
        );
    }
    for (name, cr) in &sorted {
        assert_eq!(*name, cr.coin.name(), "the paired name IS the record's key");
    }
}

#[test]
fn removal_batches_are_key_sorted() {
    let removals: Vec<Bytes32> = (0u8..32).rev().map(|t| rec(t).coin.name()).collect();
    let sorted = crate::sorted_removal_names(&removals);
    assert_eq!(sorted.len(), removals.len());
    for w in sorted.windows(2) {
        assert!(w[0].bytes() <= w[1].bytes());
    }
}
