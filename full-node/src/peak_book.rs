use dg_xch_core::blockchain::sized_bytes::Bytes32;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Cap on tracked claims.
pub const MAX_TRACKED_CLAIMS: usize = 256;
/// Cap on the quarantine cache.
pub const BAD_PEAK_CACHE_SIZE: usize = 100;
/// Claim-liveness backstop: a claim not re-announced within this window stops being selectable.
pub const STALE_CLAIM_TTL: Duration = Duration::from_secs(300);

/// One peer's announced peak (`header_hash`, `height`, `weight`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PeakClaim {
    pub header_hash: Bytes32,
    pub height: u32,
    pub weight: u128,
}

struct Entry {
    claim: PeakClaim,
    /// Inbound claims are keyed by the server-side peer id and reconciled against the live inbound
    /// map each driver tick; outbound claims are keyed by a minted per-connection id and retracted
    /// by the connection's [`ClaimGuard`] drop.
    inbound: bool,
    recorded_at: Instant,
}

struct Inner {
    claims: HashMap<Bytes32, Entry>,
    /// Quarantined peak hashes with the height they claimed.
    bad: Vec<(Bytes32, u32)>,
    /// The last published heaviest claim, for change detection (the tip-follower wake signal).
    published: Option<PeakClaim>,
}

pub struct PeakBook {
    published_height: Arc<AtomicU32>,
    next_outbound_key: AtomicU64,
    inner: Mutex<Inner>,
}

impl PeakBook {
    #[must_use]
    pub fn new(published_height: Arc<AtomicU32>) -> Self {
        Self {
            published_height,
            next_outbound_key: AtomicU64::new(1),
            inner: Mutex::new(Inner {
                claims: HashMap::new(),
                bad: Vec::new(),
                published: None,
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Mint the claim key + RAII retraction for one OUTBOUND connection. The outbound dispatch path
    /// hands every connection our own cert hash as the peer id (the dial's `peer_id` is derived
    /// from the client cert), so per-connection identity must be minted here instead; the guard's
    /// `Drop` is the disconnect retraction, fired when the connection's handler map goes.
    #[must_use]
    pub fn outbound_guard(self: &Arc<Self>) -> ClaimGuard {
        let n = self.next_outbound_key.fetch_add(1, Ordering::Relaxed);
        // A minted key can never collide with a real inbound peer id (a cert hash): tagged prefix.
        let mut key = [0u8; 32];
        key[..8].copy_from_slice(b"outbound");
        key[24..].copy_from_slice(&n.to_be_bytes());
        ClaimGuard {
            book: self.clone(),
            key: Bytes32::const_new(key),
        }
    }

    /// Record `key`'s announced peak, replacing its previous claim: one claim per peer, newest
    /// announcement wins (which is also how an honest peer WITHDRAWS an over-claim: its next
    /// announcement replaces it). Returns `true` when the published heaviest claim changed (the
    /// tip-follower wake condition).
    pub fn record(&self, key: Bytes32, inbound: bool, claim: PeakClaim) -> bool {
        let mut g = self.lock();
        let now = Instant::now();
        if g.claims.len() >= MAX_TRACKED_CLAIMS && !g.claims.contains_key(&key) {
            // At the cap, evict the stalest entry.
            if let Some(oldest) = g
                .claims
                .iter()
                .min_by_key(|(_, e)| e.recorded_at)
                .map(|(k, _)| *k)
            {
                g.claims.remove(&oldest);
            }
        }
        g.claims.insert(
            key,
            Entry {
                claim,
                inbound,
                recorded_at: now,
            },
        );
        self.republish(&mut g)
    }

    /// Retract `key`'s claim (the disconnect path).
    pub fn retract(&self, key: &Bytes32) {
        let mut g = self.lock();
        if g.claims.remove(key).is_some() {
            self.republish(&mut g);
        }
    }

    /// Retract every claim on `header_hash`, whoever made it. Driven when no peer will actually
    /// serve the claimed tip (the weight-proof fetch failed from every peer): an honest claimant
    /// re-announces within a block cadence, a phantom claim stays gone.
    pub fn retract_hash(&self, header_hash: &Bytes32) {
        let mut g = self.lock();
        let before = g.claims.len();
        g.claims.retain(|_, e| e.claim.header_hash != *header_hash);
        if g.claims.len() != before {
            self.republish(&mut g);
        }
    }

    /// Per-tick reconcile: drop inbound claims whose peer left the live inbound map and every
    /// claim past [`STALE_CLAIM_TTL`], then republish so a retraction rolls the claimed gauge
    /// back within one driver tick.
    pub fn reconcile(&self, live_inbound: &std::collections::HashSet<Bytes32>) {
        let mut g = self.lock();
        let now = Instant::now();
        g.claims.retain(|key, e| {
            (!e.inbound || live_inbound.contains(key))
                && now.duration_since(e.recorded_at) <= STALE_CLAIM_TTL
        });
        self.republish(&mut g);
    }

    /// Quarantine a peak hash whose weight proof failed to attest it. A quarantined hash is never
    /// selectable again (until evicted by the cache bound), so a poisoned peak cannot be
    /// re-selected every tick.
    pub fn quarantine(&self, header_hash: Bytes32, height: u32) {
        let mut g = self.lock();
        if !g.bad.iter().any(|(h, _)| *h == header_hash) {
            g.bad.push((header_hash, height));
            if g.bad.len() > BAD_PEAK_CACHE_SIZE {
                // Evict the minimum-height entry when over the cap.
                if let Some(min_idx) = g
                    .bad
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, (_, h))| *h)
                    .map(|(i, _)| i)
                {
                    g.bad.swap_remove(min_idx);
                }
            }
        }
        self.republish(&mut g);
    }

    #[must_use]
    pub fn is_quarantined(&self, header_hash: &Bytes32) -> bool {
        self.lock().bad.iter().any(|(h, _)| h == header_hash)
    }

    /// The heaviest selectable claim, minus quarantined hashes and stale entries. Ties break to
    /// the taller claim for determinism.
    #[must_use]
    pub fn heaviest(&self) -> Option<PeakClaim> {
        let g = self.lock();
        Self::heaviest_of(&g, Instant::now())
    }

    fn heaviest_of(g: &Inner, now: Instant) -> Option<PeakClaim> {
        g.claims
            .values()
            .filter(|e| now.duration_since(e.recorded_at) <= STALE_CLAIM_TTL)
            .map(|e| &e.claim)
            .filter(|c| !g.bad.iter().any(|(h, _)| *h == c.header_hash))
            .max_by_key(|c| (c.weight, c.height))
            .copied()
    }

    /// The highest peak height claimed by an OUTBOUND peer — the peers the fetch producer actually
    /// requests blocks from. The weight-heaviest target ([`PeakBook::heaviest`]) can ride an INBOUND
    /// claim (a peer that dialed US, which we never fetch from) past this servable tip; the producer
    /// clamps its fetch frontier here so it never requests a range no fetch source can serve — a
    /// beyond-tip range every peer rejects, spun every tick. `None` when no live outbound peer has
    /// announced a peak yet (startup), where the caller leaves the frontier unclamped.
    #[must_use]
    pub fn outbound_tip(&self) -> Option<u32> {
        let g = self.lock();
        let now = Instant::now();
        g.claims
            .values()
            .filter(|e| !e.inbound && now.duration_since(e.recorded_at) <= STALE_CLAIM_TTL)
            .filter(|e| !g.bad.iter().any(|(h, _)| *h == e.claim.header_hash))
            .map(|e| e.claim.height)
            .max()
    }

    // Recompute + publish the heaviest claim; report whether it changed.
    fn republish(&self, g: &mut Inner) -> bool {
        let heaviest = Self::heaviest_of(g, Instant::now());
        self.published_height
            .store(heaviest.map_or(0, |c| c.height), Ordering::Relaxed);
        let changed = heaviest != g.published;
        g.published = heaviest;
        changed
    }
}

/// RAII retraction for one outbound connection's claim: dropped with the connection's handler
/// map, it retracts the claim.
pub struct ClaimGuard {
    book: Arc<PeakBook>,
    key: Bytes32,
}

impl ClaimGuard {
    #[must_use]
    pub fn key(&self) -> Bytes32 {
        self.key
    }
}

impl Drop for ClaimGuard {
    fn drop(&mut self) {
        self.book.retract(&self.key);
    }
}

#[cfg(test)]
#[path = "../tests/unit/peak_book.rs"]
mod tests;
