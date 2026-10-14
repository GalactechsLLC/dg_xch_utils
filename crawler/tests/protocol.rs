use dg_xch_core::{
    blockchain::sized_bytes::Bytes32,
    constants::{CHIA_CA_CRT, CHIA_CA_KEY},
    protocols::{
        ChiaMessage, NodeType, ProtocolMessageTypes,
        full_node::{NewPeak, RespondPeers},
        shared::{ErrorMessage, Handshake},
    },
    ssl::{
        generate_ca_signed_cert_data, load_certs_from_bytes, load_private_key_from_bytes,
        make_ca_cert_data,
    },
};
use dg_xch_crawler::probe::{Identity, probe};
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use dg_xch_servers::discovery::{CrawlerPolicy, Phase};
use futures_util::{SinkExt, StreamExt};
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::tungstenite::Message;

fn wire<T: ChiaSerialize>(kind: ProtocolMessageTypes, body: &T, id: Option<u16>) -> Message {
    let version = ChiaProtocolVersion::Chia0_0_34;
    Message::Binary(
        ChiaMessage::new(kind, version, body, id)
            .unwrap()
            .to_bytes(version)
            .unwrap()
            .into(),
    )
}
fn hello(version: &str, introducer: bool) -> Handshake {
    Handshake {
        network_id: "mainnet".into(),
        protocol_version: version.into(),
        software_version: "CNI-test-peer".into(),
        server_port: 8444,
        node_type: if introducer {
            NodeType::Introducer
        } else {
            NodeType::FullNode
        } as u8,
        capabilities: if matches!(version, "0.0.32" | "0.0.33") {
            vec![(1, "1".into())]
        } else {
            vec![
                (1, "1".into()),
                (3, "1".into()),
                (7, "1".into()),
                (60000, "1".into()),
            ]
        },
    }
}
fn peak(height: u32) -> Message {
    wire(
        ProtocolMessageTypes::NewPeak,
        &NewPeak {
            header_hash: Bytes32::default(),
            height,
            weight: 1,
            fork_point_with_previous_peak: 0,
            unfinished_reward_block_hash: Bytes32::default(),
        },
        None,
    )
}
async fn mock(
    hello: Handshake,
    cert: &[u8],
    key: &[u8],
    tls12: bool,
    pre_handshake: bool,
    replies: Vec<Message>,
) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    mock_with_delayed_peak(hello, cert, key, tls12, pre_handshake, replies, None).await
}
async fn mock_with_delayed_peak(
    hello: Handshake,
    cert: &[u8],
    key: &[u8],
    tls12: bool,
    pre_handshake: bool,
    replies: Vec<Message>,
    delayed_height: Option<u32>,
) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let mut roots = rustls::RootCertStore::empty();
    let ca = if tls12 {
        CHIA_CA_CRT
    } else {
        include_str!("../src/chia_ca.crt")
    };
    for cert in load_certs_from_bytes(ca.as_bytes()).unwrap() {
        roots.add(cert).unwrap();
    }
    let client_verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(
        Arc::new(roots),
        provider.clone(),
    )
    .build()
    .unwrap();
    let versions = [if tls12 {
        &rustls::version::TLS12
    } else {
        &rustls::version::TLS13
    }];
    let tls = rustls::ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&versions)
        .unwrap()
        .with_client_cert_verifier(client_verifier)
        .with_single_cert(
            load_certs_from_bytes(cert).unwrap(),
            load_private_key_from_bytes(key).unwrap(),
        )
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let Ok(tls) = TlsAcceptor::from(Arc::new(tls)).accept(tcp).await else {
            return;
        };
        assert!(!tls.get_ref().1.peer_certificates().unwrap().is_empty());
        let Ok(mut ws) = tokio_tungstenite::accept_async(tls).await else {
            return;
        };
        let Some(Ok(Message::Binary(bytes))) = ws.next().await else {
            return;
        };
        let local = ChiaMessage::from_bytes_exact(&bytes, ChiaProtocolVersion::Chia0_0_34).unwrap();
        let local =
            Handshake::from_bytes_exact(local.data.as_slice(), ChiaProtocolVersion::Chia0_0_34)
                .unwrap();
        assert_eq!(local.protocol_version, "0.0.36");
        assert_eq!(
            local.capabilities,
            vec![(1, "1".into()), (3, "1".into()), (6, "1".into())]
        );
        if pre_handshake {
            let _ = ws.send(peak(300000)).await;
            return;
        }
        ws.send(wire(ProtocolMessageTypes::Handshake, &hello, None))
            .await
            .unwrap();
        let Some(Ok(Message::Binary(bytes))) = ws.next().await else {
            return;
        };
        let request =
            ChiaMessage::from_bytes_exact(&bytes, ChiaProtocolVersion::Chia0_0_34).unwrap();
        assert_eq!(
            request.msg_type,
            if hello.node_type == 5 {
                ProtocolMessageTypes::RequestPeersIntroducer
            } else {
                ProtocolMessageTypes::RequestPeers
            }
        );
        assert_eq!(request.id, Some(1));
        for message in replies {
            if ws.send(message).await.is_err() {
                return;
            }
        }
        if let Some(height) = delayed_height {
            tokio::time::sleep(Duration::from_millis(100)).await;
            if ws.send(peak(height)).await.is_err() {
                return;
            }
        }
        let _ = ws.next().await;
    });
    (endpoint, task)
}
async fn run(identity: &Identity, endpoint: SocketAddr) -> dg_xch_servers::discovery::Observation {
    probe(
        endpoint,
        "mainnet",
        identity,
        Duration::from_secs(5),
        &CrawlerPolicy {
            port: endpoint.port(),
            minimum_height: 240000,
            peak_wait_ms: 500,
            tcp_connect_timeout_seconds: 2,
            enable_ipv6: true,
        },
    )
    .await
}
#[tokio::test]
async fn historical_and_newer_protocols_work_with_mutual_tls_and_v2_fallback() {
    let identity = Identity::generate().unwrap();
    let (cert, key) =
        generate_ca_signed_cert_data(CHIA_CA_CRT.as_bytes(), CHIA_CA_KEY.as_bytes()).unwrap();
    for version in [
        "0.0.32", "0.0.33", "0.0.34", "0.0.35", "0.0.36", "0.0.37", "0.0.38",
    ] {
        for introducer in [false, true] {
            let response = wire(
                if introducer {
                    ProtocolMessageTypes::RespondPeersIntroducer
                } else {
                    ProtocolMessageTypes::RespondPeers
                },
                &RespondPeers { peer_list: vec![] },
                Some(1),
            );
            let (endpoint, task) = mock(
                hello(version, introducer),
                &cert,
                &key,
                matches!(version, "0.0.32" | "0.0.33"),
                false,
                vec![peak(300000), peak(299999), response],
            )
            .await;
            let obs = run(&identity, endpoint).await;
            assert!(obs.error.is_none(), "{version}: {obs:?}");
            assert_eq!(
                obs.tls_version.as_deref(),
                Some(if matches!(version, "0.0.32" | "0.0.33") {
                    "TLSv1.2"
                } else {
                    "TLSv1.3"
                })
            );
            assert_eq!(obs.handshake.unwrap().protocol_version, version);
            assert_eq!(obs.peak_height, Some(299999));
            task.await.unwrap();
        }
    }
}
#[tokio::test]
async fn a_low_peak_does_not_end_the_wait_for_a_qualifying_peak() {
    let identity = Identity::generate().unwrap();
    let (cert, key) =
        generate_ca_signed_cert_data(CHIA_CA_CRT.as_bytes(), CHIA_CA_KEY.as_bytes()).unwrap();
    for before_response in [true, false] {
        let response = wire(
            ProtocolMessageTypes::RespondPeers,
            &RespondPeers { peer_list: vec![] },
            Some(1),
        );
        let replies = if before_response {
            vec![peak(239999), response]
        } else {
            vec![response, peak(239999)]
        };
        let (endpoint, task) = mock_with_delayed_peak(
            hello("0.0.36", false),
            &cert,
            &key,
            false,
            false,
            replies,
            Some(240000),
        )
        .await;
        let obs = run(&identity, endpoint).await;
        assert!(obs.error.is_none(), "{obs:?}");
        assert_eq!(obs.peak_height, Some(240000));
        assert!(obs.peer_request_ms.is_some());
        task.await.unwrap();
    }
}
#[tokio::test]
async fn correlated_errors_and_none_responses_keep_their_real_reason() {
    let identity = Identity::generate().unwrap();
    let (cert, key) =
        generate_ca_signed_cert_data(CHIA_CA_CRT.as_bytes(), CHIA_CA_KEY.as_bytes()).unwrap();
    for (reply, expected) in [
        (
            wire(
                ProtocolMessageTypes::Error,
                &ErrorMessage {
                    code: -4,
                    message: "not ready".into(),
                    data: None,
                },
                Some(1),
            ),
            "peer protocol error -4: not ready",
        ),
        (
            Message::Binary(vec![91, 1, 0, 1, 0, 0, 0, 0].into()),
            "peer returned no response",
        ),
    ] {
        let (endpoint, task) = mock(
            hello("0.0.36", false),
            &cert,
            &key,
            false,
            false,
            vec![peak(300000), reply],
        )
        .await;
        let obs = run(&identity, endpoint).await;
        assert_eq!(obs.error.as_deref(), Some(expected));
        assert_eq!(obs.phase, Phase::PeerRequest);
        assert_eq!(obs.peak_height, Some(300000));
        assert!(obs.peer_request_ms.is_none());
        task.await.unwrap();
    }
}
#[tokio::test]
async fn unrelated_errors_and_unknown_extensions_do_not_hide_a_valid_reply() {
    let identity = Identity::generate().unwrap();
    let (cert, key) =
        generate_ca_signed_cert_data(CHIA_CA_CRT.as_bytes(), CHIA_CA_KEY.as_bytes()).unwrap();
    let replies = vec![
        Message::Binary(vec![240, 0, 0, 0, 0, 1, 42].into()),
        wire(
            ProtocolMessageTypes::Error,
            &ErrorMessage {
                code: 1,
                message: "another request".into(),
                data: None,
            },
            Some(2),
        ),
        peak(300000),
        wire(
            ProtocolMessageTypes::RespondPeers,
            &RespondPeers { peer_list: vec![] },
            Some(1),
        ),
    ];
    let (endpoint, task) = mock(hello("0.0.36", false), &cert, &key, false, false, replies).await;
    let obs = run(&identity, endpoint).await;
    assert!(obs.error.is_none(), "{obs:?}");
    task.await.unwrap();
}
#[tokio::test]
async fn unauthenticated_and_invalid_handshakes_never_count_as_reachable() {
    let identity = Identity::generate().unwrap();
    let (cert, key) =
        generate_ca_signed_cert_data(CHIA_CA_CRT.as_bytes(), CHIA_CA_KEY.as_bytes()).unwrap();
    for case in 0..4 {
        let mut hello = hello("0.0.36", false);
        if case == 1 {
            hello.protocol_version = "bad-version".into();
        }
        if case == 2 {
            hello.capabilities = vec![(1, "0".into())];
        }
        if case == 3 {
            hello.software_version = "a".repeat(129);
        }
        let (endpoint, task) = mock(hello, &cert, &key, false, case == 0, vec![]).await;
        let obs = run(&identity, endpoint).await;
        assert!(obs.handshake.is_none(), "{obs:?}");
        assert!(obs.peak_height.is_none());
        assert_eq!(obs.phase, Phase::Handshake);
        assert!(obs.error.is_some());
        task.await.unwrap();
    }
    let (ca, ca_key) = make_ca_cert_data().unwrap();
    let (cert, key) = generate_ca_signed_cert_data(&ca, &ca_key).unwrap();
    let (endpoint, task) = mock(hello("0.0.36", false), &cert, &key, false, false, vec![]).await;
    let obs = run(&identity, endpoint).await;
    assert_eq!(obs.phase, Phase::Tls);
    assert!(obs.handshake.is_none());
    assert!(obs.error.is_some());
    task.await.unwrap();
}
