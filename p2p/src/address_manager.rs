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

    // Random dial candidate; moved to the reserved (connected) set.
    pub fn take(&mut self) -> Option<TimestampedPeerInfo> {
        if self.pool.is_empty() {
            return None;
        }
        let idx = rand::random_range(0..self.pool.len());
        let picked = self.pool.remove(idx)?;
        let ep = endpoint(&picked);
        self.pooled.remove(&ep);
        self.reserved.insert(ep);
        Some(picked)
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
#[path = "../tests/unit/address_manager/tests.rs"]
mod tests;
