use super::tip_epoch_from;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::blockchain::sub_epoch_summary::SubEpochSummary;

fn ses(new_difficulty: Option<u64>, new_sub_slot_iters: Option<u64>) -> SubEpochSummary {
    SubEpochSummary {
        prev_subepoch_summary_hash: Bytes32::default(),
        reward_chain_hash: Bytes32::default(),
        num_blocks_overflow: 0,
        new_difficulty,
        new_sub_slot_iters,
    }
}

#[test]
fn empty_summaries_fall_back_to_genesis_constants() {
    assert_eq!(tip_epoch_from(&[], 128, 7), (128, 7));
}

#[test]
fn tip_epoch_takes_the_last_declared_values() {
    let s = [
        ses(Some(7), Some(128)),
        ses(Some(9), None),
        ses(None, Some(1024)),
    ];
    // last new_difficulty is 9 (third has None), last new_sub_slot_iters is 1024
    assert_eq!(tip_epoch_from(&s, 64, 3), (1024, 9));
}

#[test]
fn undeclared_fields_hold_the_starting_value() {
    let s = [ses(None, None), ses(None, None)];
    assert_eq!(tip_epoch_from(&s, 64, 3), (64, 3));
}

// Pending-boundary depth math, pinned to mainnet constants (epoch_blocks = 4608,
// sub_epoch_blocks = 384, boundary 4,575,744, previous surpass 4,571,136): every position that
// can still trigger the 4,575,744 retarget must demand records down to 4,571,008.
#[test]
fn epoch_backfill_low_covers_the_pending_boundary_retarget() {
    use super::epoch_backfill_low;
    let (e, s) = (4608u32, 384u32);
    // Mid-epoch anchor base (a sync leg's --sync-from=4575000 span base H-64): the next
    // boundary IS the pending boundary; old and new formulas agree.
    assert_eq!(epoch_backfill_low(4_574_936, e, s), 4_571_008);
    // Peak just past the boundary, retarget trigger still ahead: naive next-boundary rounding
    // would demand only 4,575,616 — one full epoch short.
    assert_eq!(epoch_backfill_low(4_575_757, e, s), 4_571_008);
    // The boundary block itself and the last height inside the trigger window.
    assert_eq!(epoch_backfill_low(4_575_744, e, s), 4_571_008);
    assert_eq!(epoch_backfill_low(4_576_127, e, s), 4_571_008);
    // Past the trigger window: the 4,575,744 retarget must have fired; only the NEXT
    // boundary (4,580,352) remains pending, whose surpass depth is 4,575,616.
    assert_eq!(epoch_backfill_low(4_576_128, e, s), 4_575_616);
    // Genesis-side saturation: never underflows.
    assert_eq!(epoch_backfill_low(0, e, s), 0);
    assert_eq!(epoch_backfill_low(383, e, s), 0);
}

#[test]
fn is_missing_record_matches_only_the_notfound_walk_error() {
    use super::SyncError;
    use crate::error::NodeError;
    let missing = SyncError::Node(NodeError::Io(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "block record not found: 0xdead",
    )));
    assert!(missing.is_missing_record());
    assert!(!missing.is_orphan());
    let invalid = SyncError::Node(NodeError::Io(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        "INVALID_VDF",
    )));
    assert!(!invalid.is_missing_record());
    let orphan = SyncError::Node(NodeError::Orphan("h".into()));
    assert!(!orphan.is_missing_record());
    let io = SyncError::Io(std::io::Error::new(std::io::ErrorKind::NotFound, "socket"));
    assert!(!io.is_missing_record());
}

#[test]
fn epoch_schedule_resolves_per_height_and_matches_the_tip_anchor() {
    use super::EpochSchedule;
    let sub_epoch_blocks = 64u32;
    // sub-epoch 0 summary declares (d=9, ssi=1024) -> active from sub-epoch 1;
    // sub-epoch 2 summary declares (d=11, ssi=2048) -> active from sub-epoch 3.
    let s = [
        ses(Some(9), Some(1024)),
        ses(None, None),
        ses(Some(11), Some(2048)),
        ses(None, None),
    ];
    let sched = EpochSchedule::from_summaries(&s, sub_epoch_blocks, 128, 7);
    assert_eq!(
        sched.at(0),
        (128, 7),
        "before any activation: starting values"
    );
    assert_eq!(sched.at(63), (128, 7), "last block of sub-epoch 0");
    assert_eq!(
        sched.at(64),
        (1024, 9),
        "first block of sub-epoch 1: summary 0 active"
    );
    assert_eq!(sched.at(191), (1024, 9), "held through sub-epoch 2");
    assert_eq!(sched.at(192), (2048, 11), "sub-epoch 3: summary 2 active");
    assert_eq!(
        sched.at(64 * 10),
        tip_epoch_from(&s, 128, 7),
        "at the tip the schedule equals the tip anchor"
    );
}
