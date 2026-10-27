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

pub(super) fn validate_block_range(
    blocks: &[FullBlock],
    start: u32,
    end: u32,
) -> Result<(), SyncError> {
    let expected = u64::from(end).checked_sub(u64::from(start)).map(|n| n + 1);
    if expected != Some(blocks.len() as u64)
        || !blocks
            .iter()
            .zip(start..=end)
            .all(|(block, height)| block.height() == height)
    {
        return Err(SyncError::RangeMismatch {
            start,
            end,
            got: blocks.len(),
        });
    }
    Ok(())
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
        let request = async {
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
            decode_block_range_reply(&reply, version, start, end)
        };
        match tokio::time::timeout(Duration::from_millis(self.timeout_ms), request).await {
            Ok(result) => result,
            Err(_) => Err(SyncError::PeerStalled(self.peer_id())),
        }
    }
}

fn decode_block_range_reply(
    reply: &ChiaMessage,
    version: ChiaProtocolVersion,
    start: u32,
    end: u32,
) -> Result<Vec<FullBlock>, SyncError> {
    let mut cursor = Cursor::new(reply.data.bytes.as_slice());
    match reply.msg_type {
        ProtocolMessageTypes::RespondBlocks => {
            let resp = RespondBlocks::from_bytes(&mut cursor, version).map_err(|e| {
                SyncError::Io(std::io::Error::other(format!("decode RespondBlocks: {e}")))
            })?;
            // Verify the peer echoed the requested range — exactly `end - start + 1` blocks,
            // ascending and contiguous — before handing them on. A mismatch is treated as a reject.
            if resp.start_height != start || resp.end_height != end {
                return Err(SyncError::RangeMismatch {
                    start,
                    end,
                    got: resp.blocks.len(),
                });
            }
            validate_block_range(&resp.blocks, start, end)?;
            Ok(resp.blocks)
        }
        // The peer cannot serve this range: a distinct, non-fatal reject so the worker
        // reclaims for another peer.
        ProtocolMessageTypes::RejectBlocks => {
            let rejection = RejectBlocks::from_bytes(&mut cursor, version)?;
            if rejection.start_height != start || rejection.end_height != end {
                return Err(SyncError::Io(std::io::Error::other(
                    "RejectBlocks echoed a different range",
                )));
            }
            Err(SyncError::RangeRejected { start, end })
        }
        _ => Err(SyncError::Io(std::io::Error::other(format!(
            "unexpected block-range response: {:?}",
            reply.msg_type
        )))),
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

#[cfg(test)]
mod range_reply_tests {
    use super::*;

    #[test]
    fn reject_replies_must_decode_and_echo_the_requested_range() {
        let version = ChiaProtocolVersion::default();
        let reply = ChiaMessage::new(
            ProtocolMessageTypes::RejectBlocks,
            version,
            &RejectBlocks {
                start_height: 100,
                end_height: 131,
            },
            None,
        )
        .unwrap();
        assert!(matches!(
            decode_block_range_reply(&reply, version, 100, 131),
            Err(SyncError::RangeRejected {
                start: 100,
                end: 131
            })
        ));
        assert!(matches!(
            decode_block_range_reply(&reply, version, 101, 132),
            Err(SyncError::Io(_))
        ));
        let mut malformed = reply.clone();
        malformed.data.bytes.clear();
        assert!(matches!(
            decode_block_range_reply(&malformed, version, 100, 131),
            Err(SyncError::Io(_))
        ));
        let mut unexpected = reply;
        unexpected.msg_type = ProtocolMessageTypes::RespondBlock;
        assert!(matches!(
            decode_block_range_reply(&unexpected, version, 100, 131),
            Err(SyncError::Io(_))
        ));
    }

    #[test]
    fn responses_must_echo_the_range_and_contain_every_requested_height() {
        let version = ChiaProtocolVersion::default();
        let block: FullBlock =
            serde_json::from_str(include_str!("../../tests/fixtures/full_block_5000000.json"))
                .unwrap();
        let height = block.height();
        for (start, end, blocks, valid) in [
            (height, height, vec![block.clone()], true),
            (height - 1, height, vec![block.clone()], false),
            (height, height, vec![], false),
            (
                height,
                height + 1,
                vec![block.clone(), block.clone()],
                false,
            ),
            (height + 1, height + 1, vec![block.clone()], false),
        ] {
            let reply = ChiaMessage::new(
                ProtocolMessageTypes::RespondBlocks,
                version,
                &RespondBlocks {
                    start_height: start,
                    end_height: end,
                    blocks,
                },
                None,
            )
            .unwrap();
            assert_eq!(
                decode_block_range_reply(&reply, version, height, height).is_ok(),
                valid
            );
        }
    }
}
