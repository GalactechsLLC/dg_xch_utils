use super::*;

// The V3Link window accounting: acquire to the bound, refuse past it, release frees.
#[test]
fn recv_window_admits_to_the_bound_and_refuses_past_it() {
    let link = V3Link::default();
    let t = ProtocolMessageTypes::RequestBlocks; // window 2
    assert_eq!(link.recv_acquire(t), Ok(true));
    assert_eq!(link.recv_acquire(t), Ok(true));
    assert_eq!(link.recv_acquire(t), Err(()), "third in-flight exceeds w=2");
    link.recv_release(t);
    assert_eq!(link.recv_acquire(t), Ok(true), "released slot re-admits");
    // A type outside the v3 table is untracked.
    assert_eq!(link.recv_acquire(ProtocolMessageTypes::NewPeak), Ok(false));
}

#[test]
fn out_window_tracks_ids_against_peer_settings() {
    let link = V3Link::default();
    let peer = settings_from_configure(&configure_message()).expect("our own table is valid");
    link.activate(peer);
    let t = ProtocolMessageTypes::RequestBlocks;
    assert_eq!(link.out_acquire(t, 1), Ok(true));
    assert_eq!(link.out_acquire(t, 2), Ok(true));
    assert_eq!(link.out_acquire(t, 3), Err(()), "peer window 2 is full");
    link.out_release(1);
    assert_eq!(link.out_acquire(t, 3), Ok(true));
    // Responses are unlimited in the peer's table — untracked.
    assert_eq!(
        link.out_acquire(ProtocolMessageTypes::RespondBlocks, 9),
        Ok(false)
    );
    // Unknown id release is a no-op.
    link.out_release(4242);
}
