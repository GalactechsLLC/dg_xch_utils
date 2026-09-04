// Per-connection OUTBOUND self-throttle — the send-side companion to the inbound `RateLimiter`
// (`rate_limits.rs`).
//
// Before writing a message, the SAME rate-limit table is run through an outgoing limiter (the
// peer's negotiated capabilities select v1/v2). If sending now would exceed the PEER's budget
// the message is NOT dropped outright — this send is skipped and the message is retried ~1s
// later. The ONE exception is `respond_peers`: when it is over budget it is dropped WITHOUT
// re-queue, because its own cap is so low that re-queuing would spin. `Unlimited` response
// types (RespondBlocks, RespondBlock, RejectBlocks, …) carry no frequency budget, so our own
// solicited fetch and serve traffic is NEVER deferred — only the frequency-capped gossip types
// (compact-VDF re-gossip, transaction announces, NewPeak, …) can be paced. That is the
// self-safety property that keeps a strict peer from banning US for a legitimate re-gossip
// burst while never throttling our sync into failure.
//
// There is no single per-connection send loop draining an outgoing queue; sends are driven
// directly by the caller task under the connection write lock. So the throttle runs in the
// CALLER's task BEFORE acquiring the write lock — never holding the write lock across a wait,
// or a deferred gossip type would stall an `Unlimited` RespondBlocks queued behind it on the
// same lock and break the self-safety guarantee. `admit` sleeps `retry_delay` between
// re-checks. It is BOUNDED: after `max_attempts` deferrals the message is shed with a
// `Drop(BackpressureCap)`. Because the limiter window is 60s, `max_attempts * retry_delay`
// spans a full window, so a message that is merely ahead of budget is always admitted once
// the window rolls; only sustained over-budget flooding is shed.

use crate::protocols::ProtocolMessageTypes;
use crate::protocols::rate_limits::RateLimiter;
use crate::protocols::shared::Capabilities;
use std::time::Duration;

/// Re-queue cadence: 1s between attempts.
pub const RETRY_DELAY: Duration = Duration::from_secs(1);

/// Bounded backpressure: the max number of `retry_delay` deferrals before a message is shed. Chosen so
/// `MAX_ATTEMPTS * RETRY_DELAY` (65s) exceeds one 60-second limiter window — a message merely ahead of
/// budget always drains once the window rolls; only sustained flooding hits the cap and is dropped.
pub const MAX_ATTEMPTS: u32 = 65;

/// True for message types dropped WITHOUT re-queue when they are over budget on the send path
/// (`respond_peers`). Every other frequency-capped type is instead deferred and retried.
#[must_use]
pub fn is_requeue_exempt(msg_type: ProtocolMessageTypes) -> bool {
    matches!(msg_type, ProtocolMessageTypes::RespondPeers)
}

/// The classification of one outbound message against the peer's budget.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutboundDecision {
    /// Within budget (or an `Unlimited` type within its per-message size cap): send now. The budget
    /// slot has been committed by this call (the outgoing counter is committed only when allowed).
    Send,
    /// Over budget and NOT re-queue-exempt: wait `retry_delay` and re-check.
    Defer,
    /// Over budget and re-queue-exempt (`respond_peers`): drop now, no retry.
    DropExempt,
}

/// Why an outbound message was not sent (the non-`Admit` outcomes of [`OutboundLimiter::admit`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropReason {
    /// A re-queue-exempt type (`respond_peers`) over budget — dropped without retry.
    Exempt,
    /// `max_attempts` deferrals were exhausted without the budget opening — bounded backpressure shed.
    BackpressureCap,
}

/// The result of running one message through the throttle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThrottleOutcome {
    /// Cleared to send now — the limiter has already committed this message's budget slot.
    Admit,
    /// The message must not be sent.
    Drop(DropReason),
}

/// A per-connection outbound self-throttle. Wraps an `incoming = false` [`RateLimiter`] (the SAME
/// composed v1/v2 table the inbound limiter uses — a peer bans us by THEIR limits, which equal ours
/// under v2-compose since both advertise it) plus the re-queue policy.
pub struct OutboundLimiter {
    limiter: RateLimiter,
    max_attempts: u32,
    retry_delay: Duration,
}

impl OutboundLimiter {
    /// Defaults: a 60-second outbound window at 100% of the published numbers, 1s re-queue
    /// cadence, bounded at [`MAX_ATTEMPTS`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            limiter: RateLimiter::new(false),
            max_attempts: MAX_ATTEMPTS,
            retry_delay: RETRY_DELAY,
        }
    }

    /// Test/tuning constructor: override the window, the percentage, the attempt cap, and the cadence.
    #[must_use]
    pub fn with_params(
        reset_seconds: u64,
        percentage_of_limit: u32,
        max_attempts: u32,
        retry_delay: Duration,
    ) -> Self {
        Self {
            limiter: RateLimiter::with_params(false, reset_seconds, percentage_of_limit),
            max_attempts,
            retry_delay,
        }
    }

    /// Classify one message against the peer's current budget. On [`OutboundDecision::Send`] the
    /// budget slot is committed (the outgoing counter advances only when the message is allowed);
    /// a `Defer`/`DropExempt` does not advance the counter, so re-checking is free of double-counting.
    #[must_use]
    pub fn decide(
        &self,
        msg_type: ProtocolMessageTypes,
        size: usize,
        peer_caps: &Capabilities,
    ) -> OutboundDecision {
        match self.limiter.process_and_check(msg_type, size, peer_caps) {
            None => OutboundDecision::Send,
            Some(_) if is_requeue_exempt(msg_type) => OutboundDecision::DropExempt,
            Some(_) => OutboundDecision::Defer,
        }
    }

    /// Block the CALLER (holding no connection lock) until the message fits the peer's budget, then
    /// return [`ThrottleOutcome::Admit`] — or shed it ([`ThrottleOutcome::Drop`]) when it is exempt or
    /// the attempt cap is reached. It defers (does not drop) a
    /// would-be-oversending message, retrying every `retry_delay`, bounded by `max_attempts`.
    pub async fn admit(
        &self,
        msg_type: ProtocolMessageTypes,
        size: usize,
        peer_caps: &Capabilities,
    ) -> ThrottleOutcome {
        let mut attempts: u32 = 0;
        loop {
            match self.decide(msg_type, size, peer_caps) {
                OutboundDecision::Send => return ThrottleOutcome::Admit,
                OutboundDecision::DropExempt => return ThrottleOutcome::Drop(DropReason::Exempt),
                OutboundDecision::Defer => {
                    if attempts >= self.max_attempts {
                        return ThrottleOutcome::Drop(DropReason::BackpressureCap);
                    }
                    attempts += 1;
                    tokio::time::sleep(self.retry_delay).await;
                }
            }
        }
    }
}

impl Default for OutboundLimiter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[path = "../../tests/unit/protocols/outbound_limiter/tests.rs"]
mod tests;
