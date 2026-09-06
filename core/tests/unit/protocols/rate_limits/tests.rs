use super::*;
use crate::protocols::shared::CAPABILITIES;

fn v2_caps() -> Capabilities {
    CAPABILITIES
        .iter()
        .map(|(v, s)| (*v, (*s).to_string()))
        .collect()
}

// v1-only peer: advertises Base + BlockHeaders but NOT RateLimitsV2.
fn v1_caps() -> Capabilities {
    vec![
        (Capability::Base as u16, "1".to_string()),
        (Capability::BlockHeaders as u16, "1".to_string()),
    ]
}

// GREEN #1: request_puzzle_solution is 1000/min in v1 and 5000/min in v2.
// A v2 peer exceeding the v1 cap is admitted; a v1-only peer is cut at 1000.
#[test]
fn v2_selection_lifts_request_puzzle_solution_to_5000() {
    let v2 = RateLimiter::new(true);
    let caps = v2_caps();
    for _ in 0..5000 {
        assert!(
            v2.process_and_check(ProtocolMessageTypes::RequestPuzzleSolution, 10, &caps)
                .is_none(),
            "under v2 the first 5000 are within budget"
        );
    }
    assert!(
        v2.process_and_check(ProtocolMessageTypes::RequestPuzzleSolution, 10, &caps)
            .is_some(),
        "the 5001st trips the v2 cap"
    );

    let v1 = RateLimiter::new(true);
    let caps = v1_caps();
    for _ in 0..1000 {
        assert!(
            v1.process_and_check(ProtocolMessageTypes::RequestPuzzleSolution, 10, &caps)
                .is_none()
        );
    }
    assert!(
        v1.process_and_check(ProtocolMessageTypes::RequestPuzzleSolution, 10, &caps)
            .is_some(),
        "a v1-only peer is still cut at 1000"
    );
}

// GREEN #2: a single oversized message is a violation on its own.
// RequestBlock max_size is 100 bytes.
#[test]
fn oversize_single_message_is_a_violation() {
    let rl = RateLimiter::new(true);
    let caps = v2_caps();
    assert!(
        rl.process_and_check(ProtocolMessageTypes::RequestBlock, 200, &caps)
            .is_some(),
        "a 200-byte RequestBlock exceeds the 100-byte per-message cap"
    );
    // A well-sized one passes.
    assert!(
        rl.process_and_check(ProtocolMessageTypes::RequestBlock, 100, &caps)
            .is_none(),
        "exactly at the cap is allowed (strict >)"
    );
}

// GREEN #3: the non-tx aggregate window trips across DISTINCT types each under their own budget.
// Five types at frequency 200 (1000 messages) then a sixth type's first message is over 1000.
#[test]
fn non_tx_aggregate_trips_across_distinct_types() {
    let rl = RateLimiter::new(true);
    let caps = v2_caps();
    let at_freq_200 = [
        ProtocolMessageTypes::NewPeak,
        ProtocolMessageTypes::NewSignagePointOrEndOfSubSlot,
        ProtocolMessageTypes::RequestSignagePointOrEndOfSubSlot,
        ProtocolMessageTypes::NewUnfinishedBlock,
        ProtocolMessageTypes::RequestUnfinishedBlock,
    ];
    for t in at_freq_200 {
        for _ in 0..200 {
            assert!(
                rl.process_and_check(t, 10, &caps).is_none(),
                "each type is under its own per-type budget"
            );
        }
    }
    assert!(
        rl.process_and_check(ProtocolMessageTypes::RespondUnfinishedBlock, 10, &caps)
            .is_some(),
        "the 1001st non-tx message trips the cross-type aggregate window"
    );
}

// A "tx" type (aggregate_limit=false) does NOT draw on the aggregate window: after 1000 non-tx
// messages the aggregate is full, but a tx type is still admitted on its own budget.
#[test]
fn tx_types_are_outside_the_aggregate_window() {
    let rl = RateLimiter::new(true);
    let caps = v2_caps();
    // Fill the aggregate exactly to 1000 with NewPeak (freq 200) across five 200-runs.
    for t in [
        ProtocolMessageTypes::NewPeak,
        ProtocolMessageTypes::NewSignagePointOrEndOfSubSlot,
        ProtocolMessageTypes::RequestSignagePointOrEndOfSubSlot,
        ProtocolMessageTypes::NewUnfinishedBlock,
        ProtocolMessageTypes::RequestUnfinishedBlock,
    ] {
        for _ in 0..200 {
            let _ = rl.process_and_check(t, 10, &caps);
        }
    }
    // NewTransaction is a tx type: unaffected by the full aggregate.
    assert!(
        rl.process_and_check(ProtocolMessageTypes::NewTransaction, 10, &caps)
            .is_none(),
        "a tx type must not be charged against the non-tx aggregate window"
    );
}

// Unlimited response types (RespondBlocks) have no frequency budget — only a per-message size cap.
#[test]
fn unlimited_types_bound_only_message_size() {
    let rl = RateLimiter::new(true);
    let caps = v2_caps();
    // Many large-but-legal RespondBlocks pass (no frequency limit).
    for _ in 0..100 {
        assert!(
            rl.process_and_check(
                ProtocolMessageTypes::RespondBlocks,
                10 * MIB as usize,
                &caps
            )
            .is_none()
        );
    }
    // One over the 50 MiB per-message cap is a violation.
    assert!(
        rl.process_and_check(
            ProtocolMessageTypes::RespondBlocks,
            (50 * MIB + 1) as usize,
            &caps
        )
        .is_some(),
        "a RespondBlocks over 50 MiB is a violation"
    );
}

#[test]
fn late_protocol_rows_match_chia_numbers() {
    use ProtocolMessageTypes as P;
    let cases: [(P, bool, u32, u64, Option<u64>); 5] = [
        (P::Error, false, 50000, 100, None),
        (P::ConfigureWindowSizes, true, 5, 1024, Some(1024)),
        (P::Solve, false, 120, 1024, None),
        (P::SolutionResponse, false, 120, 1024, None),
        (P::PartialProofs, false, 120, 3 * 1024, None),
    ];
    for (t, aggregate, freq, max_size, max_total) in cases {
        match composed_limit(t, true) {
            Limit::Rl(s) => {
                assert_eq!(s.aggregate_limit, aggregate, "{t:?} aggregate flag");
                assert_eq!(s.frequency, freq, "{t:?} frequency");
                assert_eq!(s.max_size, max_size, "{t:?} max_size");
                assert_eq!(s.max_total_size, max_total, "{t:?} max_total_size");
            }
            Limit::Unlimited(_) => panic!("{t:?} must be a time-based RLSettings row"),
        }
    }
}

// Incoming counters advance unconditionally: once over budget, they stay over.
#[test]
fn incoming_counters_advance_even_on_violation() {
    let rl = RateLimiter::new(true);
    let caps = v2_caps();
    for _ in 0..5 {
        let _ = rl.process_and_check(ProtocolMessageTypes::RequestProofOfWeight, 10, &caps);
    }
    // 6th and 7th both violate — the counter kept climbing past the 5/min cap.
    assert!(
        rl.process_and_check(ProtocolMessageTypes::RequestProofOfWeight, 10, &caps)
            .is_some()
    );
    assert!(
        rl.process_and_check(ProtocolMessageTypes::RequestProofOfWeight, 10, &caps)
            .is_some()
    );
}
