use serde::{Deserialize, Serialize};

/// Chia seeder's exponentially weighted reachability windows.
const WINDOWS: [u64; 5] = [7200, 28800, 86400, 604800, 2592000];

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct PeerStat {
    pub weight: f64,
    pub count: f64,
    pub reliability: f64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Reliability {
    pub stats: [PeerStat; 5],
    pub tries: u64,
    pub successes: u64,
    pub updated_at: Option<u64>,
    pub ignore_until: u64,
    pub ban_until: u64,
}

impl Reliability {
    pub fn is_reliable(&self) -> bool {
        (self.tries > 0 && self.tries <= 3 && self.successes * 2 >= self.tries)
            || self
                .stats
                .iter()
                .zip([0.85, 0.7, 0.55, 0.45, 0.35])
                .zip([2., 4., 8., 16., 32.])
                .any(|((s, threshold), count)| s.reliability > threshold && s.count > count)
    }

    pub fn update(&mut self, reachable: bool, now: u64) {
        let age = self.updated_at.map_or(1000, |at| now.saturating_sub(at));
        for (stat, window) in self.stats.iter_mut().zip(WINDOWS) {
            let decay = (-(age as f64) / window as f64).exp();
            stat.reliability = stat.reliability * decay + if reachable { 1. - decay } else { 0. };
            stat.count = stat.count * decay + 1.;
            stat.weight = stat.weight * decay + 1. - decay;
        }
        self.tries += 1;
        self.successes += u64::from(reachable);
        self.updated_at = Some(now);
        if self.is_reliable() {
            self.ignore_until = 0;
            self.ban_until = 0;
            return;
        }
        // Upper bounds account for the unobserved portion of each window.
        for (index, threshold, count, delay) in [
            (4, 0.15, 32., 30 * 86400),
            (3, 0.10, 16., 7 * 86400),
            (2, 0.05, 8., 86400),
        ] {
            let stat = self.stats[index];
            if stat.reliability - stat.weight + 1. < threshold && stat.count > count {
                self.ban_until = self.ban_until.max(now + delay);
                break;
            }
        }
        for (index, threshold, delay) in [
            (4, 0.20, 10 * 86400),
            (3, 0.16, 3 * 86400),
            (2, 0.12, 8 * 3600),
            (1, 0.08, 2 * 3600),
        ] {
            let stat = self.stats[index];
            if stat.reliability - stat.weight + 1. < threshold && stat.count > 2. {
                self.ignore_until = self.ignore_until.max(now + delay);
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success_failure_decay_and_recovery() {
        let mut r = Reliability::default();
        r.update(true, 1000);
        assert!(r.is_reliable());
        assert!((r.stats[0].reliability - (1. - (-1000_f64 / 7200.).exp())).abs() < 1e-12);
        r.update(false, 2000);
        assert!(r.is_reliable());
        r.update(false, 3000);
        assert!(!r.is_reliable());
        for n in 4..=1000 {
            r.update(false, n * 1000);
        }
        assert!(r.ignore_until > 1_000_000);
        assert!(r.ban_until > 1_000_000);
        for n in 1001..=1100 {
            r.update(true, n * 1000);
        }
        assert!(r.is_reliable());
        assert_eq!(r.ignore_until, 0);
        assert_eq!(r.ban_until, 0);
    }
}
