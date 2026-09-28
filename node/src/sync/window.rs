use std::collections::{BTreeSet, HashMap, HashSet};

// One splittable reservation: a contiguous run of candidate heights handed to a single peer. Holds
// identifiers (~a u32 each), never blocks — the whole point of the O(W·id) bound.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reservation {
    pub id: u64,
    pub heights: Vec<u32>,
}
impl Reservation {
    #[must_use]
    pub fn start(&self) -> u32 {
        self.heights[0]
    }
    #[must_use]
    pub fn end(&self) -> u32 {
        self.heights[self.heights.len() - 1]
    }
}

/// The bounded reservation window. Pending candidate heights are split into per-peer
/// contiguous reservations; a stalled peer's reservation is reclaimed to the pool with no gap. The window
/// holds only identifiers, capped at `capacity`, so peak RAM is flat in chain height.
pub struct ReservationWindow {
    capacity: usize,
    known: BTreeSet<u32>,
    reserved: HashMap<u64, Vec<u32>>,
    reserved_set: HashSet<u32>,
    next_id: u64,
}

/// The result of asking the window for work.
#[derive(Debug, PartialEq, Eq)]
pub enum Claim {
    /// A reservation to download.
    Reserved(Reservation),
    /// Nothing pending and nothing outstanding — the range is fully written through.
    Drained,
    /// Everything pending is currently reserved by other peers — wait for a completion or a reclaim.
    Busy,
}

impl ReservationWindow {
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            known: BTreeSet::new(),
            reserved: HashMap::new(),
            reserved_set: HashSet::new(),
            next_id: 0,
        }
    }

    // Admit newly-discovered pending heights, up to the window cap — never a block, just the identifier.
    // Already-reserved or already-known heights are ignored (idempotent refill from get_unassociated).
    pub fn refill(&mut self, heights: impl IntoIterator<Item = u32>) {
        for h in heights {
            if self.live() >= self.capacity {
                break;
            }
            if !self.reserved_set.contains(&h) {
                self.known.insert(h);
            }
        }
    }

    // Split off the next contiguous run of up to `batch` lowest pending heights for one peer.
    pub fn reserve(&mut self, batch: u32) -> Claim {
        if self.known.is_empty() {
            return if self.reserved.is_empty() {
                Claim::Drained
            } else {
                Claim::Busy
            };
        }
        let start = *self.known.iter().next().expect("non-empty");
        let mut heights = Vec::new();
        let mut h = start;
        while heights.len() < batch as usize && self.known.remove(&h) {
            self.reserved_set.insert(h);
            heights.push(h);
            h += 1;
        }
        let id = self.next_id;
        self.next_id += 1;
        self.reserved.insert(id, heights.clone());
        Claim::Reserved(Reservation { id, heights })
    }

    // A reservation's bodies are written through: retire it.
    pub fn complete(&mut self, id: u64) {
        if let Some(heights) = self.reserved.remove(&id) {
            for h in heights {
                self.reserved_set.remove(&h);
            }
        }
    }

    // A peer stalled: return its heights to the pool for another peer, no gap.
    pub fn reclaim(&mut self, id: u64) {
        if let Some(heights) = self.reserved.remove(&id) {
            for h in heights {
                self.reserved_set.remove(&h);
                self.known.insert(h);
            }
        }
    }

    // Total live identifiers held (pending + reserved) — the O(W·id) quantity, capped at `capacity`.
    #[must_use]
    pub fn live(&self) -> usize {
        self.known.len() + self.reserved_set.len()
    }

    #[must_use]
    pub fn outstanding(&self) -> usize {
        self.reserved.len()
    }
}

#[cfg(test)]
#[path = "../../tests/unit/sync/window/tests.rs"]
mod tests;
