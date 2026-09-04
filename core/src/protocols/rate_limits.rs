// Per-connection, bidirectional rate limiting.
//
// One `RateLimiter` instance lives per connection (separate inbound and outbound limiters).
// Every message is checked against three
// budgets per 60-second window, selected per connection from the capabilities both peers negotiated
// at handshake:
//   1. a per-message-type frequency (max messages / window),
//   2. a per-message max_size (a single oversized message is a violation on its own), and
//   3. a per-message-type cumulative-size budget,
// plus a cross-type NON-TX AGGREGATE window (1000 msgs / 60s and 100 MiB shared by every
// `aggregate_limit=true` type) so a flood spread across many distinct types is still bounded.
//
// v1 vs v2 selection: when BOTH peers advertise `RATE_LIMITS_V2`
// the v2 overlay is applied on top of v1 — v2 raises the
// frequency of the light-wallet/sync query types and moves several of them OUT of the aggregate
// window (`aggregate_limit` flips true→false). Otherwise the v1 numbers apply. We always advertise
// v2 (`shared::CAPABILITIES`), so "both v2" reduces to "the peer advertised v2".

use crate::blockchain::sized_bytes::Bytes32;
use crate::protocols::ProtocolMessageTypes;
use crate::protocols::shared::{Capabilities, Capability};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

const MIB: u64 = 1024 * 1024;
const KIB: u64 = 1024;

/// A time-based rate limit for one message type. Counts and sizes are per
/// `reset_seconds` (60s) window.
#[derive(Clone, Copy, Debug)]
pub struct RlSettings {
    /// When true, this type ALSO counts against the per-connection non-tx aggregate window
    /// (the cross-type budget). False for "tx" types and for the aggregate limiter itself.
    pub aggregate_limit: bool,
    /// Max messages of this type per window.
    pub frequency: u32,
    /// Max size of a SINGLE message of this type (bytes). Exceeded → violation immediately.
    pub max_size: u64,
    /// Max cumulative size of all messages of this type per window. `None` → `frequency * max_size`.
    pub max_total_size: Option<u64>,
}

/// A message type that is exempt from the rate limit but still bounded by a per-message size cap.
/// Used for response messages implicitly bounded by their request; these are
/// also outside the aggregate window (like "tx" types).
#[derive(Clone, Copy, Debug)]
pub struct Unlimited {
    pub max_size: u64,
}

/// The limit selected for a message type: a full time-based limit or a size-only cap.
#[derive(Clone, Copy, Debug)]
pub enum Limit {
    Rl(RlSettings),
    Unlimited(Unlimited),
}

const fn rl(aggregate_limit: bool, frequency: u32, max_size: u64) -> Limit {
    Limit::Rl(RlSettings {
        aggregate_limit,
        frequency,
        max_size,
        max_total_size: None,
    })
}

const fn rl_total(
    aggregate_limit: bool,
    frequency: u32,
    max_size: u64,
    max_total_size: u64,
) -> Limit {
    Limit::Rl(RlSettings {
        aggregate_limit,
        frequency,
        max_size,
        max_total_size: Some(max_total_size),
    })
}

const fn unlimited(max_size: u64) -> Limit {
    Limit::Unlimited(Unlimited { max_size })
}

/// The cross-type non-tx aggregate window:
/// 1000 messages / 60s and 100 MiB shared by every `aggregate_limit=true` message type.
const AGGREGATE: RlSettings = RlSettings {
    aggregate_limit: false,
    frequency: 1000,
    max_size: 0,
    max_total_size: Some(100 * MIB),
};

/// True when the peer advertises `RATE_LIMITS_V2` with state "1".
/// We always advertise v2 ourselves (`shared::CAPABILITIES`), so this is the whole "both peers v2"
/// test that selects the v2 numbers.
#[must_use]
pub fn peer_supports_v2(peer_caps: &Capabilities) -> bool {
    let v2 = Capability::RateLimitsV2 as u16;
    peer_caps.iter().any(|(v, state)| *v == v2 && state == "1")
}

/// The v2 overlay. Applied on top of v1 when
/// both peers advertise v2. `None` means the type keeps its v1 limit.
fn v2_override(t: ProtocolMessageTypes) -> Option<Limit> {
    use ProtocolMessageTypes as P;
    Some(match t {
        P::RequestBlockHeader => rl(false, 500, 100),
        P::RespondBlockHeader => rl(false, 500, 500 * KIB),
        P::RejectHeaderRequest => rl(false, 500, 100),
        P::RequestRemovals => rl_total(false, 5000, 50 * KIB, 10 * MIB),
        P::RespondRemovals => rl_total(false, 5000, MIB, 10 * MIB),
        P::RejectRemovalsRequest => rl(false, 500, 100),
        P::RequestAdditions => rl(false, 50000, 100 * MIB),
        P::RespondAdditions => rl(false, 50000, 100 * MIB),
        P::RejectAdditionsRequest => rl(false, 500, 100),
        P::RejectHeaderBlocks => rl(false, 1000, 100),
        P::RespondHeaderBlocks => rl(false, 5000, 2 * MIB),
        P::RequestSesHashes => rl(false, 2000, MIB),
        P::RespondSesHashes => rl(false, 2000, MIB),
        P::RequestChildren => rl(false, 2000, MIB),
        P::RespondChildren => rl(false, 2000, MIB),
        P::RequestPuzzleSolution => rl(false, 5000, 100),
        P::RespondPuzzleSolution => rl(false, 5000, MIB),
        P::RejectPuzzleSolution => rl(false, 5000, 100),
        // Lower cap since it doesn't scale with high TPS (NON_TX_FREQ); stays aggregate-limited.
        P::RequestHeaderBlocks => rl(true, 5000, 100),
        _ => return None,
    })
}

/// The v1 table. Types our enum does not model (configure_window_sizes, error, solve,
/// solution_response, partial_proofs) fall to a conservative aggregate-limited default so an
/// unknown/unhandled type can never be used to bypass the limits.
#[allow(clippy::too_many_lines)]
fn v1_limit(t: ProtocolMessageTypes) -> Limit {
    use ProtocolMessageTypes as P;
    match t {
        // ---- tx category (aggregate_limit=false): scales with TPS, outside the aggregate window ----
        P::NewTransaction | P::RequestTransaction => rl_total(false, 5000, 100, 5000 * 100),
        P::RespondTransaction => rl_total(false, 5000, MIB, 20 * MIB),
        P::SendTransaction => rl(false, 5000, MIB),
        P::TransactionAck => rl(false, 5000, 2048),
        P::NoneResponse => rl(false, 500, 100),
        P::RequestBlockHeaders => rl(false, 5000, 100),
        P::RejectBlockHeaders => rl(false, 1000, 100),
        P::RespondBlockHeaders => rl(false, 5000, 2 * MIB),
        // ---- non-tx category (aggregate_limit=true) ----
        P::Handshake => rl_total(true, 5, 10 * KIB, 5 * 10 * KIB),
        P::HarvesterHandshake => rl(true, 5, MIB),
        P::NewSignagePointHarvester => rl(true, 100, 4886),
        P::NewProofOfSpace => rl(true, 100, 2048),
        P::RequestSignatures => rl(true, 100, 2048),
        P::RespondSignatures => rl(true, 100, 2048),
        P::NewSignagePoint => rl(true, 200, 2048),
        P::DeclareProofOfSpace => rl(true, 100, 10 * KIB),
        P::RequestSignedValues => rl(true, 100, 10 * KIB),
        P::FarmingInfo => rl(true, 100, 1024),
        P::SignedValues => rl(true, 100, 1024),
        P::NewPeakTimelord => rl(true, 100, 20 * KIB),
        P::NewUnfinishedBlockTimelord => rl(true, 100, 10 * KIB),
        P::NewSignagePointVdf => rl(true, 100, 100 * KIB),
        P::NewInfusionPointVdf => rl(true, 100, 100 * KIB),
        P::NewEndOfSubSlotVdf => rl(true, 100, 100 * KIB),
        P::RequestCompactProofOfTime => rl(true, 100, 10 * KIB),
        P::RespondCompactProofOfTime => rl(true, 100, 100 * KIB),
        P::NewPeak => rl(true, 200, 512),
        P::RequestProofOfWeight => rl(true, 5, 100),
        P::RespondProofOfWeight => rl_total(true, 5, 50 * MIB, 100 * MIB),
        P::RequestBlock => rl(true, 200, 100),
        P::RejectBlock => unlimited(100),
        P::RequestBlocks => rl(true, 500, 100),
        P::RespondBlocks => unlimited(50 * MIB),
        P::RejectBlocks => unlimited(100),
        P::RespondBlock => unlimited(2 * MIB),
        P::NewUnfinishedBlock => rl(true, 200, 100),
        P::RequestUnfinishedBlock => rl(true, 200, 100),
        P::NewUnfinishedBlock2 => rl(true, 200, 100),
        P::RequestUnfinishedBlock2 => rl(true, 200, 100),
        P::RespondUnfinishedBlock => rl_total(true, 200, 2 * MIB, 10 * 2 * MIB),
        P::NewSignagePointOrEndOfSubSlot => rl(true, 200, 200),
        P::RequestSignagePointOrEndOfSubSlot => rl(true, 200, 200),
        P::RespondSignagePoint => rl(true, 200, 50 * KIB),
        P::RespondEndOfSubSlot => rl(true, 100, 50 * KIB),
        P::RequestMempoolTransactions => rl(true, 5, MIB),
        P::RequestCompactVdf => rl(true, 200, 1024),
        P::RespondCompactVdf => rl(true, 200, 100 * KIB),
        P::NewCompactVdf => rl(true, 100, 1024),
        P::RequestPeers => rl(true, 10, 100),
        P::RespondPeers => rl(true, 10, MIB),
        P::RequestPuzzleSolution => rl(true, 1000, 100),
        P::RespondPuzzleSolution => rl(true, 1000, MIB),
        P::RejectPuzzleSolution => rl(true, 1000, 100),
        P::NewPeakWallet => rl(true, 200, 300),
        P::RequestBlockHeader => rl(true, 500, 100),
        P::RespondBlockHeader => rl(true, 500, 500 * KIB),
        P::RejectHeaderRequest => rl(true, 500, 100),
        P::RequestRemovals => rl_total(true, 500, 50 * KIB, 10 * MIB),
        P::RespondRemovals => rl_total(true, 500, MIB, 10 * MIB),
        P::RejectRemovalsRequest => rl(true, 500, 100),
        P::RequestAdditions => rl_total(true, 500, MIB, 10 * MIB),
        P::RespondAdditions => rl_total(true, 500, MIB, 10 * MIB),
        P::RejectAdditionsRequest => rl(true, 500, 100),
        P::RequestHeaderBlocks => rl(true, 500, 100),
        P::RejectHeaderBlocks => rl(true, 100, 100),
        P::RespondHeaderBlocks => rl_total(true, 500, 2 * MIB, 100 * MIB),
        P::RequestPeersIntroducer => rl(true, 100, 100),
        P::RespondPeersIntroducer => rl(true, 100, MIB),
        P::FarmNewBlock => rl(true, 200, 200),
        P::RequestPlots => rl(true, 10, 10 * MIB),
        P::RespondPlots => rl(true, 10, 100 * MIB),
        P::PlotSyncStart
        | P::PlotSyncLoaded
        | P::PlotSyncRemoved
        | P::PlotSyncInvalid
        | P::PlotSyncKeysMissing
        | P::PlotSyncDuplicates
        | P::PlotSyncDone => rl(true, 1000, 100 * MIB),
        P::PlotSyncResponse => rl(true, 3000, 100 * MIB),
        P::CoinStateUpdate => rl(true, 1000, 100 * MIB),
        P::RegisterInterestInPuzzleHash => rl(true, 1000, 100 * MIB),
        P::RespondToPhUpdate => rl(true, 1000, 100 * MIB),
        P::RegisterInterestInCoin => rl(true, 1000, 100 * MIB),
        P::RespondToCoinUpdate => rl(true, 1000, 100 * MIB),
        P::RequestRemovePuzzleSubscriptions => rl(true, 1000, 100 * MIB),
        P::RespondRemovePuzzleSubscriptions => rl(true, 1000, 100 * MIB),
        P::RequestRemoveCoinSubscriptions => rl(true, 1000, 100 * MIB),
        P::RespondRemoveCoinSubscriptions => rl(true, 1000, 100 * MIB),
        P::RequestPuzzleState => rl(true, 1000, 100 * MIB),
        P::RespondPuzzleState => rl(true, 1000, 100 * MIB),
        P::RejectPuzzleState => rl(true, 200, 100),
        P::RequestCoinState => rl(true, 1000, 100 * MIB),
        P::RespondCoinState => rl(true, 1000, 100 * MIB),
        P::RejectCoinState => rl(true, 200, 100),
        P::MempoolItemsAdded => rl(true, 1000, 100 * MIB),
        P::MempoolItemsRemoved => rl(true, 1000, 100 * MIB),
        P::RequestCostInfo => rl(true, 1000, 100),
        P::RespondCostInfo => rl(true, 1000, 1024),
        P::RequestSesHashes => rl(true, 2000, MIB),
        P::RespondSesHashes => rl(true, 2000, MIB),
        P::RequestChildren => rl(true, 2000, MIB),
        P::RespondChildren => rl(true, 2000, MIB),
        P::RequestFeeEstimates => rl(true, 10, 100),
        P::RespondFeeEstimates => rl(true, 10, 100),
        // The error / solver / partial-proof rows, plus the configure_window_sizes handshake
        // follow-up.
        P::Error => rl(false, 50000, 100),
        P::ConfigureWindowSizes => rl_total(true, 5, KIB, KIB),
        P::Solve => rl(false, 120, KIB),
        P::SolutionResponse => rl(false, 120, KIB),
        P::PartialProofs => rl(false, 120, 3 * KIB),
        // The Unknown sentinel (an undefined message code): a conservative aggregate-limited
        // default, so an unhandled code can never be a limit bypass. The read loop disconnects
        // on it before the limiter ever matters.
        P::Unknown => rl(true, 100, 1024),
    }
}

/// Select the composed limit for a message type given whether the peer negotiated v2 — the
/// v2 overlay evaluated per type.
#[must_use]
pub fn composed_limit(t: ProtocolMessageTypes, both_v2: bool) -> Limit {
    if both_v2 && let Some(l) = v2_override(t) {
        return l;
    }
    v1_limit(t)
}

#[derive(Default)]
struct Window {
    slot: u64,
    counts: HashMap<u8, u32>,
    sizes: HashMap<u8, u64>,
    non_tx_count: u32,
    non_tx_size: u64,
}

/// A per-connection rate limiter. `incoming` limiters commit their counters
/// unconditionally (the bytes were already received); outgoing limiters commit only when the message
/// is allowed to be sent.
pub struct RateLimiter {
    incoming: bool,
    reset_seconds: u64,
    percentage_of_limit: u32,
    start: Instant,
    inner: Mutex<Window>,
}

impl RateLimiter {
    /// A limiter with the defaults: a 60-second window at 100% of the published numbers.
    #[must_use]
    pub fn new(incoming: bool) -> Self {
        Self::with_params(incoming, 60, 100)
    }

    #[must_use]
    pub fn with_params(incoming: bool, reset_seconds: u64, percentage_of_limit: u32) -> Self {
        Self {
            incoming,
            reset_seconds: reset_seconds.max(1),
            percentage_of_limit,
            start: Instant::now(),
            inner: Mutex::new(Window::default()),
        }
    }

    /// Check one message against the composed limits for this connection. Returns `Some(reason)` when
    /// a limit is exceeded (the caller closes the connection);
    /// `None` when the message is within budget. The aggregate check runs before the per-type
    /// check, and incoming counters commit unconditionally.
    pub fn process_and_check(
        &self,
        msg_type: ProtocolMessageTypes,
        size: usize,
        peer_caps: &Capabilities,
    ) -> Option<String> {
        let both_v2 = peer_supports_v2(peer_caps);
        let limit = composed_limit(msg_type, both_v2);
        let proportion = f64::from(self.percentage_of_limit) / 100.0;
        let size = size as u64;

        let mut w = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let slot = self.start.elapsed().as_secs() / self.reset_seconds;
        if slot != w.slot {
            w.slot = slot;
            w.counts.clear();
            w.sizes.clear();
            w.non_tx_count = 0;
            w.non_tx_size = 0;
        }

        let key = msg_type as u8;
        let new_count = w.counts.get(&key).copied().unwrap_or(0) + 1;
        let new_size = w.sizes.get(&key).copied().unwrap_or(0) + size;
        let mut new_non_tx_count = w.non_tx_count;
        let mut new_non_tx_size = w.non_tx_size;
        let mut allowed = false;

        let violation = match limit {
            Limit::Rl(s) => {
                let mut hit: Option<String> = None;
                if s.aggregate_limit {
                    new_non_tx_count = w.non_tx_count + 1;
                    new_non_tx_size = w.non_tx_size + size;
                    let freq_cap = f64::from(AGGREGATE.frequency) * proportion;
                    let size_cap = AGGREGATE.max_total_size.unwrap_or(0) as f64 * proportion;
                    if f64::from(new_non_tx_count) > freq_cap {
                        hit = Some(format!("non-tx count: {new_non_tx_count} > {freq_cap}"));
                    } else if new_non_tx_size as f64 > size_cap {
                        hit = Some(format!("non-tx size: {new_non_tx_size} > {size_cap}"));
                    }
                }
                if hit.is_none() {
                    let max_total = s
                        .max_total_size
                        .unwrap_or(u64::from(s.frequency) * s.max_size);
                    let count_cap = f64::from(s.frequency) * proportion;
                    let total_cap = max_total as f64 * proportion;
                    if f64::from(new_count) > count_cap {
                        hit = Some(format!("message count: {new_count} > {count_cap}"));
                    } else if size > s.max_size {
                        hit = Some(format!("message size: {size} > {}", s.max_size));
                    } else if new_size as f64 > total_cap {
                        hit = Some(format!("cumulative size: {new_size} > {total_cap}"));
                    } else {
                        allowed = true;
                    }
                }
                hit
            }
            Limit::Unlimited(u) => {
                if size > u.max_size {
                    Some(format!("message size: {size} > {}", u.max_size))
                } else {
                    allowed = true;
                    None
                }
            }
        };

        // For INCOMING messages the counters advance unconditionally (the bytes are already
        // received), so a peer that keeps violating keeps climbing the window. For OUTGOING, only
        // advance when we actually send (allowed).
        if self.incoming || allowed {
            w.counts.insert(key, new_count);
            w.sizes.insert(key, new_size);
            w.non_tx_count = new_non_tx_count;
            w.non_tx_size = new_non_tx_size;
        }
        violation
    }
}

/// A convenience newtype for holding a per-peer inbound limiter behind the read loop, so a connection
/// with no negotiated identity yet still has a limiter to charge the handshake against.
pub type PeerId = Bytes32;

#[cfg(test)]
#[path = "../../tests/unit/protocols/rate_limits/tests.rs"]
mod tests;
