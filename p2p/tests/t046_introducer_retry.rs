mod common;

#[tokio::test]
async fn registration_runs_even_when_outbound_target_is_already_met() {
    let intro_port = common::free_port();
    let (introducer, queries) = common::spawn_introducer_on(intro_port, vec![]).await;
    let settings = dg_xch_p2p::P2pSettings {
        target_outbound: 0,
        ..common::fast_settings()
    };
    let mut supervisor = dg_xch_p2p::Supervisor::new(settings);
    supervisor.start_introducer_registration("127.0.0.1", intro_port);
    assert!(
        common::wait_until(
            || async { queries.load(std::sync::atomic::Ordering::Relaxed) > 0 },
            common::network::NETWORK_TIMEOUT,
        )
        .await
    );
    supervisor.stop().await;
    introducer
        .run
        .store(false, std::sync::atomic::Ordering::Relaxed);
}

use common::{fast_settings, free_port, peer, spawn_introducer_on, wait_until};
use dg_xch_p2p::Supervisor;
use std::sync::atomic::Ordering;
use std::time::Duration;

// Fail the first TLS connection, then serve an introducer on the same address.
// Observe the initial attempt so slow RSA key generation cannot skip the failure phase.
#[tokio::test]
async fn boot_time_introducer_failure_recovers_when_the_introducer_appears() {
    common::install_crypto();
    let unavailable = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("reserve introducer endpoint");
    let port = unavailable.local_addr().expect("introducer address").port();
    let settings = fast_settings();
    let mut sup = Supervisor::new(settings);
    sup.start_introducer("127.0.0.1", port);

    // Reject an observed connection instead of guessing when the first dial starts.
    let (first_attempt, _) =
        tokio::time::timeout(common::network::NETWORK_TIMEOUT, unavailable.accept())
            .await
            .expect("boot-time dial starts")
            .expect("accept boot-time dial");
    drop(first_attempt);
    drop(unavailable);
    assert!(
        sup.book.lock().await.is_empty(),
        "no introducer yet — the book must still be empty"
    );

    // The introducer becomes reachable on the SAME endpoint (DNS/service now ready).
    let (server, _queries) = spawn_introducer_on(
        port,
        vec![peer("1.1.1.1", 8444, 42), peer("2.2.2.2", 8444, 42)],
    )
    .await;

    let book = sup.book.clone();
    assert!(
        wait_until(
            || async { book.lock().await.len() == 2 },
            common::network::NETWORK_TIMEOUT
        )
        .await,
        "the introducer session must retry past the boot failure and seed the book \
         once the introducer is reachable (the one-shot seed never retried)"
    );

    sup.stop().await;
    server.run.store(false, Ordering::Relaxed);
}

#[tokio::test]
async fn introducer_is_quiet_while_the_book_can_supply_candidates() {
    let intro_port = free_port();
    let (intro, queries) = spawn_introducer_on(intro_port, vec![]).await;

    let mut sup = Supervisor::new(fast_settings());
    // The book holds a candidate (routability is the outbound slots' problem, not the seed's).
    sup.seed_addresses(&[peer("203.0.113.1", 8444, 1)]).await;
    sup.start_introducer("127.0.0.1", intro_port);

    // Several retry windows: below target (0 < 2) but the book is non-empty → no queries.
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(
        queries.load(Ordering::Relaxed),
        0,
        "no introducer queries while the address book holds a candidate"
    );

    sup.stop().await;
    intro.run.store(false, Ordering::Relaxed);
}

#[tokio::test]
async fn introducer_is_quiet_once_the_outbound_target_is_met() {
    let intro_port = free_port();
    let (intro, queries) = spawn_introducer_on(intro_port, vec![]).await;

    // target_outbound = 0: the (empty) live outbound set already meets the target.
    let settings = dg_xch_p2p::P2pSettings {
        target_outbound: 0,
        ..fast_settings()
    };
    let mut sup = Supervisor::new(settings);
    sup.start_introducer("127.0.0.1", intro_port);

    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(
        queries.load(Ordering::Relaxed),
        0,
        "no introducer queries while the outbound target is met"
    );

    sup.stop().await;
    intro.run.store(false, Ordering::Relaxed);
}

#[tokio::test]
async fn introducer_refreshes_while_all_pooled_candidates_are_cooling() {
    let intro_port = free_port();
    let (intro, queries) = spawn_introducer_on(intro_port, vec![peer("2.2.2.2", 8444, 42)]).await;
    let settings = fast_settings();
    let mut sup = Supervisor::new(settings);
    sup.seed_addresses(&[peer("1.1.1.1", 8444, 42)]).await;
    let cooling = sup.book.lock().await.take().expect("seeded candidate");
    sup.book
        .lock()
        .await
        .cooldown(&cooling, common::network::NETWORK_TIMEOUT * 2);

    sup.start_introducer("127.0.0.1", intro_port);
    let book = sup.book.clone();
    assert!(
        wait_until(
            || {
                let book = book.clone();
                async move { book.lock().await.len() == 2 }
            },
            common::network::NETWORK_TIMEOUT,
        )
        .await,
        "a non-empty book with no ready candidates must not suppress introducer refresh"
    );
    assert!(queries.load(std::sync::atomic::Ordering::Relaxed) > 0);

    sup.stop().await;
    intro.run.store(false, std::sync::atomic::Ordering::Relaxed);
}
