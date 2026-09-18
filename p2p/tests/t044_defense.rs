mod common;

use common::{empty_api, fast_settings, peer, spawn_full_node, wait_until};
use dg_xch_p2p::Supervisor;
use std::time::Duration;

static NETWORK_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

// NAT hairpin: once we advertise our WAN pair, our own address enters the gossip pool and
// an outbound slot can try to dial us. The self-authority guard must refuse it even though
// a real server is listening at that address and would otherwise accept the connection.
#[tokio::test]
async fn self_dial_via_advertised_authority_is_refused() {
    let _guard = NETWORK_TEST_LOCK.lock().await;
    let server = spawn_full_node(empty_api()).await; // this IS us, reachable
    let mut sup = Supervisor::new(fast_settings());
    // advertise + seed our own reachable authority (the hairpin setup)
    sup.registry
        .add_self(("127.0.0.1".to_string(), server.port))
        .await;
    sup.seed_addresses(&[peer("127.0.0.1", server.port, 1)])
        .await;
    sup.start_outbound();

    let reg = sup.registry.clone();
    // give the slots ample time to try the (only) address; a self-dial must never register.
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(
        reg.outbound_count().await,
        0,
        "self-dial through the advertised authority is detected and dropped"
    );

    sup.stop().await;
    server
        .run
        .store(false, std::sync::atomic::Ordering::Relaxed);
}

// A misbehaving/slow peer is evicted; the owning slot tears the channel down and re-dials
// (eviction never revives the dead channel).
#[tokio::test]
async fn slow_peer_eviction_tears_down_and_the_slot_recovers() {
    let _guard = NETWORK_TEST_LOCK.lock().await;
    let server = spawn_full_node(empty_api()).await;
    let mut sup = Supervisor::new(fast_settings());
    sup.seed_addresses(&[peer("127.0.0.1", server.port, 1)])
        .await;
    sup.start_outbound();

    let reg = sup.registry.clone();
    assert!(
        wait_until(
            || async { reg.outbound_count().await == 1 },
            Duration::from_secs(60)
        )
        .await,
        "peer connects"
    );

    // Evict it (as the sync pipeline would a slow_channel).
    let ep = ("127.0.0.1".to_string(), server.port);
    assert!(reg.evict(&ep).await, "eviction targets the live channel");
    assert!(
        wait_until(
            || async { reg.outbound_count().await == 0 },
            Duration::from_secs(15)
        )
        .await,
        "evicted channel is torn down"
    );
    // the reservation returns to the pool and the slot re-dials
    assert!(
        wait_until(
            || async { reg.outbound_count().await == 1 },
            Duration::from_secs(60)
        )
        .await,
        "the slot re-dials after eviction (reservation not lost)"
    );

    sup.stop().await;
    server
        .run
        .store(false, std::sync::atomic::Ordering::Relaxed);
}
