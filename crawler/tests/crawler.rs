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
    probe::{Identity, probe},
};
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use futures_util::{SinkExt, StreamExt};
use std::{io::Cursor, net::SocketAddr, sync::Arc, time::Duration};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::tungstenite::Message;

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
            tokio::time::sleep(Duration::from_secs(2)).await;
            return;
        }
        let peak = NewPeak {
            header_hash: Bytes32::default(),
            height: 123456,
            weight: 1,
            fork_point_with_previous_peak: 0,
            unfinished_reward_block_hash: Bytes32::default(),
        };
        ws.send(wire(ProtocolMessageTypes::NewPeak, &peak, None))
            .await
            .unwrap();
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
    assert_eq!(obs.error.as_deref(), Some("probe timed out"));
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
