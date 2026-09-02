mod common;

use common::{
    drop_all_server_peers, empty_api, fast_settings, peer, spawn_full_node, spawn_introducer,
    wait_until,
};
use dg_xch_p2p::Supervisor;
use std::time::Duration;

static NETWORK_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

// Deadline headroom: a single dial attempt may legally consume the full connect timeout (5s in
// fast_settings), so a loaded runner fitting two attempts plus backoff needs more than 10s.
// wait_until returns the moment the condition holds — the headroom costs nothing when healthy.

#[tokio::test]
async fn outbound_dials_and_reconnects_and_inbound_accepts() {
    let _guard = NETWORK_TEST_LOCK.lock().await;
    let server = spawn_full_node(empty_api()).await;
    let mut sup = Supervisor::new(fast_settings());
    sup.seed_addresses(&[peer("127.0.0.1", server.port, 1)])
        .await;
    sup.start_outbound();

    let reg = sup.registry.clone();
    assert!(
        wait_until(
            || async { reg.outbound_count().await >= 1 },
            Duration::from_secs(30)
        )
        .await,
        "outbound slot dials the loopback peer"
    );
    // inbound accept: the server admitted the dialing peer
    assert!(
        !server.peers.read().await.is_empty(),
        "server accepted inbound"
    );

    // Drop the peer server-side; the slot must reconnect from OUTSIDE the dead channel.
    drop_all_server_peers(&server).await;
    assert!(
        wait_until(
            || async { reg.outbound_count().await == 0 },
            Duration::from_secs(30)
        )
        .await,
        "drop is detected"
    );
    assert!(
        wait_until(
            || async { reg.outbound_count().await >= 1 },
            Duration::from_secs(30)
        )
        .await,
        "slot re-dials a fresh channel after the drop"
    );

    sup.stop().await;
    assert_eq!(
        reg.outbound_count().await,
        0,
        "all channels drained on stop"
    );
    server
        .run
        .store(false, std::sync::atomic::Ordering::Relaxed);
}

#[tokio::test]
async fn manual_peer_persists_across_a_drop() {
    let _guard = NETWORK_TEST_LOCK.lock().await;
    let server = spawn_full_node(empty_api()).await;
    let mut sup = Supervisor::new(fast_settings());
    sup.start_manual("127.0.0.1", server.port);

    let reg = sup.registry.clone();
    assert!(
        wait_until(
            || async { reg.outbound_count().await == 1 },
            Duration::from_secs(30)
        )
        .await,
        "manual peer connects"
    );

    drop_all_server_peers(&server).await;
    assert!(
        wait_until(
            || async { reg.outbound_count().await == 0 },
            Duration::from_secs(30)
        )
        .await,
        "drop detected"
    );
    assert!(
        wait_until(
            || async { reg.outbound_count().await == 1 },
            Duration::from_secs(30)
        )
        .await,
        "manual peer reconnects (persists across the drop)"
    );

    sup.stop().await;
    server
        .run
        .store(false, std::sync::atomic::Ordering::Relaxed);
}

#[tokio::test]
async fn seed_is_one_shot_and_fills_the_pool() {
    let _guard = NETWORK_TEST_LOCK.lock().await;
    // The introducer hands back two peers over the introducer protocol (RequestPeersIntroducer ->
    // RespondPeersIntroducer); the seed pulls them into the book and exits.
    let server = spawn_introducer(vec![peer("1.1.1.1", 8444, 42), peer("2.2.2.2", 8444, 42)]).await;
    let settings = fast_settings();
    let book = std::sync::Arc::new(tokio::sync::Mutex::new(dg_xch_p2p::AddressBook::new(
        &settings,
    )));
    let accepted = dg_xch_p2p::seed_once("127.0.0.1", server.port, book.clone(), &settings)
        .await
        .expect("seed completes");
    assert_eq!(accepted, 2, "seed filled the address pool then completed");
    assert_eq!(book.lock().await.len(), 2);
    server
        .run
        .store(false, std::sync::atomic::Ordering::Relaxed);
}
