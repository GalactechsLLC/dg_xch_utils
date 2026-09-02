use crate::config::P2pSettings;
use dg_xch_core::blockchain::peer_info::TimestampedPeerInfo;
use rand::seq::IndexedRandom;
use std::collections::{HashSet, VecDeque};
use std::path::Path;

pub type Endpoint = (String, u16);

fn endpoint(p: &TimestampedPeerInfo) -> Endpoint {
    (p.host.clone(), p.port)
}

// libbitcoin `hosts`: a bounded ring (circular_buffer host_pool_capacity), deduped
// on intake against the pooled + reserved + self sets, randomly selected, aged.
pub struct AddressBook {
    pool: VecDeque<TimestampedPeerInfo>,
    pooled: HashSet<Endpoint>,
    reserved: HashSet<Endpoint>,
    selfs: HashSet<Endpoint>,
    // Addresses sitting out a re-dial cooldown (a session that died young: a peer at capacity
    // accepting-then-closing, or one rejecting fetches). Consulted by `take`; expired entries
    // are pruned there. On a fully-cooling pool `take` still serves — never a wedge.
    cooling: std::collections::HashMap<Endpoint, std::time::Instant>,
    capacity: usize,
    address_lower: usize,
    address_upper: usize,
}

impl AddressBook {
    #[must_use]
    pub fn new(settings: &P2pSettings) -> Self {
        Self {
            pool: VecDeque::with_capacity(settings.host_pool_capacity),
            pooled: HashSet::new(),
            reserved: HashSet::new(),
            selfs: HashSet::new(),
            cooling: std::collections::HashMap::new(),
            capacity: settings.host_pool_capacity,
            address_lower: settings.address_lower,
            address_upper: settings.address_upper,
        }
    }

    pub fn add_self(&mut self, host: &str, port: u16) {
        self.selfs.insert((host.to_string(), port));
    }

    #[must_use]
    pub fn is_self(&self, host: &str, port: u16) -> bool {
        self.selfs.contains(&(host.to_string(), port))
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.pool.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pool.is_empty()
    }

    // Flood-bounded intake: dedup against pooled/reserved/self, evict oldest when full
    // (the ring never grows past capacity no matter how many are gossiped). Returns the
    // count actually accepted (libbitcoin hosts.cpp save).
    pub fn insert_many(&mut self, peers: &[TimestampedPeerInfo]) -> usize {
        let mut accepted = 0;
        for p in peers {
            let ep = endpoint(p);
            if self.selfs.contains(&ep) || self.reserved.contains(&ep) || self.pooled.contains(&ep)
            {
                continue;
            }
            if self.pool.len() >= self.capacity
                && let Some(old) = self.pool.pop_front()
            {
                self.pooled.remove(&endpoint(&old));
            }
            self.pooled.insert(ep);
            self.pool.push_back(p.clone());
            accepted += 1;
        }
        accepted
    }

    // Random dial candidate; moved to the reserved (connected) set. Cooling addresses are
    // passed over while any non-cooling candidate exists; a fully-cooling pool still serves
    // (availability over politeness when starved).
    pub fn take(&mut self) -> Option<TimestampedPeerInfo> {
        if self.pool.is_empty() {
            return None;
        }
        let now = std::time::Instant::now();
        self.cooling.retain(|_, until| *until > now);
        let eligible: Vec<usize> = (0..self.pool.len())
            .filter(|i| {
                self.pool
                    .get(*i)
                    .is_some_and(|p| !self.cooling.contains_key(&endpoint(p)))
            })
            .collect();
        let idx = match eligible.as_slice() {
            [] => rand::random_range(0..self.pool.len()),
            some => *some.choose(&mut rand::rng())?,
        };
        let picked = self.pool.remove(idx)?;
        let ep = endpoint(&picked);
        self.pooled.remove(&ep);
        self.reserved.insert(ep);
        Some(picked)
    }

    // Reclaim with a re-dial cooldown — for a session that died young.
    pub fn reclaim_after(&mut self, peer: &TimestampedPeerInfo, cooldown: std::time::Duration) {
        self.cooling
            .insert(endpoint(peer), std::time::Instant::now() + cooldown);
        self.reclaim(peer, false);
    }

    // On channel stop: drop the reservation; a non-violating peer is returned to the
    // pool only if it is not full, so timeouts cannot drain the pool
    // (libbitcoin session_outbound.cpp reclaim policy, commit fe23205de).
    pub fn reclaim(&mut self, peer: &TimestampedPeerInfo, forget: bool) {
        let ep = endpoint(peer);
        self.reserved.remove(&ep);
        if forget || self.pooled.contains(&ep) {
            return;
        }
        if self.pool.len() < self.capacity {
            self.pooled.insert(ep);
            self.pool.push_back(peer.clone());
        }
    }

    // Randomized-size subset for RespondPeers gossip so pool size is not fingerprinted
    // (libbitcoin hosts.cpp fetch, address_lower..address_upper).
    #[must_use]
    pub fn fetch(&self) -> Vec<TimestampedPeerInfo> {
        if self.pool.is_empty() {
            return Vec::new();
        }
        let hi = self.address_upper.min(self.pool.len());
        let lo = self.address_lower.min(hi).max(1);
        let n = if lo == hi {
            lo
        } else {
            rand::random_range(lo..=hi)
        };
        let mut rng = rand::rng();
        self.pool
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .sample(&mut rng, n)
            .cloned()
            .collect()
    }

    // Drop entries older than the recent-peer threshold.
    pub fn age(&mut self, now: u64, threshold_secs: u64) {
        let cutoff = now.saturating_sub(threshold_secs);
        let mut kept = VecDeque::with_capacity(self.pool.len());
        self.pooled.clear();
        while let Some(p) = self.pool.pop_front() {
            if p.timestamp >= cutoff {
                self.pooled.insert(endpoint(&p));
                kept.push_back(p);
            }
        }
        self.pool = kept;
    }

    // Persisted hosts file (libbitcoin hosts load/save): a restart does not re-bootstrap
    // from cold. Line-oriented `host\tport\ttimestamp`, deduped through the normal intake.
    #[must_use]
    pub fn serialize(&self) -> String {
        let mut out = String::new();
        for p in &self.pool {
            out.push_str(&format!("{}\t{}\t{}\n", p.host, p.port, p.timestamp));
        }
        out
    }

    pub fn load_str(&mut self, data: &str) -> usize {
        let peers: Vec<TimestampedPeerInfo> = data
            .lines()
            .filter_map(|l| {
                let mut it = l.split('\t');
                Some(TimestampedPeerInfo {
                    host: it.next()?.to_string(),
                    port: it.next()?.parse().ok()?,
                    timestamp: it.next()?.parse().ok()?,
                })
            })
            .collect();
        self.insert_many(&peers)
    }

    /// # Errors
    /// Returns [`std::io::Error`] if the file cannot be written.
    pub fn save_file(&self, path: &Path) -> std::io::Result<()> {
        std::fs::write(path, self.serialize())
    }

    /// # Errors
    /// Returns [`std::io::Error`] if the file cannot be read.
    pub fn load_file(&mut self, path: &Path) -> std::io::Result<usize> {
        Ok(self.load_str(&std::fs::read_to_string(path)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(host: &str, port: u16, ts: u64) -> TimestampedPeerInfo {
        TimestampedPeerInfo {
            host: host.to_string(),
            port,
            timestamp: ts,
        }
    }

    #[test]
    fn gossiped_peers_are_stored_and_deduped_on_intake() {
        let mut book = AddressBook::new(&P2pSettings::default());
        let accepted = book.insert_many(&[peer("1.1.1.1", 8444, 10), peer("2.2.2.2", 8444, 10)]);
        assert_eq!(accepted, 2);
        // re-gossip of the same endpoints is fully deduped
        let again = book.insert_many(&[peer("1.1.1.1", 8444, 99), peer("2.2.2.2", 8444, 99)]);
        assert_eq!(again, 0);
        assert_eq!(book.len(), 2);
    }

    #[test]
    fn a_flood_of_junk_holds_the_ring_at_its_cap() {
        let settings = P2pSettings {
            host_pool_capacity: 64,
            ..P2pSettings::default()
        };
        let mut book = AddressBook::new(&settings);
        let flood: Vec<_> = (0..100_000u32)
            .map(|i| {
                let o = i.to_le_bytes();
                peer(&format!("{}.{}.{}.{}", o[0], o[1], o[2], o[3]), 8444, 1)
            })
            .collect();
        let accepted = book.insert_many(&flood);
        assert_eq!(book.len(), 64, "ring bounded at capacity under flood");
        assert!(accepted <= 100_000);
    }

    #[test]
    fn take_moves_to_reserved_and_reclaim_returns() {
        let mut book = AddressBook::new(&P2pSettings::default());
        book.insert_many(&[peer("1.1.1.1", 8444, 10)]);
        let taken = book.take().expect("a candidate");
        assert!(book.is_empty(), "taken address leaves the pool");
        // a duplicate gossip while reserved is skipped
        assert_eq!(book.insert_many(&[peer("1.1.1.1", 8444, 20)]), 0);
        book.reclaim(&taken, false);
        assert_eq!(book.len(), 1, "clean disconnect returns to pool");
    }

    // An address whose session ended almost immediately (a peer at capacity accepting and
    // closing, or rejecting our fetches) must sit out a cooldown instead of going straight
    // back into the random pick — on a small pool the hot re-dial loop burns every slot on
    // the same closers.
    #[test]
    fn a_cooled_address_is_not_the_next_candidate() {
        let mut book = AddressBook::new(&P2pSettings::default());
        book.insert_many(&[peer("1.1.1.1", 8444, 10), peer("2.2.2.2", 8444, 10)]);
        let churner = book.take().expect("a candidate");
        book.reclaim_after(&churner, std::time::Duration::from_secs(60));
        for _ in 0..8 {
            let picked = book.take().expect("the healthy address is available");
            assert_ne!(
                (picked.host.as_str(), picked.port),
                (churner.host.as_str(), churner.port),
                "a cooling address must not be re-dialed"
            );
            book.reclaim(&picked, false);
        }
    }

    // Cooldowns must never wedge the dialer: when every pooled address is cooling, take()
    // still serves one (availability over politeness on a starved pool).
    #[test]
    fn an_all_cooling_pool_still_serves_a_candidate() {
        let mut book = AddressBook::new(&P2pSettings::default());
        book.insert_many(&[peer("1.1.1.1", 8444, 10)]);
        let only = book.take().expect("a candidate");
        book.reclaim_after(&only, std::time::Duration::from_secs(60));
        assert!(
            book.take().is_some(),
            "a starved pool serves a cooling address rather than nothing"
        );
    }

    #[test]
    fn violation_reclaim_forgets_the_peer() {
        let mut book = AddressBook::new(&P2pSettings::default());
        book.insert_many(&[peer("1.1.1.1", 8444, 10)]);
        let taken = book.take().expect("a candidate");
        book.reclaim(&taken, true);
        assert!(book.is_empty(), "a violating peer is not returned");
    }

    #[test]
    fn self_authority_is_never_stored() {
        let mut book = AddressBook::new(&P2pSettings::default());
        book.add_self("9.9.9.9", 8444);
        let accepted = book.insert_many(&[peer("9.9.9.9", 8444, 10), peer("1.1.1.1", 8444, 10)]);
        assert_eq!(accepted, 1, "own advertised authority is skipped on intake");
    }

    #[test]
    fn age_drops_stale_entries() {
        let mut book = AddressBook::new(&P2pSettings::default());
        book.insert_many(&[peer("1.1.1.1", 8444, 100), peer("2.2.2.2", 8444, 9000)]);
        book.age(10_000, 6000);
        assert_eq!(book.len(), 1, "entry older than threshold aged out");
    }

    #[test]
    fn persist_round_trips_and_dedups_on_reload() {
        let mut a = AddressBook::new(&P2pSettings::default());
        a.insert_many(&[peer("1.1.1.1", 8444, 10), peer("2.2.2.2", 8444, 20)]);
        let blob = a.serialize();
        let mut b = AddressBook::new(&P2pSettings::default());
        assert_eq!(b.load_str(&blob), 2, "restart reloads the persisted pool");
        assert_eq!(b.load_str(&blob), 0, "reload is deduped on intake");
        assert_eq!(b.len(), 2);
    }

    #[test]
    fn fetch_returns_a_bounded_random_subset() {
        let settings = P2pSettings::default();
        let mut book = AddressBook::new(&settings);
        let many: Vec<_> = (0..50u16).map(|i| peer("1.1.1.1", 8000 + i, 1)).collect();
        book.insert_many(&many);
        let sample = book.fetch();
        assert!(sample.len() >= settings.address_lower && sample.len() <= settings.address_upper);
    }
}
