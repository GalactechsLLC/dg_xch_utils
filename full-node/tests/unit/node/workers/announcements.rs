use super::*;

fn ip(s: &str) -> IpAddr {
    s.parse().unwrap()
}

// An outbound dial's dispatch id is our OWN cert hash, shared across every outbound peer,
// so exclusion must key on the remote host or the origin peer gets its own echo back.
#[test]
fn outbound_origin_is_excluded_by_remote_host() {
    let our_id = Bytes32::from([0xAB; 32]); // our shared outbound dispatch id
    let origin = TxOrigin {
        peer_id: our_id,
        host: Some(ip("203.0.113.7")),
    };
    // The origin outbound peer (same host) MUST be excluded.
    assert!(
        is_tx_rebroadcast_origin(Some(&origin), None, Some(ip("203.0.113.7"))),
        "the outbound peer the tx arrived from must not get an echo"
    );
    // A DIFFERENT outbound peer (other host) must still receive it.
    assert!(
        !is_tx_rebroadcast_origin(Some(&origin), None, Some(ip("198.51.100.9"))),
        "a non-origin outbound peer must still receive the re-broadcast"
    );
}

// The inbound-origin case stays exact on the cert-hash id (unchanged behavior).
#[test]
fn inbound_origin_is_excluded_by_exact_peer_id() {
    let origin_id = Bytes32::from([0x11; 32]);
    let origin = TxOrigin {
        peer_id: origin_id,
        host: Some(ip("198.51.100.9")),
    };
    assert!(
        is_tx_rebroadcast_origin(Some(&origin), Some(&origin_id), None),
        "the inbound origin is excluded by its exact cert-hash id"
    );
    let other = Bytes32::from([0x22; 32]);
    assert!(
        !is_tx_rebroadcast_origin(Some(&origin), Some(&other), None),
        "a different inbound peer still receives the re-broadcast"
    );
}

// No recorded origin (e.g. a locally-pushed tx) → nobody excluded.
#[test]
fn no_origin_excludes_nobody() {
    assert!(!is_tx_rebroadcast_origin(
        None,
        None,
        Some(ip("203.0.113.7"))
    ));
    assert!(!is_tx_rebroadcast_origin(
        None,
        Some(&Bytes32::from([0x33; 32])),
        None
    ));
}
