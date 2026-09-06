use std::time::Duration;

// Slot targets and caps reuse the full-node config defaults.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct P2pSettings {
    pub target_outbound: usize,
    pub target_peer_count: usize,
    pub host_pool_capacity: usize,
    pub address_lower: usize,
    pub address_upper: usize,
    pub connect_timeout: Duration,
    pub handshake_timeout: Duration,
    pub retry_timeout: Duration,
    pub heartbeat: Duration,
    pub pong_deadline: Duration,
    pub recent_peer_threshold: Duration,
    pub jitter_floor: f64,
}

impl Default for P2pSettings {
    fn default() -> Self {
        Self {
            target_outbound: 8,
            target_peer_count: 80,
            host_pool_capacity: 1000,
            address_lower: 5,
            address_upper: 10,
            connect_timeout: Duration::from_secs(30),
            handshake_timeout: Duration::from_secs(15),
            retry_timeout: Duration::from_secs(1),
            heartbeat: Duration::from_secs(120),
            pong_deadline: Duration::from_secs(30),
            recent_peer_threshold: Duration::from_secs(6000),
            jitter_floor: 0.5,
        }
    }
}

impl P2pSettings {
    /// Validate limits that would otherwise fail only after the peer supervisor starts.
    ///
    /// # Errors
    /// Returns an error when a capacity is zero, a peer target is inconsistent, a timeout is
    /// zero, or the jitter floor is outside the inclusive 0 to 1 range.
    pub fn validate(&self) -> Result<(), String> {
        if self.target_peer_count == 0 {
            return Err("target_peer_count must be greater than zero".to_string());
        }
        if self.target_outbound > self.target_peer_count {
            return Err("target_outbound cannot exceed target_peer_count".to_string());
        }
        if self.host_pool_capacity == 0 {
            return Err("host_pool_capacity must be greater than zero".to_string());
        }
        if self.address_lower > self.address_upper {
            return Err("address_lower cannot exceed address_upper".to_string());
        }
        if self.address_upper > self.host_pool_capacity {
            return Err("address_upper cannot exceed host_pool_capacity".to_string());
        }
        for (name, value) in [
            ("connect_timeout", self.connect_timeout),
            ("handshake_timeout", self.handshake_timeout),
            ("retry_timeout", self.retry_timeout),
            ("heartbeat", self.heartbeat),
            ("pong_deadline", self.pong_deadline),
            ("recent_peer_threshold", self.recent_peer_threshold),
        ] {
            if value.is_zero() {
                return Err(format!("{name} must be greater than zero"));
            }
        }
        if !self.jitter_floor.is_finite() || !(0.0..=1.0).contains(&self.jitter_floor) {
            return Err("jitter_floor must be between 0 and 1".to_string());
        }
        Ok(())
    }

    // Full-jitter backoff: retry_timeout scaled to [jitter_floor, 1.0].
    #[must_use]
    pub fn jittered_backoff(&self, attempt: u32) -> Duration {
        let capped = attempt.min(6);
        let base = self.retry_timeout.saturating_mul(1u32 << capped);
        let frac = self.jitter_floor + (1.0 - self.jitter_floor) * rand::random::<f64>();
        base.mul_f64(frac)
    }
}

#[cfg(test)]
#[path = "../tests/unit/config/tests.rs"]
mod tests;
