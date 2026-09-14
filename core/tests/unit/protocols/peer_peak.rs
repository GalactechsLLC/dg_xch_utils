use super::*;

#[test]
fn connection_peak_replaces_expires_and_does_not_cross_connections() {
    let peak = PeerPeak::default();
    assert_eq!(peak.height(), None);
    peak.record(100);
    assert_eq!(peak.height(), Some(100));
    peak.record(99);
    assert_eq!(peak.height(), Some(99));
    assert_eq!(
        peak.height_at(Instant::now() + Duration::from_secs(301)),
        None
    );
    assert_eq!(PeerPeak::default().height(), None);
    peak.record(0);
    assert_eq!(peak.height(), Some(0));
}
