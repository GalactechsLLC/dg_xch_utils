use super::{ChiaMessage, PendingRequest, PendingRequests, ProtocolMessageTypes};
use crate::blockchain::unsized_bytes::UnsizedBytes;
use crate::protocols::rate_limits_v3::{V3Link, configure_message, settings_from_configure};
use std::collections::HashSet;
use std::sync::Arc;

fn msg(id: Option<u16>, t: ProtocolMessageTypes) -> Arc<ChiaMessage> {
    Arc::new(ChiaMessage {
        msg_type: t,
        id,
        data: UnsizedBytes::new(vec![]),
    })
}

// Allocation is connection-unique and non-zero: a run of registrations (all still in flight)
// hands out strictly distinct, non-zero ids. This is the property whose *absence* — the per-source
// counter reset to 1 — let two concurrent requests share id 1 and produced the 27 s stall.
#[tokio::test]
async fn register_hands_out_distinct_nonzero_ids() {
    let pending = PendingRequests::default();
    let mut ids = HashSet::new();
    let mut _keep = Vec::new();
    for _ in 0..1000 {
        let (id, rx) = pending.register();
        assert_ne!(id, 0, "id 0 is reserved (id-less gossip / handshake)");
        assert!(
            ids.insert(id),
            "id {id} was handed out twice while still in flight"
        );
        _keep.push(rx); // hold the receivers so their ids stay live and cannot be reused
    }
}

// A live id is never re-handed even as the u16 counter advances: with two waiters outstanding, a
// third allocation differs from both.
#[tokio::test]
async fn register_skips_live_ids() {
    let pending = PendingRequests::default();
    let (a, _ra) = pending.register();
    let (b, _rb) = pending.register();
    let (c, _rc) = pending.register();
    assert!(a != b && b != c && a != c, "live ids {a},{b},{c} collided");
}

// A reply is routed to the ONE waiter that owns its id, and only that waiter — the other waiter's
// receiver is untouched. This is the demux invariant: no fan-out to every matching handler.
#[tokio::test]
async fn deliver_routes_to_exactly_the_owning_waiter() {
    let pending = PendingRequests::default();
    let (id_a, rx_a) = pending.register();
    let (id_b, rx_b) = pending.register();

    // Deliver B first, then A — out-of-order, as concurrent replies arrive.
    assert!(pending.deliver(id_b, msg(Some(id_b), ProtocolMessageTypes::RespondBlocks)));
    assert!(pending.deliver(id_a, msg(Some(id_a), ProtocolMessageTypes::RejectBlocks)));

    let got_a = rx_a.await.expect("waiter A received its reply");
    let got_b = rx_b.await.expect("waiter B received its reply");
    assert_eq!(got_a.id, Some(id_a), "waiter A got another request's reply");
    assert_eq!(got_a.msg_type, ProtocolMessageTypes::RejectBlocks);
    assert_eq!(got_b.id, Some(id_b), "waiter B got another request's reply");
    assert_eq!(got_b.msg_type, ProtocolMessageTypes::RespondBlocks);
}

// An id nobody is waiting on (an inbound request to answer, or a stale/late reply) reports
// `false`, so the read loop falls through to the gossip/handler scan instead of dropping it.
#[tokio::test]
async fn deliver_unknown_id_is_not_consumed() {
    let pending = PendingRequests::default();
    assert!(!pending.deliver(4242, msg(Some(4242), ProtocolMessageTypes::NewPeak)));
}

// Delivery consumes the waiter: a duplicate/late second reply for the same id is dropped (returns
// `false`), never routed into an already-satisfied — and now closed — channel. That closed-channel
// re-delivery was precisely how the true reply got lost under id aliasing.
#[tokio::test]
async fn deliver_is_idempotent_after_the_first() {
    let pending = PendingRequests::default();
    let (id, rx) = pending.register();
    assert!(pending.deliver(id, msg(Some(id), ProtocolMessageTypes::RespondBlocks)));
    assert!(
        !pending.deliver(id, msg(Some(id), ProtocolMessageTypes::RespondBlocks)),
        "a second reply for a consumed id must not be re-delivered"
    );
    assert!(rx.await.is_ok(), "the one delivery reached the waiter");
}

// Cancel (timeout / send failure) frees the slot so the table never leaks, and a reply that then
// shows up is treated as unowned.
#[tokio::test]
async fn cancel_frees_the_slot() {
    let pending = PendingRequests::default();
    let (id, _rx) = pending.register();
    pending.cancel(id);
    assert!(!pending.deliver(id, msg(Some(id), ProtocolMessageTypes::RespondBlocks)));
}

// An enclosing timeout drops the request future rather than calling an async cleanup path. The
// guarded request must synchronously free both its correlation waiter and its negotiated V3 slot.
#[test]
fn guarded_request_drop_releases_pending_and_v3_slots() {
    let pending = Arc::new(PendingRequests::default());
    let v3 = Arc::new(V3Link::default());
    v3.activate(settings_from_configure(&configure_message()).expect("valid local settings"));

    let first = PendingRequest::new(pending.clone(), v3.clone());
    let second = PendingRequest::new(pending.clone(), v3.clone());
    assert_eq!(
        v3.out_acquire(ProtocolMessageTypes::RequestBlocks, first.id()),
        Ok(true)
    );
    assert_eq!(
        v3.out_acquire(ProtocolMessageTypes::RequestBlocks, second.id()),
        Ok(true)
    );

    let third = PendingRequest::new(pending.clone(), v3.clone());
    assert_eq!(
        v3.out_acquire(ProtocolMessageTypes::RequestBlocks, third.id()),
        Err(()),
        "the negotiated two-request window starts full"
    );
    let released_id = first.id();
    drop(first);

    assert!(
        !pending.deliver(
            released_id,
            msg(Some(released_id), ProtocolMessageTypes::RespondBlocks)
        ),
        "dropping the request removes its correlation waiter"
    );
    assert_eq!(
        v3.out_acquire(ProtocolMessageTypes::RequestBlocks, third.id()),
        Ok(true),
        "dropping the request immediately releases its V3 slot"
    );
}
