use dg_xch_core::{
    blockchain::{peer_info::TimestampedPeerInfo, sized_bytes::Bytes32},
    constants::{CHIA_CA_CRT, CHIA_CA_KEY},
    protocols::{
        ChiaMessage, NodeType, ProtocolMessageTypes,
        full_node::{NewPeak, RespondPeers},
        shared::Handshake,
    },
    ssl::{generate_ca_signed_cert_data, load_certs_from_bytes, load_private_key_from_bytes},
};
use dg_xch_crawler::{
    config::admissible,
    metrics,
    probe::{Identity, probe as probe_peer},
};
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use futures_util::{SinkExt, StreamExt};
use std::{io::Cursor, net::SocketAddr, sync::Arc, time::Duration};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::tungstenite::Message;

async fn probe(
    endpoint: SocketAddr,
    network: &str,
    identity: &Identity,
    deadline: Duration,
) -> dg_xch_servers::discovery::Observation {
    probe_peer(
        endpoint,
        network,
        identity,
        deadline,
        &dg_xch_servers::discovery::CrawlerPolicy {
            port: endpoint.port(),
            minimum_height: 100000,
            peak_wait_ms: 500,
            tcp_connect_timeout_seconds: 2,
            enable_ipv6: true,
        },
    )
    .await
}
fn hello(network: &str) -> Handshake {
    Handshake {
        network_id: network.into(),
        protocol_version: "0.0.37".into(),
        software_version: "test-node/1".into(),
        server_port: 8444,
        node_type: NodeType::FullNode as u8,
        capabilities: vec![(1, "1".into())],
    }
}
fn wire<T: ChiaSerialize>(kind: ProtocolMessageTypes, body: &T, id: Option<u16>) -> Message {
    let version = ChiaProtocolVersion::default();
    Message::Binary(
        ChiaMessage::new(kind, version, body, id)
            .unwrap()
            .to_bytes(version)
            .unwrap()
            .into(),
    )
}
async fn mock(
    network: &str,
    respond: bool,
    introducer: bool,
) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    mock_on(network, respond, introducer, "127.0.0.1:0").await
}
async fn mock_on(
    network: &str,
    respond: bool,
    introducer: bool,
    listen: &str,
) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    mock_behavior(network, respond, introducer, listen, Some(123456), false).await
}
async fn mock_behavior(
    network: &str,
    respond: bool,
    introducer: bool,
    listen: &str,
    height: Option<u32>,
    delayed: bool,
) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    mock_delayed_peak(
        network,
        respond,
        introducer,
        listen,
        height,
        if delayed {
            Duration::from_millis(100)
        } else {
            Duration::ZERO
        },
    )
    .await
}
async fn mock_delayed_peak(
    network: &str,
    respond: bool,
    introducer: bool,
    listen: &str,
    height: Option<u32>,
    delay: Duration,
) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let (cert, key) =
        generate_ca_signed_cert_data(CHIA_CA_CRT.as_bytes(), CHIA_CA_KEY.as_bytes()).unwrap();
    let tls = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(
        load_certs_from_bytes(&cert).unwrap(),
        load_private_key_from_bytes(&key).unwrap(),
    )
    .unwrap();
    let listener = TcpListener::bind(listen).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let network = network.to_owned();
    let task = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let tls = TlsAcceptor::from(Arc::new(tls)).accept(tcp).await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(tls).await.unwrap();
        let Message::Binary(bytes) = ws.next().await.unwrap().unwrap() else {
            panic!("expected handshake")
        };
        let msg = ChiaMessage::from_bytes(&mut Cursor::new(&bytes), ChiaProtocolVersion::default())
            .unwrap();
        assert_eq!(msg.msg_type, ProtocolMessageTypes::Handshake);
        let local = Handshake::from_bytes(
            &mut Cursor::new(msg.data.as_slice()),
            ChiaProtocolVersion::default(),
        )
        .unwrap();
        assert_eq!(local.server_port, 0);
        let mut remote = hello(&network);
        if introducer {
            remote.node_type = NodeType::Introducer as u8;
        }
        ws.send(wire(ProtocolMessageTypes::Handshake, &remote, None))
            .await
            .unwrap();
        if network != "mainnet" {
            let _ = ws.next().await;
            return;
        }
        let Message::Binary(bytes) = ws.next().await.unwrap().unwrap() else {
            panic!("expected peer request")
        };
        let msg = ChiaMessage::from_bytes(&mut Cursor::new(&bytes), ChiaProtocolVersion::default())
            .unwrap();
        assert_eq!(
            msg.msg_type,
            if introducer {
                ProtocolMessageTypes::RequestPeersIntroducer
            } else {
                ProtocolMessageTypes::RequestPeers
            }
        );
        if !respond {
            if let Some(height) = height {
                let peak = NewPeak {
                    header_hash: Bytes32::default(),
                    height,
                    weight: 1,
                    fork_point_with_previous_peak: 0,
                    unfinished_reward_block_hash: Bytes32::default(),
                };
                ws.send(wire(ProtocolMessageTypes::NewPeak, &peak, None))
                    .await
                    .unwrap();
            }
            tokio::time::sleep(Duration::from_secs(4)).await;
            return;
        }
        let peak = NewPeak {
            header_hash: Bytes32::default(),
            height: height.unwrap_or(0),
            weight: 1,
            fork_point_with_previous_peak: 0,
            unfinished_reward_block_hash: Bytes32::default(),
        };
        if height.is_some() && delay.is_zero() {
            ws.send(wire(ProtocolMessageTypes::NewPeak, &peak, None))
                .await
                .unwrap();
        }
        let response = RespondPeers {
            peer_list: vec![TimestampedPeerInfo {
                host: "8.8.8.8".into(),
                port: 8444,
                timestamp: 77,
            }],
        };
        // Ignore an uncorrelated response before consuming the matching reply.
        ws.send(wire(
            ProtocolMessageTypes::RespondPeers,
            &RespondPeers { peer_list: vec![] },
            Some(2),
        ))
        .await
        .unwrap();
        // Both reply bodies have the same TimestampedPeerInfo list encoding.
        let response_kind = if introducer {
            ProtocolMessageTypes::RespondPeersIntroducer
        } else {
            ProtocolMessageTypes::RespondPeers
        };
        ws.send(wire(response_kind, &response, msg.id))
            .await
            .unwrap();
        if height.is_some() && !delay.is_zero() {
            tokio::time::sleep(delay).await;
            ws.send(wire(ProtocolMessageTypes::NewPeak, &peak, None))
                .await
                .unwrap();
        }
        let _ = ws.next().await;
    });
    (addr, task)
}
#[tokio::test]
async fn real_tls_handshake_correlates_gossip_and_records_certificate_and_peak() {
    let identity = Identity::generate().unwrap();
    let (addr, task) = mock("mainnet", true, false).await;
    let obs = probe(addr, "mainnet", &identity, Duration::from_secs(3)).await;
    assert!(obs.error.is_none(), "{:?}", obs.error);
    assert_eq!(obs.peers.len(), 1);
    assert_eq!(obs.node_id.as_ref().unwrap().len(), 64);
    assert_eq!(obs.peak_height, Some(123456));
    assert!(obs.peer_request_ms.is_some());
    task.await.unwrap();
}
#[tokio::test]
async fn timeout_preserves_handshake_but_wrong_network_does_not_count() {
    let identity = Identity::generate().unwrap();
    let (addr, task) = mock("mainnet", false, false).await;
    let obs = probe(addr, "mainnet", &identity, Duration::from_millis(500)).await;
    assert!(obs.handshake.is_some());
    assert_eq!(obs.error.as_deref(), Some("peer request timed out"));
    assert_eq!(obs.phase, dg_xch_crawler::probe::ProbePhase::PeerRequest);
    assert!(obs.peer_request_ms.is_none());
    task.abort();
    let (addr, task) = mock("testnet11", true, false).await;
    let obs = probe(addr, "mainnet", &identity, Duration::from_secs(3)).await;
    assert!(obs.handshake.is_none());
    assert!(obs.error.unwrap().contains("wrong network"));
    task.await.unwrap();
}

#[tokio::test]
async fn introducer_uses_its_separate_peer_discovery_protocol() {
    let identity = Identity::generate().unwrap();
    let (addr, task) = mock("mainnet", true, true).await;
    let obs = probe(addr, "mainnet", &identity, Duration::from_secs(3)).await;
    assert!(obs.error.is_none(), "{:?}", obs.error);
    assert_eq!(obs.handshake.unwrap().node_type, NodeType::Introducer as u8);
    assert_eq!(obs.peers.len(), 1);
    task.await.unwrap();
}

#[tokio::test]
async fn ipv6_tls_probe_completes_handshake_and_peer_request() {
    let identity = Identity::generate().unwrap();
    let (addr, task) = mock_on("mainnet", true, false, "[::1]:0").await;
    assert!(addr.is_ipv6());
    let obs = probe(addr, "mainnet", &identity, Duration::from_secs(3)).await;
    assert!(obs.error.is_none(), "{:?}", obs.error);
    assert!(obs.handshake.is_some());
    assert!(obs.peer_request_ms.is_some());
    assert_eq!(obs.node_id.as_ref().unwrap().len(), 64);
    task.await.unwrap();
}

#[tokio::test]
async fn disabling_ipv6_prevents_connections_while_ipv4_still_works() {
    let identity = Identity::generate().unwrap();
    let listener = TcpListener::bind("[::1]:0").await.unwrap();
    let endpoint = listener.local_addr().unwrap();
    let mut policy = dg_xch_servers::discovery::CrawlerPolicy {
        port: endpoint.port(),
        enable_ipv6: false,
        minimum_height: 100000,
        peak_wait_ms: 500,
        tcp_connect_timeout_seconds: 2,
    };
    let observation = probe_peer(
        endpoint,
        "mainnet",
        &identity,
        Duration::from_secs(3),
        &policy,
    )
    .await;
    assert_eq!(
        observation.error.as_deref(),
        Some("IPv6 probing is disabled")
    );
    assert_eq!(
        observation.phase,
        dg_xch_crawler::probe::ProbePhase::Unknown
    );
    assert!(observation.handshake.is_none());
    assert!(
        tokio::time::timeout(Duration::from_millis(100), listener.accept())
            .await
            .is_err()
    );

    let (endpoint, task) = mock("mainnet", true, false).await;
    policy.port = endpoint.port();
    let observation = probe_peer(
        endpoint,
        "mainnet",
        &identity,
        Duration::from_secs(3),
        &policy,
    )
    .await;
    assert!(observation.error.is_none(), "{observation:?}");
    assert!(observation.handshake.is_some());
    task.await.unwrap();
}

#[test]
fn ipv6_configuration_defaults_on_and_preserves_explicit_false() {
    use dg_xch_crawler::config::Config;
    let old: Config = serde_json::from_str(r#"{"database_url":"postgres://fixture"}"#).unwrap();
    assert!(old.enable_ipv6);
    assert!(old.crawler_policy().unwrap().enable_ipv6);
    let disabled: Config =
        serde_json::from_str(r#"{"database_url":"postgres://fixture","enable_ipv6":false}"#)
            .unwrap();
    disabled.validate().unwrap();
    assert!(!disabled.crawler_policy().unwrap().enable_ipv6);
    let encoded = serde_json::to_value(&disabled).unwrap();
    assert_eq!(encoded["enable_ipv6"], false);
    assert!(serde_json::from_str::<Config>(r#"{"enable_ipv6":"false"}"#).is_err());
}

#[test]
fn address_policy_and_metric_labels_are_bounded() {
    for host in [
        "127.0.0.1:8444",
        "10.0.0.1:8444",
        "[::1]:8444",
        "[::ffff:8.8.8.8]:8444",
        "8.8.8.8:22",
        "224.0.0.1:8444",
    ] {
        assert!(!admissible(host.parse().unwrap(), false));
    }
    assert!(admissible("[2606:4700::1111]:8444".parse().unwrap(), false));
    let counts = (0..100).map(|n| (format!("version{n}"), n + 1)).collect();
    let bounded = metrics::bounded_counts(&counts, 32);
    assert_eq!(bounded.len(), 33);
    assert_eq!(bounded.iter().map(|(_, n)| n).sum::<usize>(), 5050);
}

#[test]
fn failure_reasons_keep_routing_distinct_from_remote_refusals() {
    use dg_xch_servers::discovery::{Phase, failure_reason};
    for (phase, message, expected) in [
        (
            Phase::TcpConnect,
            "Network is unreachable (os error 101)",
            "routing",
        ),
        (
            Phase::TcpConnect,
            "No route to host (os error 113)",
            "routing",
        ),
        (
            Phase::TcpConnect,
            "Connection refused (os error 111)",
            "refused",
        ),
        (Phase::TcpConnect, "probe timed out", "timeout"),
        (
            Phase::TcpConnect,
            "Operation not permitted (os error 1)",
            "permission",
        ),
        (
            Phase::TcpConnect,
            "Cannot assign requested address (os error 99)",
            "local_address",
        ),
        (
            Phase::Handshake,
            "peer handshake has wrong network or unsupported node type",
            "handshake",
        ),
        (Phase::PeerRequest, "peer closed connection", "closed"),
        (Phase::Tls, "invalid certificate", "protocol"),
    ] {
        assert_eq!(failure_reason(phase, message), expected);
    }
}

#[tokio::test]
async fn peak_after_peer_response_is_observed_and_missing_or_low_peaks_fail() {
    let identity = Identity::generate().unwrap();
    for (height, delayed, expected) in [
        (Some(123456), true, true),
        (None, false, false),
        (Some(42), false, false),
    ] {
        let (addr, task) =
            mock_behavior("mainnet", true, false, "127.0.0.1:0", height, delayed).await;
        let obs = probe(addr, "mainnet", &identity, Duration::from_secs(3)).await;
        assert_eq!(obs.error.is_none(), expected, "{obs:?}");
        assert!(obs.handshake.is_some());
        assert!(obs.peer_request_ms.is_some());
        assert_eq!(obs.peak_height, height);
        if !expected {
            assert_eq!(obs.phase, dg_xch_crawler::probe::ProbePhase::Verification);
        }
        task.await.unwrap();
    }
}
#[tokio::test]
async fn wrong_port_is_rejected_before_any_connection() {
    let identity = Identity::generate().unwrap();
    let obs = probe_peer(
        "127.0.0.1:8445".parse().unwrap(),
        "mainnet",
        &identity,
        Duration::from_secs(1),
        &dg_xch_servers::discovery::CrawlerPolicy {
            port: 8444,
            minimum_height: 240000,
            peak_wait_ms: 2500,
            tcp_connect_timeout_seconds: 2,
            enable_ipv6: true,
        },
    )
    .await;
    assert!(obs.handshake.is_none());
    assert_eq!(obs.phase, dg_xch_crawler::probe::ProbePhase::Unknown);
    assert!(obs.error.unwrap().contains("port"));
}
#[test]
fn network_defaults_and_seed_ports_are_validated() {
    use dg_xch_crawler::config::Config;
    let mut c = Config {
        database_url: Some("postgres://fixture".into()),
        ..Config::default()
    };
    assert_eq!(c.crawler_policy().unwrap().port, 8444);
    assert_eq!(c.concurrency, 250);
    assert_eq!(c.recent_seconds, 5 * 86400);
    c.peer_port = Some(8445);
    assert!(c.validate().is_err());
    c.peer_port = None;
    c.seed = vec!["node.chia.net:8445".into()];
    assert!(c.validate().is_err());
    c.network = "testnet11".into();
    c.seed = vec!["testnet11-node.chia.net:58444".into()];
    assert!(c.validate().is_ok());
    assert_eq!(c.crawler_policy().unwrap().port, 58444);
    c.network = "custom".into();
    assert!(c.validate().is_err());
    c.peer_port = Some(58444);
    assert!(c.validate().is_ok());
}

#[tokio::test]
async fn peak_is_preserved_when_peer_request_times_out() {
    let identity = Identity::generate().unwrap();
    let (addr, task) = mock("mainnet", false, false).await;
    let obs = probe(addr, "mainnet", &identity, Duration::from_secs(5)).await;
    assert!(obs.handshake.is_some());
    assert_eq!(obs.peak_height, Some(123456));
    assert!(obs.peer_request_ms.is_none());
    assert_eq!(obs.error.as_deref(), Some("peer request timed out"));
    assert_eq!(obs.phase, dg_xch_crawler::probe::ProbePhase::PeerRequest);
    task.abort();
}

#[test]
fn tcp_timeout_configuration_defaults_round_trips_and_rejects_invalid_limits() {
    use dg_xch_crawler::config::Config;
    let mut config = Config {
        database_url: Some("postgres://fixture".into()),
        ..Config::default()
    };
    assert_eq!(config.tcp_connect_timeout_seconds, 2);
    for timeout in [1, 10, 60] {
        config.tcp_connect_timeout_seconds = timeout;
        config.validate().unwrap();
        let round_trip: Config =
            serde_json::from_str(&serde_json::to_string(&config).unwrap()).unwrap();
        assert_eq!(
            round_trip
                .crawler_policy()
                .unwrap()
                .tcp_connect_timeout_seconds,
            timeout
        );
    }
    for timeout in [0, 61] {
        config.tcp_connect_timeout_seconds = timeout;
        assert!(config.validate().is_err());
    }
    let old: Config = serde_json::from_str(r#"{"database_url":"postgres://fixture"}"#).unwrap();
    assert_eq!(old.tcp_connect_timeout_seconds, 2);
}

#[tokio::test]
async fn delayed_peak_outlives_connection_deadline_and_records_last_known_height() {
    let identity = Identity::generate().unwrap();
    let (addr, task) = mock_delayed_peak(
        "mainnet",
        true,
        false,
        "127.0.0.1:0",
        Some(123456),
        Duration::from_secs(3),
    )
    .await;
    let obs = probe_peer(
        addr,
        "mainnet",
        &identity,
        Duration::from_secs(2),
        &dg_xch_servers::discovery::CrawlerPolicy {
            port: addr.port(),
            minimum_height: 100000,
            peak_wait_ms: 4000,
            tcp_connect_timeout_seconds: 2,
            enable_ipv6: true,
        },
    )
    .await;
    assert!(obs.error.is_none(), "{obs:?}");
    assert_eq!(obs.peak_height, Some(123456));
    assert!(obs.peer_request_ms.is_some());
    task.await.unwrap();
}
#[test]
fn new_peak_wait_defaults_to_a_minute_and_has_its_own_bounds() {
    use dg_xch_crawler::config::Config;
    let mut config = Config {
        database_url: Some("postgres://fixture".into()),
        ..Config::default()
    };
    assert_eq!(config.peak_wait_ms, 60000);
    assert_eq!(config.timeout_seconds, 15);
    config.validate().unwrap();
    config.peak_wait_ms = 120000;
    config.validate().unwrap();
    config.peak_wait_ms = 120001;
    assert!(config.validate().is_err());
}
