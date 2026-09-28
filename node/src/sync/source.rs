use crate::sync::SyncError;
use async_trait::async_trait;
use dg_xch_clients::websocket::{oneshot, oneshot_message};
use dg_xch_core::blockchain::full_block::FullBlock;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::blockchain::weight_proof::WeightProof;
use dg_xch_core::protocols::ChiaMessage;
use dg_xch_core::protocols::ProtocolMessageTypes;
use dg_xch_core::protocols::full_node::{
    RejectBlocks, RequestBlocks, RequestProofOfWeight, RespondBlocks, RespondProofOfWeight,
};
use dg_xch_p2p::OutboundPeer;
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use log::{info, warn};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// A peer that serves a contiguous block range. The production impl issues `RequestBlocks` over a
/// `dg_xch_p2p` outbound channel; tests back it in-memory.
#[async_trait]
pub trait BlockRangeSource: Send + Sync {
    /// Stable identity for reservation bookkeeping and slow-peer eviction.
    fn peer_id(&self) -> u64;

    /// The connection has closed; the supervisor should drop this source.
    fn is_closed(&self) -> bool;

    fn advertised_height(&self) -> Option<u32> {
        None
    }

    /// Fetch `start..=end` inclusive.
    ///
    /// # Errors
    /// Returns [`SyncError`] if the peer rejects the range, the response fails to decode, or it times out.
    async fn fetch_range(&self, start: u32, end: u32) -> Result<Vec<FullBlock>, SyncError>;
}

pub fn select_fetch_source(
    sources: &[Arc<dyn BlockRangeSource>],
    rotation: usize,
    end: u32,
    load: impl Fn(&dyn BlockRangeSource) -> Option<usize>,
) -> Option<usize> {
    let mut selected = None;
    let mut best_rank = None;
    for offset in 0..sources.len() {
        let index = (rotation.wrapping_add(offset)) % sources.len();
        let source = &sources[index];
        let height = source.advertised_height();
        if source.is_closed() || height.is_some_and(|height| height < end) {
            continue;
        }
        let Some(load) = load(source.as_ref()) else {
            continue;
        };
        let rank = (height.is_none(), load);
        if best_rank.is_none_or(|best| rank < best) {
            best_rank = Some(rank);
            selected = Some(index);
            if rank == (false, 0) {
                break;
            }
        }
    }
    selected
}

/// Fetches block ranges through an outbound peer's `WsClient`.
pub struct OutboundPeerSource {
    peer: Arc<OutboundPeer>,
    id: u64,
    version: ChiaProtocolVersion,
    timeout_ms: u64,
}

impl OutboundPeerSource {
    #[must_use]
    pub fn new(peer: Arc<OutboundPeer>, timeout: Duration) -> Self {
        let mut hasher = DefaultHasher::new();
        peer.endpoint.hash(&mut hasher);
        Self {
            id: hasher.finish(),
            peer,
            version: ChiaProtocolVersion::default(),
            timeout_ms: u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
        }
    }
}

#[async_trait]
impl BlockRangeSource for OutboundPeerSource {
    fn advertised_height(&self) -> Option<u32> {
        self.peer.client.peer_peak.height()
    }
    fn peer_id(&self) -> u64 {
        self.id
    }

    fn is_closed(&self) -> bool {
        self.peer.is_closed()
    }

    async fn fetch_range(&self, start: u32, end: u32) -> Result<Vec<FullBlock>, SyncError> {
        // `id: None` defers correlation-id minting to the connection. Wait for either RespondBlocks
        // or RejectBlocks (both echo the id); a reject resolves immediately instead of hanging the
        // worker until its request timeout.
        let msg = ChiaMessage::new(
            ProtocolMessageTypes::RequestBlocks,
            self.version,
            &RequestBlocks {
                start_height: start,
                end_height: end,
                include_transaction_block: true,
            },
            None,
        )?;
        let version = self.version;
        let reply = oneshot_message(
            self.peer.client.connection.clone(),
            msg,
            None,
            None,
            Some(self.timeout_ms),
        )
        .await?;
        let mut cursor = Cursor::new(reply.data.bytes.as_slice());
        match reply.msg_type {
            ProtocolMessageTypes::RespondBlocks => {
                let resp = RespondBlocks::from_bytes(&mut cursor, version).map_err(|e| {
                    SyncError::Io(std::io::Error::other(format!("decode RespondBlocks: {e}")))
                })?;
                // Verify the peer echoed the requested range — exactly `end - start + 1` blocks,
                // ascending and contiguous — before handing them on. A mismatch is treated as a reject.
                let expected_len = (end.saturating_sub(start).saturating_add(1)) as usize;
                let covers_range = resp.blocks.len() == expected_len
                    && resp
                        .blocks
                        .iter()
                        .zip(start..=end)
                        .all(|(b, want)| b.height() == want);
                if !covers_range {
                    return Err(SyncError::RangeMismatch {
                        start,
                        end,
                        got: resp.blocks.len(),
                    });
                }
                Ok(resp.blocks)
            }
            // The peer cannot serve this range: a distinct, non-fatal reject so the worker
            // reclaims for another peer.
            _ => {
                let _ = RejectBlocks::from_bytes(&mut cursor, version);
                Err(SyncError::RangeRejected { start, end })
            }
        }
    }
}

/// Wraps a [`BlockRangeSource`] and writes every fetched range to disk as a `RespondBlocks` blob
/// (`blocks_<start>_<end>.bin`) before returning it, for offline replay. Capture happens at fetch
/// time, so the range is saved even if downstream confirmation later fails.
pub struct CapturingSource {
    inner: Arc<dyn BlockRangeSource>,
    dir: PathBuf,
    version: ChiaProtocolVersion,
}

impl CapturingSource {
    #[must_use]
    pub fn new(inner: Arc<dyn BlockRangeSource>, dir: PathBuf) -> Self {
        Self {
            inner,
            dir,
            version: ChiaProtocolVersion::default(),
        }
    }
}

#[async_trait]
impl BlockRangeSource for CapturingSource {
    fn advertised_height(&self) -> Option<u32> {
        self.inner.advertised_height()
    }
    fn peer_id(&self) -> u64 {
        self.inner.peer_id()
    }

    fn is_closed(&self) -> bool {
        self.inner.is_closed()
    }

    async fn fetch_range(&self, start: u32, end: u32) -> Result<Vec<FullBlock>, SyncError> {
        let blocks = self.inner.fetch_range(start, end).await?;
        let resp = RespondBlocks {
            start_height: start,
            end_height: end,
            blocks: blocks.clone(),
        };
        match resp.to_bytes(self.version) {
            Ok(bytes) => {
                let path = self.dir.join(format!("blocks_{start}_{end}.bin"));
                if let Err(e) = std::fs::write(&path, &bytes) {
                    warn!("failed to write block-range capture error={}", e);
                } else {
                    info!(
                        "captured block range path={} blocks={} bytes={}",
                        path.display(),
                        blocks.len(),
                        bytes.len()
                    );
                }
            }
            Err(e) => warn!(
                "failed to serialize RespondBlocks for capture error={:?}",
                e
            ),
        }
        Ok(blocks)
    }
}

/// Request a weight proof to `tip` from an outbound peer — the fast-sync entry. `total_blocks` is
/// the peer's claimed tip height, used to size the proof.
///
/// # Errors
/// Returns [`SyncError`] if the peer rejects the request, the response fails to decode, or it times out.
pub async fn request_weight_proof(
    peer: &Arc<OutboundPeer>,
    tip: Bytes32,
    total_blocks: u32,
    timeout: Duration,
) -> Result<WeightProof, SyncError> {
    let version = ChiaProtocolVersion::default();
    let timeout_ms = u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX);
    let msg = ChiaMessage::new(
        ProtocolMessageTypes::RequestProofOfWeight,
        version,
        &RequestProofOfWeight {
            total_number_of_blocks: total_blocks,
            tip,
        },
        Some(1),
    )?;
    let resp: RespondProofOfWeight = oneshot(
        peer.client.connection.clone(),
        msg,
        Some(ProtocolMessageTypes::RespondProofOfWeight),
        version,
        Some(1),
        Some(timeout_ms),
    )
    .await?;
    Ok(resp.wp)
}
