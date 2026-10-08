use super::*;
use std::time::Instant;

const SYNC_LOG_INTERVAL: Duration = Duration::from_secs(30);

struct SyncProgress {
    last_report: Instant,
    last_height: Option<u32>,
}

impl SyncProgress {
    fn sample(&mut self, now: Instant, height: Option<u32>) -> Option<f64> {
        let elapsed = now.duration_since(self.last_report);
        if elapsed < SYNC_LOG_INTERVAL {
            return None;
        }
        // An initial anchor jump and a reorg are not validated-block throughput.
        let advanced = match (self.last_height, height) {
            (Some(previous), Some(current)) => current.saturating_sub(previous),
            _ => 0,
        };
        self.last_report = now;
        self.last_height = height;
        Some(f64::from(advanced) / elapsed.as_secs_f64())
    }
}

// Independent of the sync driver so downloads, proof validation, and recovery cannot
// suppress progress reports. The driver's JoinSet owns and cancels this task on shutdown.
pub(super) async fn report_sync_progress<S: BlockStore + CoinStore + Send + Sync + 'static>(
    node: Arc<FullNode<S>>,
    registry: Arc<dyn OutboundPeers>,
) {
    let height = node.store.get_peak().await.ok().flatten().map(|(_, h)| h);
    let mut progress = SyncProgress {
        last_report: Instant::now(),
        last_height: height,
    };
    while node.run.load(Ordering::Relaxed) {
        tokio::time::sleep(SYNC_LOG_INTERVAL).await;
        if !node.run.load(Ordering::Relaxed) {
            break;
        }
        let Ok(peak) = node.store.get_peak().await else {
            continue;
        };
        let height = peak.map(|(_, h)| h);
        let Some(rate) = progress.sample(Instant::now(), height) else {
            continue;
        };
        if node.synced.load(Ordering::Relaxed) {
            continue;
        }
        let local = height.unwrap_or(0);
        let target = node.claimed_peak.load(Ordering::Relaxed);
        let peers = registry.live_peers().await.len();
        info!(
            "sync progress height={} target={} remaining={} blocks_per_sec={:.1} outbound_peers={}",
            local,
            target,
            target.saturating_sub(local),
            rate,
            peers
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_is_time_based_and_uses_actual_elapsed_time() {
        let start = Instant::now();
        let mut progress = SyncProgress {
            last_report: start,
            last_height: Some(100),
        };
        for seconds in 1..30 {
            assert_eq!(
                progress.sample(start + Duration::from_secs(seconds), Some(100_000)),
                None
            );
        }
        assert_eq!(
            progress.sample(start + SYNC_LOG_INTERVAL, Some(160)),
            Some(2.0)
        );
        assert_eq!(
            progress.sample(start + Duration::from_secs(90), Some(220)),
            Some(1.0)
        );
        assert_eq!(
            progress.sample(start + Duration::from_secs(120), Some(220)),
            Some(0.0)
        );
    }

    #[test]
    fn initial_peak_and_reorg_do_not_inflate_throughput() {
        let start = Instant::now();
        let mut progress = SyncProgress {
            last_report: start,
            last_height: None,
        };
        for (seconds, height, rate) in [(30, 100_000, 0.0), (60, 99_970, 0.0), (90, 100_000, 1.0)] {
            assert_eq!(
                progress.sample(start + Duration::from_secs(seconds), Some(height)),
                Some(rate)
            );
        }
    }
}
