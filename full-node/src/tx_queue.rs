// The gossip-transaction admission queue with a trusted priority lane. This is the bounded
// The websocket read loop fills this inbox and the transaction validator drains it.
//
// Priority semantics: a high-priority (trusted, or local) entry goes into the high-priority
// queue, which `pop()` drains ENTIRELY before it ever touches the per-peer untrusted queues.
//
// UNTRUSTED-LANE ORDERING is a deficit round robin across peers, by advertised CLVM cost.
//   - Each untrusted peer has its own queue ordered by advertised fee-per-cost, highest first;
//     a no-cost-info entry sorts last.
//   - `pop()` walks the peers round-robin from a cursor. A peer may send its TOP transaction
//     only when its cost DEFICIT covers the transaction's advertised cost (falling back to
//     `max_tx_clvm_cost` when the peer advertised no cost); the pop spends the deficit, resets
//     it to zero when the peer's queue empties, and advances the cursor to the NEXT peer. When
//     no peer can afford its top transaction, the LOWEST top-cost among peers with queued
//     transactions is added to every such peer's deficit and the walk repeats.
//   This bounds the effect of one peer spamming the node: a peer's high-fpc stream cannot
//   monopolize validation order — service interleaves across peers by cost.
//
// Bounds: a per-peer cap AND an aggregate cap on the whole untrusted backlog. The advertised
// fee/cost affect ONLY service order, never admission — every bundle is validated at its TRUE
// fee downstream.
//
// The drain is batch-total: `drain_batch` empties every lane, so the per-peer state is reset in
// full at the end of the drain. A peer's deficit is already zeroed the moment its queue empties,
// so the post-drain reset is state-identical.

use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::blockchain::spend_bundle::SpendBundle;
use std::cmp::Ordering;
use std::collections::{HashMap, VecDeque};

// One untrusted-lane entry: the bundle and the ADVERTISED fee + cost (from the peer's
// `NewTransaction`) the lane orders on.
struct UntrustedTx {
    bundle: SpendBundle,
    advertised_fee: u64,
    advertised_cost: u64,
}

// Order two untrusted entries by advertised fee-per-cost, HIGHEST first. fpc = fee/cost compared
// without division via cross-multiplication in u128 (advertised_fee <= ~2^50, advertised_cost <
// 2^33, so the product never overflows u128). An entry with zero/unknown advertised cost has no
// fpc and sorts LAST.
fn fee_per_cost_desc(a: &UntrustedTx, b: &UntrustedTx) -> Ordering {
    match (a.advertised_cost, b.advertised_cost) {
        (0, 0) => Ordering::Equal,
        (0, _) => Ordering::Greater, // a has no fpc → sorts after b
        (_, 0) => Ordering::Less,    // b has no fpc → a sorts before b
        (ac, bc) => {
            let lhs = u128::from(a.advertised_fee) * u128::from(bc);
            let rhs = u128::from(b.advertised_fee) * u128::from(ac);
            // DESC: the higher fpc compares Less so it lands first.
            rhs.cmp(&lhs)
        }
    }
}

// A peer's untrusted lane: its fee-ordered queue plus its DRR cost deficit.
#[derive(Default)]
struct PeerLane {
    // fpc-descending; FIFO among equal fpc (insertion keeps arrival order within a priority).
    queue: VecDeque<UntrustedTx>,
    // The peer's deficit, in CLVM cost units.
    deficit: u64,
}

/// A bounded gossip-transaction inbox with a trusted high-priority lane and a deficit-round-robin
/// untrusted lane.
pub struct TxQueue {
    // Trusted (and local) transactions, drained in full before the untrusted lanes. Unbounded:
    // trust is the admission control here. FIFO.
    high: VecDeque<(Bytes32, SpendBundle)>,
    // Per-peer untrusted lanes.
    lanes: HashMap<Bytes32, PeerLane>,
    // Round-robin order + cursor.
    order: Vec<Bytes32>,
    cursor: usize,
    // Total entries across all untrusted lanes (the aggregate bound's accounting).
    total_untrusted: usize,
    // Aggregate cap on the untrusted lane, on top of the per-peer bound.
    cap: usize,
    // Per-peer cap; a put past it is refused.
    per_peer: usize,
    // The DRR cost fallback for entries with no advertised cost; `MAX_BLOCK_COST_CLVM / 2`.
    max_tx_clvm_cost: u64,
}

impl TxQueue {
    /// A queue with the given untrusted-lane bounds and the DRR cost fallback. The
    /// high-priority lane is unbounded.
    #[must_use]
    pub fn new(cap: usize, per_peer: usize, max_tx_clvm_cost: u64) -> Self {
        Self {
            high: VecDeque::new(),
            lanes: HashMap::new(),
            order: Vec::new(),
            cursor: 0,
            total_untrusted: 0,
            cap,
            per_peer,
            // A zero fallback would let a costless entry pop for free forever, so floor it.
            max_tx_clvm_cost: max_tx_clvm_cost.max(1),
        }
    }

    /// Enqueue `bundle` from `peer`. `high_priority` (a trusted peer) routes to the unbounded
    /// priority lane and always succeeds; an untrusted bundle is admitted to its peer's lane only
    /// if BOTH the aggregate and the per-peer bound have room — otherwise it is dropped on the
    /// floor.
    /// `advertised_fee`/`advertised_cost` are the peer's `NewTransaction` values; they order the
    /// lane and price the DRR pop, never admission. Returns whether it was admitted.
    pub fn push(
        &mut self,
        peer: Bytes32,
        bundle: SpendBundle,
        high_priority: bool,
        advertised_fee: u64,
        advertised_cost: u64,
    ) -> bool {
        if high_priority {
            self.high.push_back((peer, bundle));
            return true;
        }
        if self.total_untrusted >= self.cap {
            return false;
        }
        if !self.lanes.contains_key(&peer) {
            self.order.push(peer);
        }
        let lane = self.lanes.entry(peer).or_default();
        if lane.queue.len() >= self.per_peer {
            return false;
        }
        let entry = UntrustedTx {
            bundle,
            advertised_fee,
            advertised_cost,
        };
        // Stable fpc-desc insert: after every entry with fpc >= the new one.
        let pos = lane
            .queue
            .iter()
            .position(|e| fee_per_cost_desc(e, &entry) == Ordering::Greater)
            .unwrap_or(lane.queue.len());
        lane.queue.insert(pos, entry);
        self.total_untrusted += 1;
        true
    }

    // The DRR cost of a lane's top entry — the advertised cost, or `max_tx_clvm_cost` when the
    // peer sent no cost info; an unknown cost falls back to the highest.
    fn top_cost(&self, lane: &PeerLane) -> Option<u64> {
        lane.queue.front().map(|e| {
            if e.advertised_cost > 0 {
                e.advertised_cost
            } else {
                self.max_tx_clvm_cost
            }
        })
    }

    // One deficit-round-robin pop from the untrusted lanes.
    fn pop_untrusted(&mut self) -> Option<(Bytes32, SpendBundle)> {
        if self.total_untrusted == 0 {
            return None;
        }
        loop {
            let n = self.order.len();
            debug_assert!(n > 0, "total_untrusted > 0 implies peers in the order map");
            let mut lowest_top_cost: Option<u64> = None;
            for offset in 0..n {
                let idx = (self.cursor + offset) % n;
                let peer = self.order[idx];
                let Some(lane) = self.lanes.get(&peer) else {
                    continue;
                };
                let Some(cost) = self.top_cost(lane) else {
                    continue; // empty lane
                };
                lowest_top_cost = Some(lowest_top_cost.map_or(cost, |m: u64| m.min(cost)));
                if lane.deficit >= cost {
                    // This peer can afford its top transaction.
                    let lane = self.lanes.get_mut(&peer).expect("lane exists");
                    let entry = lane.queue.pop_front().expect("top exists");
                    lane.deficit -= cost;
                    if lane.queue.is_empty() {
                        lane.deficit = 0;
                    }
                    self.cursor = (idx + 1) % n;
                    self.total_untrusted -= 1;
                    return Some((peer, entry.bundle));
                }
            }
            // No peer could afford its top transaction: add the lowest top-cost to every peer
            // with queued transactions and try again.
            let add = lowest_top_cost?;
            for peer in &self.order {
                if let Some(lane) = self.lanes.get_mut(peer)
                    && !lane.queue.is_empty()
                {
                    lane.deficit = lane.deficit.saturating_add(add);
                }
            }
        }
    }

    /// Drain the whole queue for the validator worker's batch pass — the high-priority lane first
    /// (FIFO), then the untrusted lanes in deficit-round-robin order. The per-peer state is
    /// reset afterwards (see the module docs).
    pub fn drain_batch(&mut self) -> Vec<(Bytes32, SpendBundle)> {
        let mut out = Vec::with_capacity(self.high.len() + self.total_untrusted);
        out.extend(self.high.drain(..));
        while let Some(entry) = self.pop_untrusted() {
            out.push(entry);
        }
        // Every lane is empty now (deficits zeroed on empty); drop the bookkeeping.
        self.lanes.clear();
        self.order.clear();
        self.cursor = 0;
        out
    }

    /// Drop every queued bundle — the not-synced transition flush (no transactions while
    /// syncing: the worker clears the inbox rather than validate stale
    /// spends).
    pub fn clear(&mut self) {
        self.high.clear();
        self.lanes.clear();
        self.order.clear();
        self.cursor = 0;
        self.total_untrusted = 0;
    }

    /// Total queued across both lanes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.high.len() + self.total_untrusted
    }

    /// Whether both lanes are empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.high.is_empty() && self.total_untrusted == 0
    }
}

#[cfg(test)]
#[path = "../tests/unit/tx_queue.rs"]
mod tests;
