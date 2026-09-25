mod common;

use common::{
    empty_api, fast_settings, keepalive_settings, peer, spawn_full_node, spawn_silent_node,
    wait_for_reconnections, wait_until,
};
use dg_xch_p2p::{P2pSettings, Supervisor};
use std::time::{Duration, Instant};

static NETWORK_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[test]
fn jittered_backoff_spreads_reconnects_no_thundering_herd() {
    let s = P2pSettings {
        retry_timeout: Duration::from_millis(1000),
        jitter_floor: 0.5,
        ..P2pSettings::default()
    };
    let samples: Vec<f64> = (0..2000)
        .map(|_| s.jittered_backoff(0).as_secs_f64() * 1000.0)
        .collect();
    let min = samples.iter().cloned().fold(f64::MAX, f64::min);
    let max = samples.iter().cloned().fold(f64::MIN, f64::max);
    let mean = samples.iter().sum::<f64>() / samples.len() as f64;
    println!(
        "[MEASURED] attempt-0 backoff ms: min={min:.1} max={max:.1} mean={mean:.1} (base 1000, floor 0.5)"
    );
    assert!((500.0..560.0).contains(&min), "floor honored: {min}");
    assert!((940.0..=1000.0).contains(&max), "cap honored: {max}");
    assert!(max - min > 350.0, "reconnects are spread, not synchronized");

    let a5 = s.jittered_backoff(5).as_secs_f64() * 1000.0;
    println!("[MEASURED] attempt-5 backoff ms: {a5:.1} (base 32000, floor 0.5)");
    assert!((16_000.0..=32_000.0).contains(&a5));
}

#[tokio::test]
async fn mass_drop_reconnects_all_slots() {
    let _guard = NETWORK_TEST_LOCK.lock().await;
    let settings = P2pSettings {
        target_outbound: 4,
        retry_timeout: Duration::from_millis(400),
        ..fast_settings()
    };
    let mut servers = Vec::new();
    let mut sup = Supervisor::new(settings);
    for _ in 0..4 {
        let srv = spawn_full_node(empty_api()).await;
        sup.seed_addresses(&[peer("127.0.0.1", srv.port, 1)]).await;
        servers.push(srv);
    }
    sup.start_outbound();
    let reg = sup.registry.clone();
    let connected = wait_until(
        || async { reg.outbound_count().await == 4 },
        Duration::from_secs(40),
    )
    .await;
    println!("[MEASURED] slots connected: {}", reg.outbound_count().await);
    assert!(connected, "all four slots connect");

    let previous = reg.outbound_peers().await;
    for srv in &servers {
        common::drop_all_server_peers(srv).await;
    }
    let recovered = wait_for_reconnections(&reg, &previous, Duration::from_secs(60)).await;
    if !recovered {
        let pooled = sup.book.lock().await.len();
        let server_state: Vec<_> = servers
            .iter()
            .map(|server| {
                (
                    server.handle.is_finished(),
                    server.peers.try_read().map(|p| p.len()),
                )
            })
            .collect();
        eprintln!(
            "reconnect stalled: connected={} pooled={pooled} servers={server_state:?}",
            reg.outbound_count().await
        );
    }
    assert!(recovered, "every slot reconnected after the mass drop");

    sup.stop().await;
    for srv in servers {
        srv.run.store(false, std::sync::atomic::Ordering::Relaxed);
    }
}

#[tokio::test]
async fn silent_half_open_peer_is_torn_down_within_the_deadline() {
    let _guard = NETWORK_TEST_LOCK.lock().await;
    let silent = spawn_silent_node().await;
    let settings = keepalive_settings();
    let mut sup = Supervisor::new(settings);
    sup.start_manual("127.0.0.1", silent.port);
    let reg = sup.registry.clone();

    assert!(
        wait_until(
            || async { reg.outbound_count().await == 1 },
            Duration::from_secs(30)
        )
        .await,
        "manual peer connects to the silent node"
    );

    let t0 = Instant::now();
    let torn = wait_until(
        || async { reg.outbound_count().await == 0 },
        Duration::from_secs(5),
    )
    .await;
    let detect = t0.elapsed();
    println!(
        "[MEASURED] half-open teardown in {}ms (heartbeat {}ms + pong deadline {}ms)",
        detect.as_millis(),
        settings.heartbeat.as_millis(),
        settings.pong_deadline.as_millis()
    );
    assert!(torn, "silent peer is torn down");
    assert!(
        detect < settings.heartbeat + settings.pong_deadline + Duration::from_millis(1500),
        "teardown within the deadline, not a TCP limbo"
    );

    sup.stop().await;
    silent
        .run
        .store(false, std::sync::atomic::Ordering::Relaxed);
}
