use super::*;

fn claim(hash: [u8; 32], height: u32, weight: u128) -> PeakClaim {
    PeakClaim {
        header_hash: Bytes32::const_new(hash),
        height,
        weight,
    }
}

fn book() -> (Arc<PeakBook>, Arc<AtomicU32>) {
    let published = Arc::new(AtomicU32::new(0));
    (Arc::new(PeakBook::new(published.clone())), published)
}

// Weight orders the target; height only breaks ties.
#[test]
fn heaviest_is_by_weight_not_height() {
    let (b, published) = book();
    b.record(
        Bytes32::const_new([1; 32]),
        true,
        claim([0xAA; 32], 100, 1_000),
    );
    b.record(
        Bytes32::const_new([2; 32]),
        true,
        claim([0xBB; 32], 120, 900),
    );
    assert_eq!(b.heaviest(), Some(claim([0xAA; 32], 100, 1_000)));
    assert_eq!(published.load(Ordering::Relaxed), 100);
}

// A peer's newest announcement REPLACES its claim
// — the withdrawal path for an over-claim.
#[test]
fn a_peers_new_announcement_replaces_its_claim() {
    let (b, published) = book();
    let peer = Bytes32::const_new([1; 32]);
    b.record(peer, true, claim([0xAA; 32], 500, 5_000));
    b.record(peer, true, claim([0xBB; 32], 100, 1_000));
    assert_eq!(b.heaviest(), Some(claim([0xBB; 32], 100, 1_000)));
    assert_eq!(published.load(Ordering::Relaxed), 100);
}

// Retraction rolls the published claim BACK (the fetch_max
// slot this replaces could never regress).
#[test]
fn retract_rolls_the_published_claim_back() {
    let (b, published) = book();
    let bogus = Bytes32::const_new([9; 32]);
    b.record(bogus, true, claim([0xEE; 32], 9_999_999, u128::MAX));
    b.record(
        Bytes32::const_new([1; 32]),
        true,
        claim([0xAA; 32], 100, 1_000),
    );
    assert_eq!(published.load(Ordering::Relaxed), 9_999_999);
    b.retract(&bogus);
    assert_eq!(b.heaviest(), Some(claim([0xAA; 32], 100, 1_000)));
    assert_eq!(published.load(Ordering::Relaxed), 100);
}

// The outbound guard IS the disconnect callback: dropping it retracts the connection's claim.
#[test]
fn claim_guard_drop_retracts_the_outbound_claim() {
    let (b, published) = book();
    let guard = b.outbound_guard();
    b.record(guard.key(), false, claim([0xEE; 32], 9_999_999, u128::MAX));
    assert_eq!(published.load(Ordering::Relaxed), 9_999_999);
    drop(guard);
    assert_eq!(b.heaviest(), None);
    assert_eq!(published.load(Ordering::Relaxed), 0);
}

// Inbound reconcile: claims of departed inbound peers
// are dropped; outbound (guard-keyed) claims are untouched by the inbound reconcile.
#[test]
fn reconcile_drops_departed_inbound_claims_only() {
    let (b, published) = book();
    let inbound = Bytes32::const_new([1; 32]);
    let guard = b.outbound_guard();
    b.record(inbound, true, claim([0xEE; 32], 9_000_000, 9_000));
    b.record(guard.key(), false, claim([0xAA; 32], 100, 1_000));
    b.reconcile(&std::collections::HashSet::new()); // the inbound peer is gone
    assert_eq!(b.heaviest(), Some(claim([0xAA; 32], 100, 1_000)));
    assert_eq!(published.load(Ordering::Relaxed), 100);
}

// A quarantined hash is never re-selected; the next-heaviest claim is.
#[test]
fn quarantined_peak_is_not_reselected() {
    let (b, published) = book();
    b.record(
        Bytes32::const_new([1; 32]),
        true,
        claim([0xEE; 32], 9_000_000, 9_000),
    );
    b.record(
        Bytes32::const_new([2; 32]),
        true,
        claim([0xAA; 32], 100, 1_000),
    );
    b.quarantine(Bytes32::const_new([0xEE; 32]), 9_000_000);
    assert!(b.is_quarantined(&Bytes32::const_new([0xEE; 32])));
    assert_eq!(b.heaviest(), Some(claim([0xAA; 32], 100, 1_000)));
    assert_eq!(published.load(Ordering::Relaxed), 100);
    // A RE-announcement of the quarantined hash stays unselectable.
    b.record(
        Bytes32::const_new([3; 32]),
        true,
        claim([0xEE; 32], 9_000_000, 9_000),
    );
    assert_eq!(b.heaviest(), Some(claim([0xAA; 32], 100, 1_000)));
}

// Bounds: the quarantine cache caps at BAD_PEAK_CACHE_SIZE evicting the lowest height, and
// the claim map caps at MAX_TRACKED_CLAIMS.
#[test]
fn quarantine_cache_and_claim_map_are_bounded() {
    let (b, _) = book();
    for i in 0..=BAD_PEAK_CACHE_SIZE {
        let mut h = [0u8; 32];
        h[..8].copy_from_slice(&(i as u64).to_be_bytes());
        b.quarantine(Bytes32::const_new(h), u32::try_from(i).unwrap());
    }
    assert_eq!(b.lock().bad.len(), BAD_PEAK_CACHE_SIZE);
    // The min-height entry (height 0) was evicted.
    assert!(!b.is_quarantined(&Bytes32::const_new([0u8; 32])));

    for i in 0..(MAX_TRACKED_CLAIMS + 8) {
        let mut k = [0u8; 32];
        k[..8].copy_from_slice(&(i as u64).to_be_bytes());
        b.record(Bytes32::const_new(k), true, claim([0x11; 32], 1, 1));
    }
    assert_eq!(b.lock().claims.len(), MAX_TRACKED_CLAIMS);
}

// outbound_tip is the SERVABLE frontier: the highest OUTBOUND claim (the peers we fetch from),
// NOT the weight-heaviest claim. An inbound peer over-announcing 12 past the real tip becomes the
// weight-heaviest target but must not lift the fetch frontier past what our fetch sources serve.
#[test]
fn outbound_tip_ignores_a_heavier_inbound_over_claim() {
    let (b, _) = book();
    // Inbound peer over-claims beyond the real tip with a heavier weight.
    b.record(
        Bytes32::const_new([1; 32]),
        true,
        claim([0xEE; 32], 9_208_323, u128::MAX),
    );
    // The outbound peers we actually fetch from top out at the real tip.
    let out = b.outbound_guard();
    b.record(out.key(), false, claim([0xAA; 32], 9_208_311, 9_000));
    assert_eq!(
        b.heaviest().map(|c| c.height),
        Some(9_208_323),
        "the inbound over-claim is still the weight-heaviest target"
    );
    assert_eq!(
        b.outbound_tip(),
        Some(9_208_311),
        "but the servable outbound tip is the real tip"
    );
}

// No outbound claim yet (startup) -> None, so the caller leaves the frontier unclamped. A stale
// outbound claim (past the TTL) is not servable either.
#[test]
fn outbound_tip_is_none_without_a_live_outbound_claim() {
    let (b, _) = book();
    assert_eq!(b.outbound_tip(), None);
    // An inbound-only book still yields no servable outbound frontier.
    b.record(
        Bytes32::const_new([1; 32]),
        true,
        claim([0xEE; 32], 100, 1_000),
    );
    assert_eq!(b.outbound_tip(), None);
}

// retract_hash drops EVERY claimant of a never-served tip (the all-peers weight-proof-fetch
// failure path); honest peers re-announce on the next peak and repopulate.
#[test]
fn retract_hash_drops_all_claimants_of_that_tip() {
    let (b, published) = book();
    b.record(
        Bytes32::const_new([1; 32]),
        true,
        claim([0xEE; 32], 9_000_000, 9_000),
    );
    b.record(
        Bytes32::const_new([2; 32]),
        true,
        claim([0xEE; 32], 9_000_000, 9_000),
    );
    b.record(
        Bytes32::const_new([3; 32]),
        true,
        claim([0xAA; 32], 100, 1_000),
    );
    b.retract_hash(&Bytes32::const_new([0xEE; 32]));
    assert_eq!(b.heaviest(), Some(claim([0xAA; 32], 100, 1_000)));
    assert_eq!(published.load(Ordering::Relaxed), 100);
}
