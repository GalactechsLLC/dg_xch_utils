use super::*;
use dg_xch_core::blockchain::sized_bytes::Bytes96;

// The test cost fallback is `MAX_BLOCK_COST_CLVM / 2`; the DRR vector below uses 20.
const BIG_COST: u64 = 11_000_000_000 / 2;

fn empty_bundle() -> SpendBundle {
    SpendBundle {
        coin_spends: vec![],
        aggregated_signature: Bytes96::from([0u8; 96]),
    }
}

fn peer(byte: u8) -> Bytes32 {
    Bytes32::from([byte; 32])
}

// A trusted peer's bundle jumps an already-queued untrusted bundle: high priority routes to
// the separate lane the drain empties first.
#[test]
fn trusted_bundle_jumps_untrusted_backlog() {
    let mut q = TxQueue::new(256, 32, BIG_COST);
    let untrusted = peer(0x11);
    let trusted = peer(0x22);
    assert!(q.push(untrusted, empty_bundle(), false, 100, 1000));
    assert!(q.push(trusted, empty_bundle(), true, 0, 0));
    let batch = q.drain_batch();
    assert_eq!(batch.len(), 2);
    assert_eq!(batch[0].0, trusted, "trusted (high-priority) drains first");
    assert_eq!(batch[1].0, untrusted, "untrusted backlog follows");
    assert!(q.is_empty());
}

// WITHIN one peer's lane the queue drains by advertised fee-per-cost, highest FIRST.
// Low-fpc inserted first, high-fpc second — the high one pops first.
#[test]
fn within_a_peer_lane_highest_fee_per_cost_drains_first() {
    let mut q = TxQueue::new(256, 32, BIG_COST);
    let p = peer(0x11);
    let low = SpendBundle {
        coin_spends: vec![],
        aggregated_signature: Bytes96::from([1u8; 96]),
    };
    let high = SpendBundle {
        coin_spends: vec![],
        aggregated_signature: Bytes96::from([2u8; 96]),
    };
    assert!(q.push(p, low, false, 100, 1000));
    assert!(q.push(p, high.clone(), false, 900, 1000));
    let batch = q.drain_batch();
    assert_eq!(batch.len(), 2);
    assert_eq!(
        batch[0].1.aggregated_signature, high.aggregated_signature,
        "the peer's higher-fpc bundle validates first"
    );
}

// Validation order round-robins ACROSS peers by CLVM-cost deficit: one peer's high-fpc
// stream must NOT be serviced ahead of every other peer's backlog. Two peers, three equal-cost bundles each, one peer advertising 90x the fee:
// the drain must interleave A,B,A,B,A,B (each pop spends the peer's deficit and the cursor
// moves on), not A,A,A,B,B,B.
#[test]
fn drain_interleaves_peers_by_cost_deficit_round_robin() {
    let mut q = TxQueue::new(256, 32, BIG_COST);
    let rich = peer(0xAA);
    let poor = peer(0xBB);
    for _ in 0..3 {
        assert!(q.push(rich, empty_bundle(), false, 900, 10));
    }
    for _ in 0..3 {
        assert!(q.push(poor, empty_bundle(), false, 10, 10));
    }
    let batch = q.drain_batch();
    let order: Vec<Bytes32> = batch.iter().map(|(p, _)| *p).collect();
    assert_eq!(
        order,
        vec![rich, poor, rich, poor, rich, poor],
        "equal-cost backlogs from two peers must interleave (deficit round robin), \
         not drain the high-fee peer to exhaustion first"
    );
}

// The deficit-round-robin vector: four peers with top costs 15 / 5 / 10 / no-cost-info
// (fallback max_tx_clvm_cost = 20), equal fee 42. Deficit replenishment picks
// the LOWEST top cost each round, so the service order is peer2 (cost 5), peer3 (10),
// peer1 (15), peer4 (fallback 20).
#[test]
fn chia_deficit_round_robin_vector_orders_by_affordability() {
    let mut q = TxQueue::new(256, 32, 20);
    let p1 = peer(1);
    let p2 = peer(2);
    let p3 = peer(3);
    let p4 = peer(4);
    assert!(q.push(p1, empty_bundle(), false, 42, 15));
    assert!(q.push(p2, empty_bundle(), false, 42, 5));
    assert!(q.push(p3, empty_bundle(), false, 42, 10));
    assert!(q.push(p4, empty_bundle(), false, 42, 0)); // no cost info → fallback 20
    let order: Vec<Bytes32> = q.drain_batch().iter().map(|(p, _)| *p).collect();
    assert_eq!(
        order,
        vec![p2, p3, p1, p4],
        "DRR: the cheapest affordable top transaction services first, \
         the no-cost-info peer prices at max_tx_clvm_cost and goes last"
    );
}

// A zero/unknown advertised cost prices at the max_tx_clvm_cost fallback, so it drains after
// a known-cost entry: it sorts last in the lane and prices at the highest DRR fallback.
#[test]
fn unknown_cost_untrusted_entry_drains_last() {
    let mut q = TxQueue::new(256, 32, BIG_COST);
    let unknown = peer(0x11);
    let known = peer(0x22);
    assert!(q.push(unknown, empty_bundle(), false, 0, 0));
    assert!(q.push(known, empty_bundle(), false, 10, 1000));
    let batch = q.drain_batch();
    assert_eq!(
        batch[0].0, known,
        "known fee-per-cost drains before unknown-cost"
    );
    assert_eq!(batch[1].0, unknown, "unknown-cost drains last");
}

// Equal fee-per-cost and equal cost across two peers: the round robin services them in
// arrival (registration) order — first-registered peer first.
#[test]
fn equal_fee_per_cost_keeps_insertion_order() {
    let mut q = TxQueue::new(256, 32, BIG_COST);
    let first = peer(0x11);
    let second = peer(0x22);
    assert!(q.push(first, empty_bundle(), false, 500, 1000));
    assert!(q.push(second, empty_bundle(), false, 500, 1000));
    let batch = q.drain_batch();
    assert_eq!(
        batch[0].0, first,
        "equal fpc: first-registered drains first"
    );
    assert_eq!(batch[1].0, second);
}

// The untrusted lane keeps the anti-spam bounds; the high lane does not.
#[test]
fn untrusted_lane_bounds_hold_high_lane_is_unbounded() {
    let mut q = TxQueue::new(3, 2, BIG_COST);
    let spammer = peer(0x11);
    // per-peer cap = 2: the third untrusted push from one peer is dropped.
    assert!(q.push(spammer, empty_bundle(), false, 1, 1000));
    assert!(q.push(spammer, empty_bundle(), false, 1, 1000));
    assert!(
        !q.push(spammer, empty_bundle(), false, 1, 1000),
        "per-peer cap holds"
    );
    // A trusted peer is never throttled — well past the aggregate cap.
    for _ in 0..10 {
        assert!(q.push(peer(0x22), empty_bundle(), true, 0, 0));
    }
    assert_eq!(q.len(), 12);
}

// Multiple high-priority entries drain in FIFO order among themselves.
#[test]
fn high_priority_lane_is_fifo() {
    let mut q = TxQueue::new(256, 32, BIG_COST);
    assert!(q.push(peer(0x01), empty_bundle(), true, 0, 0));
    assert!(q.push(peer(0x02), empty_bundle(), true, 0, 0));
    let batch = q.drain_batch();
    assert_eq!(batch[0].0, peer(0x01));
    assert_eq!(batch[1].0, peer(0x02));
}

// Draining resets the round-robin bookkeeping; a fresh backlog starts from a clean cursor
// and clean deficits.
#[test]
fn state_resets_between_batches() {
    let mut q = TxQueue::new(256, 32, BIG_COST);
    let a = peer(0x0A);
    let b = peer(0x0B);
    assert!(q.push(a, empty_bundle(), false, 1, 10));
    assert!(q.push(b, empty_bundle(), false, 1, 10));
    let first = q.drain_batch();
    assert_eq!(first.len(), 2);
    assert!(q.is_empty());
    // Second round, reversed registration order: b registers first now and services first.
    assert!(q.push(b, empty_bundle(), false, 1, 10));
    assert!(q.push(a, empty_bundle(), false, 1, 10));
    let second = q.drain_batch();
    assert_eq!(
        second.iter().map(|(p, _)| *p).collect::<Vec<_>>(),
        vec![b, a],
        "a drained queue carries no cursor/deficit residue into the next batch"
    );
}
