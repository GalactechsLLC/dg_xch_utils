//! v1 accumulator unit vectors. The `naive` module is a DELIBERATE independent
//! reimplementation of the spec text in `tools/src/coin_roots/mod.rs` (straight from the byte-layout
//! rules, no shared code with the streaming accumulator) — every agreement below is the
//! "an independent implementation reproduces it byte-for-byte" check.

use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_dev_tools::coin_roots::{CoinSetAccumulator, RootsError};

/// Naive spec-text implementation: materialize all leaves, build each perfect subtree
/// recursively, bag right-to-left, bind counts, combine.
mod naive {
    use sha2::{Digest, Sha256};

    pub fn h(parts: &[&[u8]]) -> [u8; 32] {
        let mut hasher = Sha256::new();
        for p in parts {
            hasher.update(p);
        }
        hasher.finalize().into()
    }

    pub fn coin_leaf(i: u64, coin_id: &[u8; 32], height: u32, timestamp: u64) -> [u8; 32] {
        h(&[
            b"dgxch.coinroot.v1.coin-leaf",
            &i.to_le_bytes(),
            coin_id,
            &height.to_le_bytes(),
            &timestamp.to_le_bytes(),
        ])
    }

    fn perfect(leaves: &[[u8; 32]]) -> [u8; 32] {
        if leaves.len() == 1 {
            return leaves[0];
        }
        let (l, r) = leaves.split_at(leaves.len() / 2);
        h(&[b"dgxch.coinroot.v1.node", &perfect(l), &perfect(r)])
    }

    /// MMR root over `leaves` with count binding `n` under `domain`.
    pub fn mmr_root(leaves: &[[u8; 32]], n: u64, domain: &[u8]) -> [u8; 32] {
        if leaves.is_empty() {
            return h(&[b"dgxch.coinroot.v1.empty"]);
        }
        // Maximal power-of-two prefixes, left to right (one peak per set bit of the count).
        let mut peaks: Vec<[u8; 32]> = Vec::new();
        let mut rest = leaves;
        while !rest.is_empty() {
            let mut k = 1usize;
            while k * 2 <= rest.len() {
                k *= 2;
            }
            peaks.push(perfect(&rest[..k]));
            rest = &rest[k..];
        }
        // Bag right-to-left.
        let mut acc = *peaks.last().expect("nonempty");
        for p in peaks.iter().rev().skip(1) {
            acc = h(&[b"dgxch.coinroot.v1.bag", p, &acc]);
        }
        h(&[domain, &n.to_le_bytes(), &acc])
    }

    /// Spent-bitmap root for `spent` flags (one per coin leaf), 1024-bit chunks, LSB-first.
    pub fn bitmap_root(spent: &[bool]) -> [u8; 32] {
        let mut chunk_leaves: Vec<[u8; 32]> = Vec::new();
        for (j, chunk_flags) in spent.chunks(1024).enumerate() {
            let mut chunk = [0u8; 128];
            for (i, &s) in chunk_flags.iter().enumerate() {
                if s {
                    chunk[i / 8] |= 1 << (i % 8);
                }
            }
            chunk_leaves.push(h(&[
                b"dgxch.coinroot.v1.bitmap-leaf",
                &(j as u64).to_le_bytes(),
                &chunk,
            ]));
        }
        mmr_root(
            &chunk_leaves,
            spent.len() as u64,
            b"dgxch.coinroot.v1.bitmap-root",
        )
    }

    /// The combined v1 root over coins `(coin_id, confirmed_height, timestamp, spent_as_of_h)`
    /// already in canonical order.
    pub fn root_v1(
        coins: &[([u8; 32], u32, u64, bool)],
        height: u32,
        header_hash: &[u8; 32],
    ) -> [u8; 32] {
        let leaves: Vec<[u8; 32]> = coins
            .iter()
            .enumerate()
            .map(|(i, (id, ch, ts, _))| coin_leaf(i as u64, id, *ch, *ts))
            .collect();
        let n = coins.len() as u64;
        let mmr = mmr_root(&leaves, n, b"dgxch.coinroot.v1.mmr-root");
        let spent: Vec<bool> = coins.iter().map(|(_, _, _, s)| *s).collect();
        let bitmap = bitmap_root(&spent);
        h(&[
            b"dgxch.coinroot.v1.root",
            &mmr,
            &bitmap,
            &height.to_le_bytes(),
            header_hash,
        ])
    }
}

fn id(tag: u8) -> Bytes32 {
    Bytes32::from([tag; 32])
}

const HH: [u8; 32] = [0xAA; 32];

#[test]
fn empty_set_matches_naive() {
    let acc = CoinSetAccumulator::new();
    let root = acc.root_at(0, Bytes32::from(HH)).expect("empty root");
    assert_eq!(root.coin_count, 0);
    assert_eq!(root.spent_count, 0);
    let expected = naive::root_v1(&[], 0, &HH);
    assert_eq!(root.root_v1.const_bytes(), expected);
    // Both subtree roots are the empty-tree constant.
    let empty = naive::h(&[b"dgxch.coinroot.v1.empty"]);
    let mmr = naive::h(&[
        b"dgxch.coinroot.v1.mmr-root".as_slice(),
        &0u64.to_le_bytes(),
    ]);
    // n = 0 never reaches the count binding: the whole tree is the empty constant.
    assert_eq!(root.mmr_root.const_bytes(), empty);
    assert_eq!(root.spent_bitmap_root.const_bytes(), empty);
    assert_ne!(root.mmr_root.const_bytes(), mmr);
}

#[test]
fn single_coin_matches_naive() {
    let mut acc = CoinSetAccumulator::new();
    acc.append(id(1), 5, 1700, 0).expect("append");
    let root = acc.root_at(10, Bytes32::from(HH)).expect("root");
    let expected = naive::root_v1(&[([1u8; 32], 5, 1700, false)], 10, &HH);
    assert_eq!(root.root_v1.const_bytes(), expected);
    assert_eq!(root.coin_count, 1);
    assert_eq!(root.spent_count, 0);
}

#[test]
fn multi_peak_mmr_matches_naive() {
    // 5 coins = peaks of order 2 and 0 — exercises merge and right-to-left bagging.
    let coins: Vec<([u8; 32], u32, u64, bool)> = (0u8..5)
        .map(|i| ([i + 1; 32], u32::from(i) + 2, 1000 + u64::from(i), i == 1))
        .collect();
    let mut acc = CoinSetAccumulator::new();
    for (cid, ch, ts, spent) in &coins {
        let spent_index = if *spent { *ch } else { 0 };
        acc.append(Bytes32::from(*cid), *ch, *ts, spent_index)
            .expect("append");
    }
    let root = acc.root_at(100, Bytes32::from(HH)).expect("root");
    let expected = naive::root_v1(&coins, 100, &HH);
    assert_eq!(root.root_v1.const_bytes(), expected);
    assert_eq!(root.spent_count, 1);
}

#[test]
fn spend_inside_boundary_changes_root_outside_does_not() {
    let mut unspent = CoinSetAccumulator::new();
    unspent.append(id(1), 5, 1700, 0).expect("append");
    let mut spent_late = CoinSetAccumulator::new();
    spent_late.append(id(1), 5, 1700, 50).expect("append");

    let hh = Bytes32::from(HH);
    // As of height 10 the late spend (at 50) is invisible: identical root, spent_count 0.
    let a = unspent.root_at(10, hh).expect("root");
    let b = spent_late.root_at(10, hh).expect("root");
    assert_eq!(a.root_v1.const_bytes(), b.root_v1.const_bytes());
    assert_eq!(b.spent_count, 0);
    // As of height 50 the spend is visible: different root, same MMR, different bitmap.
    let c = spent_late.root_at(50, hh).expect("root");
    let a50 = unspent.root_at(50, hh).expect("root");
    assert_ne!(a50.root_v1.const_bytes(), c.root_v1.const_bytes());
    assert_eq!(a50.mmr_root.const_bytes(), c.mmr_root.const_bytes());
    assert_ne!(
        a50.spent_bitmap_root.const_bytes(),
        c.spent_bitmap_root.const_bytes()
    );
    assert_eq!(c.spent_count, 1);
}

#[test]
fn permuted_insertion_order_changes_root() {
    // RED-FIRST for the order pin: leaf hashes commit to the leaf index, so assigning the
    // same two coins to swapped indices MUST change the MMR root — the canonical-order
    // definition is load-bearing, not cosmetic.
    let a = ([1u8; 32], 5u32, 1700u64, false);
    let b = ([2u8; 32], 5u32, 1700u64, false);
    let canonical = naive::root_v1(&[a, b], 10, &HH);
    let permuted = naive::root_v1(&[b, a], 10, &HH);
    assert_ne!(canonical, permuted);

    // And the streaming accumulator refuses to produce the permuted root at all: same-height
    // coins must arrive in ascending coin_id order.
    let mut acc = CoinSetAccumulator::new();
    acc.append(id(2), 5, 1700, 0).expect("append");
    let err = acc.append(id(1), 5, 1700, 0).expect_err("out of order");
    assert!(matches!(err, RootsError::OutOfOrder { .. }));
    // Descending height is equally rejected.
    let mut acc = CoinSetAccumulator::new();
    acc.append(id(1), 5, 1700, 0).expect("append");
    let err = acc
        .append(id(2), 4, 1700, 0)
        .expect_err("descending height");
    assert!(matches!(err, RootsError::OutOfOrder { .. }));
}

#[test]
fn duplicate_coin_rejected() {
    let mut acc = CoinSetAccumulator::new();
    acc.append(id(1), 5, 1700, 0).expect("append");
    let err = acc.append(id(1), 5, 1700, 0).expect_err("duplicate");
    assert!(matches!(err, RootsError::OutOfOrder { .. }));
}

#[test]
fn spent_before_created_rejected() {
    let mut acc = CoinSetAccumulator::new();
    let err = acc
        .append(id(1), 5, 1700, 4)
        .expect_err("spent below created");
    assert!(matches!(err, RootsError::SpentBeforeCreated { .. }));
    // Same-block ephemeral spend is legal.
    acc.append(id(1), 5, 1700, 5).expect("ephemeral");
}

#[test]
fn boundary_behind_appends_rejected() {
    let mut acc = CoinSetAccumulator::new();
    acc.append(id(1), 5, 1700, 0).expect("append");
    let err = acc
        .root_at(4, Bytes32::from(HH))
        .expect_err("boundary behind");
    assert!(matches!(err, RootsError::BoundaryBehindAppends { .. }));
}

#[test]
fn bitmap_chunk_boundary_matches_naive() {
    // 1025 coins spanning two 1024-bit chunks, with spends sprinkled across both chunks.
    let coins: Vec<([u8; 32], u32, u64, bool)> = (0u32..1025)
        .map(|i| {
            let mut cid = [0u8; 32];
            cid[..4].copy_from_slice(&i.to_be_bytes());
            (cid, 7, 1234, i % 5 == 0)
        })
        .collect();
    let mut acc = CoinSetAccumulator::new();
    for (cid, ch, ts, spent) in &coins {
        acc.append(Bytes32::from(*cid), *ch, *ts, if *spent { 9 } else { 0 })
            .expect("append");
    }
    let root = acc.root_at(20, Bytes32::from(HH)).expect("root");
    let expected = naive::root_v1(&coins, 20, &HH);
    assert_eq!(root.root_v1.const_bytes(), expected);
    assert_eq!(
        root.spent_count,
        coins.iter().filter(|c| c.3).count() as u64
    );
}

#[test]
fn known_vector_pins_v1_layout() {
    // A frozen vector: any change to domain strings, endianness, order, or structure fails
    // here. Independent implementations must reproduce this exact value.
    let mut acc = CoinSetAccumulator::new();
    acc.append(id(1), 1, 1000, 0).expect("append");
    acc.append(id(2), 2, 2000, 3).expect("append");
    acc.append(id(3), 3, 3000, 0).expect("append");
    let root = acc.root_at(4, Bytes32::from([0xBB; 32])).expect("root");
    assert_eq!(
        format!("{}", root.root_v1),
        // Computed once from the naive spec implementation; frozen forever for v1.
        format!(
            "{}",
            Bytes32::from(naive::root_v1(
                &[
                    ([1u8; 32], 1, 1000, false),
                    ([2u8; 32], 2, 2000, true),
                    ([3u8; 32], 3, 3000, false),
                ],
                4,
                &[0xBB; 32],
            ))
        )
    );
    assert_eq!(
        format!("{}", root.root_v1),
        "0xa0c562262c66b35a041e5c386ceef3002acc2dcd9cb6726593bfa0d69463d112"
    );
}
