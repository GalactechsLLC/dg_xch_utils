use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Default)]
pub struct PeerPeak {
    latest: Mutex<Option<(u32, Instant)>>,
}

impl PeerPeak {
    pub fn record(&self, height: u32) {
        *self
            .latest
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((height, Instant::now()));
    }

    #[must_use]
    pub fn height(&self) -> Option<u32> {
        self.height_at(Instant::now())
    }

    fn height_at(&self, now: Instant) -> Option<u32> {
        self.latest
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .filter(|(_, recorded)| {
                now.saturating_duration_since(*recorded) <= Duration::from_secs(300)
            })
            .map(|(height, _)| height)
    }
}

#[cfg(test)]
#[path = "../../tests/unit/protocols/peer_peak.rs"]
mod tests;
