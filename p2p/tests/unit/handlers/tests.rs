use super::served;
use dg_xch_core::protocols::ProtocolMessageTypes;

// The dispatch filter must MATCH tip announcements, block requests, the pure-gossip
// broadcasts (so they graceful-ignore instead of logging "No Matches"), AND the four block
// replies (solicited and recently timed-out ones are consumed by the read loop's correlation-id
// fast path before the handler scan, so a match here is genuinely unsolicited → the close arm).
// RespondProofOfWeight stays oneshot-owned and unmatched — it falls to the read loop's
// no-match drop.
#[test]
fn filter_matches_gossip_but_not_oneshot_responses() {
    for t in [
        ProtocolMessageTypes::Handshake,
        ProtocolMessageTypes::NewPeak,
        ProtocolMessageTypes::RequestBlock,
        ProtocolMessageTypes::RequestBlocks,
        ProtocolMessageTypes::RespondBlock,
        ProtocolMessageTypes::RespondBlocks,
        ProtocolMessageTypes::RejectBlock,
        ProtocolMessageTypes::RejectBlocks,
        ProtocolMessageTypes::NewCompactVdf,
        ProtocolMessageTypes::NewSignagePointOrEndOfSubSlot,
        ProtocolMessageTypes::NewUnfinishedBlock2,
        ProtocolMessageTypes::RequestMempoolTransactions,
        ProtocolMessageTypes::RequestProofOfWeight,
        ProtocolMessageTypes::NewInfusionPointVdf,
        ProtocolMessageTypes::NewSignagePointVdf,
        ProtocolMessageTypes::NewEndOfSubSlotVdf,
        ProtocolMessageTypes::SendTransaction,
        ProtocolMessageTypes::RequestPuzzleSolution,
        ProtocolMessageTypes::RequestBlockHeader,
        ProtocolMessageTypes::RequestHeaderBlocks,
        ProtocolMessageTypes::RequestBlockHeaders,
        ProtocolMessageTypes::RequestAdditions,
        ProtocolMessageTypes::RequestRemovals,
        ProtocolMessageTypes::RequestChildren,
        ProtocolMessageTypes::RegisterInterestInPuzzleHash,
        ProtocolMessageTypes::RegisterInterestInCoin,
        ProtocolMessageTypes::RequestPuzzleState,
        ProtocolMessageTypes::RequestCoinState,
        ProtocolMessageTypes::RequestRemovePuzzleSubscriptions,
        ProtocolMessageTypes::RequestRemoveCoinSubscriptions,
        ProtocolMessageTypes::RequestFeeEstimates,
    ] {
        assert!(
            served(t),
            "{t:?} must be handled (dispatched or graceful-ignored)"
        );
    }
    assert!(
        !served(ProtocolMessageTypes::RespondProofOfWeight),
        "RespondProofOfWeight is oneshot-owned and must not match the dispatch filter"
    );
}
