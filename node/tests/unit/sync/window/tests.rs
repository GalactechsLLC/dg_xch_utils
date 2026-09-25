use super::*;

#[test]
fn stalled_reservation_returns_to_the_pool_with_no_gap() {
    let mut w = ReservationWindow::new(64);
    w.refill(100..110);
    assert_eq!(w.live(), 10);

    // Two peers each split off a contiguous run.
    let Claim::Reserved(a) = w.reserve(3) else {
        panic!("first reservation")
    };
    let Claim::Reserved(b) = w.reserve(3) else {
        panic!("second reservation")
    };
    assert_eq!(a.heights, vec![100, 101, 102]);
    assert_eq!(b.heights, vec![103, 104, 105]);

    // Peer A stalls: its run goes back to the pool, no height is dropped.
    w.reclaim(a.id);
    // Peer B completes normally.
    w.complete(b.id);

    // Every not-yet-written height is claimable; the completed 103..106 are gone.
    let mut got: Vec<u32> = Vec::new();
    while let Claim::Reserved(r) = w.reserve(4) {
        got.extend(r.heights);
    }
    got.sort_unstable();
    assert_eq!(got, vec![100, 101, 102, 106, 107, 108, 109]);
    assert_eq!(w.live(), 7);
}

#[test]
fn window_is_capacity_bounded() {
    let mut w = ReservationWindow::new(8);
    w.refill(0..1000);
    assert_eq!(
        w.live(),
        8,
        "the window never holds more than its cap of identifiers"
    );
}
