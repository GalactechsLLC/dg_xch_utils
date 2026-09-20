use super::{
    ChiaMessage, PendingDelivery, PendingRequest, PendingRequests, ProtocolMessageTypes,
    RETIRED_REQUEST_CAP,
};
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
        let (id, rx) = pending.register().unwrap();
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
    let (a, _ra) = pending.register().unwrap();
    let (b, _rb) = pending.register().unwrap();
    let (c, _rc) = pending.register().unwrap();
    assert!(a != b && b != c && a != c, "live ids {a},{b},{c} collided");
}

// A reply is routed to the ONE waiter that owns its id, and only that waiter — the other waiter's
// receiver is untouched. This is the demux invariant: no fan-out to every matching handler.
#[tokio::test]
async fn deliver_routes_to_exactly_the_owning_waiter() {
    let pending = PendingRequests::default();
    let (id_a, rx_a) = pending.register().unwrap();
    let (id_b, rx_b) = pending.register().unwrap();

    // Deliver B first, then A — out-of-order, as concurrent replies arrive.
    assert_eq!(
        pending.deliver(id_b, msg(Some(id_b), ProtocolMessageTypes::RespondBlocks)),
        PendingDelivery::Delivered
    );
    assert_eq!(
        pending.deliver(id_a, msg(Some(id_a), ProtocolMessageTypes::RejectBlocks)),
        PendingDelivery::Delivered
    );

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
    assert_eq!(
        pending.deliver(4242, msg(Some(4242), ProtocolMessageTypes::NewPeak)),
        PendingDelivery::Unmatched
    );
}

// Delivery consumes the waiter: a duplicate second reply for the same id is unmatched, never
// routed into an already-satisfied — and now closed — channel. That closed-channel re-delivery was
// precisely how the true reply got lost under id aliasing.
#[tokio::test]
async fn deliver_is_idempotent_after_the_first() {
    let pending = PendingRequests::default();
    let (id, rx) = pending.register().unwrap();
    assert_eq!(
        pending.deliver(id, msg(Some(id), ProtocolMessageTypes::RespondBlocks)),
        PendingDelivery::Delivered
    );
    assert_eq!(
        pending.deliver(id, msg(Some(id), ProtocolMessageTypes::RespondBlocks)),
        PendingDelivery::Unmatched,
        "a second reply for a consumed id must not be re-delivered"
    );
    assert!(rx.await.is_ok(), "the one delivery reached the waiter");
}

// Cancel (timeout / send failure) frees the waiter but retains a bounded tombstone. A late block
// response is consumed rather than reaching the unsolicited-response ban path.
#[tokio::test]
async fn cancel_tolerates_late_block_reply() {
    let pending = PendingRequests::default();
    let (id, _rx) = pending.register().unwrap();
    pending.cancel(id);
    assert_eq!(
        pending.deliver(id, msg(Some(id), ProtocolMessageTypes::RespondBlocks)),
        PendingDelivery::Retired
    );
    assert_eq!(
        pending.deliver(id, msg(Some(id), ProtocolMessageTypes::RespondBlocks)),
        PendingDelivery::Retired,
        "duplicate late replies remain harmless while the tombstone is retained"
    );
}

#[tokio::test]
async fn retired_ids_only_consume_block_replies() {
    let pending = PendingRequests::default();
    let (id, _rx) = pending.register().unwrap();
    pending.cancel(id);
    assert_eq!(
        pending.deliver(id, msg(Some(id), ProtocolMessageTypes::RequestBlocks)),
        PendingDelivery::Unmatched,
        "an inbound request whose id collides with a tombstone must still reach its handler"
    );
}

#[tokio::test]
async fn retired_id_history_is_bounded() {
    let pending = PendingRequests::default();
    let mut first = 0;
    for i in 0..=RETIRED_REQUEST_CAP {
        let (id, _rx) = pending.register().unwrap();
        if i == 0 {
            first = id;
        }
        pending.cancel(id);
    }
    assert_eq!(
        pending.deliver(first, msg(Some(first), ProtocolMessageTypes::RespondBlocks)),
        PendingDelivery::Unmatched,
        "the oldest tombstone is evicted when the bounded history fills"
    );
}

// An enclosing timeout drops the request future rather than calling an async cleanup path. The
// guarded request must synchronously free both its correlation waiter and its negotiated V3 slot.
#[test]
fn guarded_request_drop_releases_pending_and_v3_slots() {
    let pending = Arc::new(PendingRequests::default());
    let v3 = Arc::new(V3Link::default());
    v3.activate(settings_from_configure(&configure_message()).expect("valid local settings"));

    let first = PendingRequest::new(pending.clone(), v3.clone(), None).unwrap();
    let second = PendingRequest::new(pending.clone(), v3.clone(), None).unwrap();
    assert_eq!(
        v3.out_acquire(ProtocolMessageTypes::RequestBlocks, first.id()),
        Ok(true)
    );
    assert_eq!(
        v3.out_acquire(ProtocolMessageTypes::RequestBlocks, second.id()),
        Ok(true)
    );

    let third = PendingRequest::new(pending.clone(), v3.clone(), None).unwrap();
    assert_eq!(
        v3.out_acquire(ProtocolMessageTypes::RequestBlocks, third.id()),
        Err(()),
        "the negotiated two-request window starts full"
    );
    let released_id = first.id();
    drop(first);

    assert_eq!(
        pending.deliver(
            released_id,
            msg(Some(released_id), ProtocolMessageTypes::RespondBlocks)
        ),
        PendingDelivery::Retired,
        "dropping the request removes its correlation waiter"
    );
    assert_eq!(
        v3.out_acquire(ProtocolMessageTypes::RequestBlocks, third.id()),
        Ok(true),
        "dropping the request immediately releases its V3 slot"
    );
}

#[tokio::test]
async fn wrong_response_type_does_not_consume_pending_request() {
    let pending = PendingRequests::default();
    let filter = Arc::new(super::ChiaMessageFilter {
        msg_type: Some(ProtocolMessageTypes::RespondBlocks),
        id: None,
        custom_fn: None,
    });
    let (id, mut receiver) = pending.register_matching(Some(filter)).unwrap();
    assert_eq!(
        pending.deliver(id, msg(Some(id), ProtocolMessageTypes::RespondBlock)),
        PendingDelivery::Unmatched
    );
    assert!(matches!(
        receiver.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Empty)
    ));
    assert_eq!(
        pending.deliver(id, msg(Some(id), ProtocolMessageTypes::RespondBlocks)),
        PendingDelivery::Delivered
    );
    assert_eq!(
        receiver.await.unwrap().msg_type,
        ProtocolMessageTypes::RespondBlocks
    );
}

#[test]
fn exhausted_request_ids_return_an_error() {
    let pending = PendingRequests::default();
    for _ in 0..u16::MAX {
        pending.register().unwrap();
    }
    assert_eq!(
        pending.register().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[tokio::test]
async fn subscription_drop_removes_handler_after_lock_contention() {
    struct Handler;
    #[async_trait::async_trait]
    impl super::MessageHandler for Handler {
        async fn handle(
            &self,
            _message: Arc<ChiaMessage>,
            _peer_id: Arc<crate::blockchain::sized_bytes::Bytes32>,
            _peers: super::PeerMap,
        ) -> Result<(), std::io::Error> {
            Ok(())
        }
    }
    let id = uuid::Uuid::new_v4();
    let handlers = Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new()));
    handlers.write().await.insert(
        id,
        Arc::new(super::ChiaMessageHandler {
            filter: Arc::new(super::ChiaMessageFilter {
                msg_type: None,
                id: None,
                custom_fn: None,
            }),
            handle: Arc::new(Handler),
        }),
    );
    let subscription = super::MessageSubscription {
        id,
        handlers: handlers.clone(),
    };
    let read_guard = handlers.read().await;
    drop(subscription);
    assert_eq!(read_guard.len(), 1);
    drop(read_guard);
    tokio::task::yield_now().await;
    assert!(handlers.read().await.is_empty());
}
