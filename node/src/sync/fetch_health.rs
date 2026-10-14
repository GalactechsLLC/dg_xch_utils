use super::{BlockRangeSource, SyncError};
use dg_xch_core::blockchain::full_block::FullBlock;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::time::Instant;

#[derive(Default)]
pub(super) struct FetchHealth {
    peers: Mutex<HashMap<u64, Failure>>,
}

struct Failure {
    strikes: u32,
    last_failure: Instant,
    retry_at: Instant,
}

impl FetchHealth {
    pub(super) fn retain(&self, sources: &[Arc<dyn BlockRangeSource>]) {
        self.peers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|id, _| sources.iter().any(|source| source.peer_id() == *id));
    }

    pub(super) fn eligible(&self, peer: u64) -> bool {
        self.peers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&peer)
            .is_none_or(|failure| Instant::now() >= failure.retry_at)
    }

    // Direct requests and spawned prefetch requests share this state and timeout boundary.
    pub(super) async fn fetch(
        &self,
        source: &dyn BlockRangeSource,
        start: u32,
        end: u32,
        timeout: Duration,
    ) -> Result<Vec<FullBlock>, SyncError> {
        let started = Instant::now();
        let result = match tokio::time::timeout(timeout, source.fetch_range(start, end)).await {
            Ok(result) => result.and_then(|blocks| {
                super::source::validate_block_range(&blocks, start, end)?;
                Ok(blocks)
            }),
            Err(_) => Err(SyncError::PeerStalled(source.peer_id())),
        };
        self.record(source, end, started, &result);
        result
    }

    fn record(
        &self,
        source: &dyn BlockRangeSource,
        end: u32,
        started: Instant,
        result: &Result<Vec<FullBlock>, SyncError>,
    ) {
        let mut peers = self.peers.lock().unwrap_or_else(|e| e.into_inner());
        let id = source.peer_id();
        if result.is_ok() {
            // A successful request already in flight when another request failed must not
            // erase that newer failure (e.g. a peer serves recent blocks but lacks old ones).
            if peers
                .get(&id)
                .is_none_or(|failure| started > failure.last_failure)
            {
                peers.remove(&id);
            }
            return;
        }
        let now = Instant::now();
        let strikes = peers.get(&id).map_or(1, |f| f.strikes.saturating_add(1));
        let near_tip = source
            .advertised_height()
            .is_some_and(|height| height.saturating_sub(end) <= 32);
        let (base_ms, ceiling_ms) = match result {
            Err(SyncError::RangeRejected { .. }) if near_tip => (250, 2_000),
            Err(SyncError::RangeRejected { .. } | SyncError::RangeMismatch { .. }) => {
                (5_000, 300_000)
            }
            _ => (1_000, 30_000),
        };
        let delay =
            Duration::from_millis((base_ms << strikes.saturating_sub(1).min(9)).min(ceiling_ms));
        peers.insert(
            id,
            Failure {
                strikes,
                last_failure: now,
                retry_at: now + delay,
            },
        );
    }
}
