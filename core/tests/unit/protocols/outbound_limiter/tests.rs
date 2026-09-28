use super::*;
use crate::protocols::shared::{CAPABILITIES, Capability};
use std::time::Instant;

const MIB: usize = 1024 * 1024;

fn v2_caps() -> Capabilities {
    CAPABILITIES
        .iter()
        .map(|(v, s)| (*v, (*s).to_string()))
        .collect()
}

// TEST (1): a burst of a frequency-capped gossip type over the peer's per-minute budget is PACED,
// not sent immediately. NewCompactVdf is 100/min (no v2 override). With
// a window that never rolls in-test, the first 100 are admitted (this is what a peer allows) and
// every further one is Defer — delayed rather than send-and-get-banned.
#[test]
fn frequency_capped_burst_is_paced_not_sent_immediately() {
    let ol = OutboundLimiter::with_params(3600, 100, 65, Duration::from_millis(1));
    let caps = v2_caps();
    for _ in 0..100 {
        assert_eq!(
            ol.decide(ProtocolMessageTypes::NewCompactVdf, 64, &caps),
            OutboundDecision::Send,
            "the first 100 NewCompactVdf are within the peer's 100/min budget"
        );
    }
    for _ in 0..10 {
        assert_eq!(
            ol.decide(ProtocolMessageTypes::NewCompactVdf, 64, &caps),
            OutboundDecision::Defer,
            "each over-budget gossip message is DEFERRED (paced), never sent immediately"
        );
    }
}

#[test]
fn unlimited_serve_type_is_never_deferred() {
    let ol = OutboundLimiter::with_params(3600, 100, 65, Duration::from_millis(1));
    let caps = v2_caps();
    for _ in 0..2000 {
        assert_eq!(
            ol.decide(ProtocolMessageTypes::RespondBlocks, 10 * MIB, &caps),
            OutboundDecision::Send,
            "an Unlimited serve type is never throttled — our sync/serve is unaffected"
        );
    }
}

// TEST (3) — the re-queue exemption: respond_peers over budget is DROPPED, not deferred.
// respond_peers is 10/min. The first 10 admit;
// the 11th is DropExempt (not Defer).
#[test]
fn exempt_type_is_dropped_not_deferred_when_over_budget() {
    let ol = OutboundLimiter::with_params(3600, 100, 65, Duration::from_millis(1));
    let caps = v2_caps();
    for _ in 0..10 {
        assert_eq!(
            ol.decide(ProtocolMessageTypes::RespondPeers, 64, &caps),
            OutboundDecision::Send,
        );
    }
    assert_eq!(
        ol.decide(ProtocolMessageTypes::RespondPeers, 64, &caps),
        OutboundDecision::DropExempt,
        "respond_peers is exempt from re-queue — dropped when over budget, never deferred"
    );
}

// TEST (4) — bounded queue: when the budget never opens (window pinned) an over-budget non-exempt
// message is shed after exactly `max_attempts` deferrals with Drop(BackpressureCap). Proves the
// throttle can never grow an unbounded outbound backlog.
#[tokio::test]
async fn admit_sheds_after_max_attempts_when_budget_never_opens() {
    let ol = OutboundLimiter::with_params(3600, 100, 3, Duration::from_millis(1));
    let caps = v2_caps();
    for _ in 0..100 {
        let _ = ol.decide(ProtocolMessageTypes::NewCompactVdf, 64, &caps);
    }
    let outcome = ol
        .admit(ProtocolMessageTypes::NewCompactVdf, 64, &caps)
        .await;
    assert_eq!(
        outcome,
        ThrottleOutcome::Drop(DropReason::BackpressureCap),
        "a pinned-over-budget message is shed after max_attempts — bounded, no unbounded queue"
    );
}

// TEST (1) headline — deferred, NOT dropped, and EVENTUALLY sent once the window rolls. A 1s
// window; fill NewCompactVdf to its 100/min cap, then admit one more: it defers across the window
// boundary and is admitted (not dropped). Real-time ~1s (one slow-ish test).
#[tokio::test]
async fn admit_eventually_sends_over_budget_message_after_window_rolls() {
    let ol = OutboundLimiter::with_params(1, 100, 65, Duration::from_millis(50));
    let caps = v2_caps();
    for _ in 0..100 {
        assert_eq!(
            ol.decide(ProtocolMessageTypes::NewCompactVdf, 64, &caps),
            OutboundDecision::Send,
        );
    }
    let start = Instant::now();
    let outcome = ol
        .admit(ProtocolMessageTypes::NewCompactVdf, 64, &caps)
        .await;
    assert_eq!(
        outcome,
        ThrottleOutcome::Admit,
        "the over-budget message is DELAYED, not dropped, and sent once the window rolls"
    );
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "it drains within a window, not the full attempt cap"
    );
}

// A v1-only peer selects the v1 numbers for the outbound direction too: request_puzzle_solution is
// 1000/min under v1 (5000 under v2), so against a v1 peer the 1001st is deferred. Confirms the
// reused table is capability-driven on the send side exactly as on the receive side.
#[test]
fn v1_only_peer_selects_v1_numbers_on_the_send_side() {
    let ol = OutboundLimiter::with_params(3600, 100, 65, Duration::from_millis(1));
    let v1_caps: Capabilities = vec![
        (Capability::Base as u16, "1".to_string()),
        (Capability::BlockHeaders as u16, "1".to_string()),
    ];
    for _ in 0..1000 {
        assert_eq!(
            ol.decide(ProtocolMessageTypes::RequestPuzzleSolution, 10, &v1_caps),
            OutboundDecision::Send,
        );
    }
    assert_eq!(
        ol.decide(ProtocolMessageTypes::RequestPuzzleSolution, 10, &v1_caps),
        OutboundDecision::Defer,
        "a v1 peer caps request_puzzle_solution at 1000/min on the send side"
    );
}
