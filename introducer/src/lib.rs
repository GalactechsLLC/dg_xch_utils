#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::todo,
        clippy::unimplemented
    )
)]
mod dashboard;
mod metrics;
use dg_xch_core::blockchain::peer_info::TimestampedPeerInfo;
use dg_xch_core::consensus::chain_definition::ChainSelection;
use dg_xch_core::protocols::introducer::{RequestPeersIntroducer, RespondPeersIntroducer};
use dg_xch_core::protocols::shared::Handshake;
use dg_xch_core::protocols::{ChiaMessage, NodeType, ProtocolMessageTypes};
use dg_xch_servers::discovery::{Observation, Phase, Policy, Role, Store};
use dg_xch_servers::transport::{
    SERVICE_MESSAGE_LIMIT, TlsIdentity, decode_exact, encode, handshake, shutdown_signal,
};
use futures_util::{SinkExt, StreamExt};
use metrics::Metrics;
use rustls::pki_types::ServerName;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::io::{Error, ErrorKind};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinSet;
use tokio_rustls::{TlsAcceptor, TlsConnector};
use tokio_tungstenite::tungstenite::{Message, protocol::WebSocketConfig};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub listen: SocketAddr,
    #[serde(default)]
    pub database_url: Option<String>,
    /// Plain HTTP Prometheus listener; null disables it.
    #[serde(default = "default_metrics_listen")]
    pub metrics_listen: Option<SocketAddr>,
    #[serde(default)]
    pub chain: ChainSelection,
    pub tls: TlsIdentity,
    pub peer_server_name: String,
    #[serde(default)]
    pub allow_private_addresses: bool,
    pub max_connections: usize,
    #[serde(default = "default_probe_concurrency")]
    pub probe_concurrency: usize,
    #[serde(default = "default_probe_interval")]
    pub probe_interval_ms: u64,
    pub peer_ttl_seconds: u64,
}

fn default_probe_concurrency() -> usize {
    4
}
fn default_probe_interval() -> u64 {
    2000
}
fn default_metrics_listen() -> Option<SocketAddr> {
    Some(SocketAddr::from(([0, 0, 0, 0], 8581)))
}

impl Config {
    pub fn validate(&self) -> Result<(), Error> {
        if self
            .metrics_listen
            .is_some_and(|listen| listen.port() == 0 || listen.port() == self.listen.port())
        {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "introducer metrics port must be nonzero and different from the peer port",
            ));
        }
        if self
            .database_url
            .as_deref()
            .is_none_or(|url| url.trim().is_empty())
        {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "introducer requires database_url or DGX_INTRODUCER_DATABASE_URL",
            ));
        }
        self.chain.constants().map_err(Error::other)?;
        ServerName::try_from(self.peer_server_name.clone()).map_err(Error::other)?;
        if self.listen.port() == 0
            || !(1..=256).contains(&self.max_connections)
            || !(60..=86_400).contains(&self.peer_ttl_seconds)
            || !(1..=32).contains(&self.probe_concurrency)
            || !(500..=60_000).contains(&self.probe_interval_ms)
        {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "invalid introducer resource limits",
            ));
        }
        Ok(())
    }
}

use dg_xch_servers::discovery::admissible as admissible_address;

fn websocket_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .max_message_size(Some(SERVICE_MESSAGE_LIMIT))
        .max_frame_size(Some(SERVICE_MESSAGE_LIMIT))
}

async fn read_message<Stream: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(
    socket: &mut tokio_tungstenite::WebSocketStream<Stream>,
) -> Result<ChiaMessage, Error> {
    match socket.next().await {
        Some(Ok(Message::Binary(bytes))) => decode_exact(&bytes),
        _ => Err(Error::new(
            ErrorKind::InvalidData,
            "expected binary Chia protocol message",
        )),
    }
}

async fn probe(
    endpoint: SocketAddr,
    config: &Config,
    tls: Arc<rustls::ClientConfig>,
    network: &str,
) -> Observation {
    let mut obs = Observation::default();
    let result = tokio::time::timeout(Duration::from_secs(3), async {
        let started = Instant::now();
        obs.phase = Phase::TcpConnect;
        let stream = TcpStream::connect(endpoint).await?;
        let name = ServerName::try_from(config.peer_server_name.clone()).map_err(Error::other)?;
        obs.phase = Phase::Tls;
        let stream = TlsConnector::from(tls).connect(name, stream).await?;
        obs.node_id = stream
            .get_ref()
            .1
            .peer_certificates()
            .and_then(|v| v.first())
            .map(|cert| {
                dg_xch_core::utils::hash_256(cert.as_ref())
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect()
            });
        obs.phase = Phase::Websocket;
        let (mut socket, _) = tokio_tungstenite::client_async_with_config(
            format!("wss://{endpoint}/ws"),
            stream,
            Some(websocket_config()),
        )
        .await
        .map_err(Error::other)?;
        obs.phase = Phase::Handshake;
        socket
            .send(encode(
                ProtocolMessageTypes::Handshake,
                &handshake(network, NodeType::FullNode, 0),
                None,
            )?)
            .await
            .map_err(Error::other)?;
        let message = read_message(&mut socket).await?;
        let greeting: Handshake = decode_exact(message.data.as_slice())?;
        if message.msg_type != ProtocolMessageTypes::Handshake
            || greeting.network_id != network
            || greeting.node_type != NodeType::FullNode as u8
        {
            return Err(Error::other("endpoint is not a full node on this network"));
        }
        obs.connect_ms = Some(started.elapsed().as_secs_f64() * 1000.0);
        obs.handshake = Some(greeting);
        Ok::<_, Error>(())
    })
    .await;
    obs.error = match result {
        Ok(Ok(())) => None,
        Ok(Err(error)) => Some(error.to_string()),
        Err(_) => Some("verification timed out".into()),
    };
    obs
}

async fn session(
    stream: TcpStream,
    remote: SocketAddr,
    config: Arc<Config>,
    acceptor: TlsAcceptor,
    store: Store,
    metrics: Arc<Metrics>,
) -> Result<(), Error> {
    let stream = acceptor.accept(stream).await?;
    let mut socket = tokio_tungstenite::accept_async_with_config(stream, Some(websocket_config()))
        .await
        .map_err(Error::other)?;
    let message = read_message(&mut socket).await?;
    if message.msg_type != ProtocolMessageTypes::Handshake {
        return Err(Error::new(ErrorKind::InvalidData, "handshake required"));
    }
    let greeting: Handshake = decode_exact(message.data.as_slice())?;
    let network = config.chain.handshake_network_id().map_err(Error::other)?;
    if greeting.network_id != network || greeting.node_type != NodeType::FullNode as u8 {
        return Err(Error::new(
            ErrorKind::PermissionDenied,
            "wrong network or peer role",
        ));
    }
    metrics.handshakes.inc();
    socket
        .send(encode(
            ProtocolMessageTypes::Handshake,
            &handshake(&network, NodeType::Introducer, config.listen.port()),
            None,
        )?)
        .await
        .map_err(Error::other)?;
    let request = read_message(&mut socket).await?;
    if request.msg_type != ProtocolMessageTypes::RequestPeersIntroducer {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "introducer peer request required",
        ));
    }
    decode_exact::<RequestPeersIntroducer>(request.data.as_slice())?;
    metrics.requests.inc();
    let endpoint = SocketAddr::new(remote.ip(), greeting.server_port);
    if admissible_address(endpoint, config.allow_private_addresses) {
        store.enqueue_verification(endpoint).await?;
    } else {
        metrics
            .skipped_probes
            .with_label_values(&["invalid_address"])
            .inc();
    }
    // Serve the existing verified set immediately; verification and retries run independently.
    let peer_list = store
        .response(endpoint, config.allow_private_addresses)
        .await?
        .into_iter()
        .map(|(peer, timestamp)| TimestampedPeerInfo {
            host: peer.ip().to_string(),
            port: peer.port(),
            timestamp,
        })
        .collect::<Vec<_>>();
    let response_size = peer_list.len();
    socket
        .send(encode(
            ProtocolMessageTypes::RespondPeersIntroducer,
            &RespondPeersIntroducer { peer_list },
            request.id,
        )?)
        .await
        .map_err(Error::other)?;
    metrics.responses.inc();
    metrics.peers_sent.inc_by(response_size as u64);
    metrics.response_size.observe(response_size as f64);
    socket.close(None).await.map_err(Error::other)
}

pub async fn serve(config: Config) -> Result<(), Error> {
    config.validate()?;
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let acceptor = TlsAcceptor::from(config.tls.public_server()?);
    let client_tls = config.tls.client()?;
    let listener = TcpListener::bind(config.listen).await?;
    let config = Arc::new(config);
    let url = config
        .database_url
        .as_deref()
        .ok_or_else(|| Error::other("introducer requires PostgreSQL"))?;
    let store = Store::connect(
        url,
        config.chain.handshake_network_id().map_err(Error::other)?,
    )
    .await?;
    store
        .configure(
            Role::Introducer,
            Policy {
                crawler: None,
                concurrency: config.probe_concurrency,
                interval_ms: config.probe_interval_ms,
                timeout_seconds: 3,
                revisit_seconds: config.peer_ttl_seconds / 2,
                cooldown_seconds: 60,
                allow_private: config.allow_private_addresses,
            },
        )
        .await?;
    let metrics = Metrics::new(&config)?;
    eprintln!(
        "Introducer listening on {} for {}",
        config.listen,
        config.chain.resolve().map_err(Error::other)?.network_id
    );
    let metrics_server = metrics::server(config.clone(), metrics.clone(), store.clone());
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let mut peers = Box::pin(serve_peers(
        listener,
        config.clone(),
        acceptor,
        client_tls,
        store,
        metrics,
        stop_rx,
    ));
    if let Some(server) = metrics_server {
        if let Some(listen) = config.metrics_listen {
            eprintln!("Introducer metrics: http://{listen}/metrics");
        }
        let handle = server.handle();
        let mut task = tokio::spawn(server.run());
        tokio::select! {
            result = &mut peers => {
                handle.shutdown();
                let metrics_result = task.await.map_err(Error::other)?.map_err(|e| Error::other(e.to_string()));
                result.and(metrics_result)
            },
            result = &mut task => {
                let _ = stop_tx.send(true);
                peers.await?;
                result.map_err(Error::other)?.map_err(|e| Error::other(e.to_string()))
            },
        }
    } else {
        peers.await
    }
}

async fn serve_peers(
    listener: TcpListener,
    config: Arc<Config>,
    acceptor: TlsAcceptor,
    client_tls: Arc<rustls::ClientConfig>,
    store: Store,
    metrics: Arc<Metrics>,
    mut stop: tokio::sync::watch::Receiver<bool>,
) -> Result<(), Error> {
    let mut sessions = JoinSet::new();
    let mut active = HashSet::new();
    let verification = verify_loop(
        config.clone(),
        client_tls,
        store.clone(),
        metrics.clone(),
        stop.clone(),
    );
    tokio::pin!(verification);
    let signal = shutdown_signal();
    tokio::pin!(signal);
    let result = loop {
        tokio::select! {
            result = &mut signal => break result,
            _ = stop.changed() => break Ok(()),
            _ = &mut verification => break Err(Error::other("introducer verification worker stopped")),
            Some(completed) = sessions.join_next(), if !sessions.is_empty() => {
                match completed {
                    Ok(address) => { active.remove(&address); }
                    Err(error) => break Err(Error::other(format!("introducer connection task failed: {error}"))),
                }
            }
            accepted = listener.accept() => {
                let (stream, remote) = match accepted { Ok(accepted) => accepted, Err(error) => break Err(error) };
                metrics.connections.inc();
                if sessions.len() >= config.max_connections {
                    metrics.rejected_connections.with_label_values(&["capacity"]).inc(); continue;
                }
                if !active.insert(remote.ip()) {
                    metrics.rejected_connections.with_label_values(&["duplicate_ip"]).inc(); continue;
                }
                let config = config.clone(); let acceptor = acceptor.clone(); let store = store.clone(); let metrics = metrics.clone();
                let mut guard = metrics.session();
                sessions.spawn(async move {
                    let result = tokio::time::timeout(Duration::from_secs(10), session(stream, remote, config, acceptor, store, metrics)).await;
                    guard.finish(match result { Ok(Ok(())) => "success", Ok(Err(_)) => "error", Err(_) => "timeout" });
                    remote.ip()
                });
            }
        }
    };
    sessions.abort_all();
    while sessions.join_next().await.is_some() {}
    result
}

async fn verify_loop(
    config: Arc<Config>,
    client_tls: Arc<rustls::ClientConfig>,
    store: Store,
    metrics: Arc<Metrics>,
    mut stop: tokio::sync::watch::Receiver<bool>,
) {
    let mut probes = JoinSet::new();
    let mut poll = tokio::time::interval(Duration::from_millis(config.probe_interval_ms));
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut maintenance = tokio::time::interval(Duration::from_secs(30));
    maintenance.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _=stop.changed()=>break,
            _=poll.tick()=>if probes.len()<config.probe_concurrency{
                match store.claim(Role::Introducer).await{
                    Ok(Some(claim))=>{let config=config.clone();let tls=client_tls.clone();let store=store.clone();let metrics=metrics.clone();probes.spawn(async move{
                        let mut guard=metrics.probe();let _timer=metrics.probe_duration.start_timer();
                        let obs=probe(claim.endpoint,&config,tls,&store.network).await;
                        let success=obs.error.is_none();guard.finish(if success{"success"}else if obs.error.as_deref()==Some("verification timed out"){"timeout"}else{"error"});
                        if let Some(error)=&obs.error{eprintln!("{}",serde_json::json!({"event":"introducer_probe","network":store.network,"endpoint":claim.endpoint,"phase":obs.phase,"error":error.chars().take(256).collect::<String>()}));}
                        match store.finish(claim,obs,Some(config.peer_ttl_seconds)).await {Ok(true) if success=>metrics.registrations.inc(),Ok(_)=>{},Err(error)=>eprintln!("introducer completion failed: {error}")}
                    });},Ok(None)=>{},Err(error)=>eprintln!("introducer claim failed: {error}")
                }
            },
            Some(result)=probes.join_next(),if !probes.is_empty()=>{if let Err(error)=result{eprintln!("introducer verification task failed: {error}");}},
            _=maintenance.tick()=>{match store.maintenance(Role::Introducer,90).await { Ok(expired)=>metrics.expirations.inc_by(expired),Err(error)=>eprintln!("introducer maintenance failed: {error}") }},
        }
    }
    while probes.join_next().await.is_some() {}
}

#[cfg(test)]
mod tests {
    use super::*;
    fn configuration(directory: &std::path::Path) -> Config {
        serde_json::from_value(serde_json::json!({"listen":"127.0.0.1:8445","metrics_listen":null,"database_url":"postgres://test","chain":"testnet11","tls":{"certificate":directory.join("cert.pem"),"private_key":directory.join("key.pem"),"ca_certificate":directory.join("ca.pem")},"peer_server_name":"chia.net","allow_private_addresses":true,"max_connections":4,"peer_ttl_seconds":60,"probe_concurrency":1,"probe_interval_ms":500})).unwrap()
    }
    fn identity(directory: &std::path::Path) {
        use dg_xch_core::{
            constants::{CHIA_CA_CRT, CHIA_CA_KEY},
            ssl::generate_ca_signed_cert_data,
        };
        let (cert, key) =
            generate_ca_signed_cert_data(CHIA_CA_CRT.as_bytes(), CHIA_CA_KEY.as_bytes()).unwrap();
        std::fs::write(directory.join("cert.pem"), cert).unwrap();
        std::fs::write(directory.join("key.pem"), key).unwrap();
        std::fs::write(directory.join("ca.pem"), CHIA_CA_CRT).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                directory.join("key.pem"),
                std::fs::Permissions::from_mode(0o600),
            )
            .unwrap();
        }
    }
    #[test]
    fn first_release_configuration_requires_postgres_and_rejects_ignored_limits() {
        let directory = tempfile::tempdir().unwrap();
        let mut config = configuration(directory.path());
        assert!(config.validate().is_ok());
        config.database_url = None;
        assert!(config.validate().is_err());
        let mut document = serde_json::to_value(configuration(directory.path())).unwrap();
        document["max_peers"] = serde_json::json!(100);
        assert!(serde_json::from_value::<Config>(document).is_err());
    }
    #[tokio::test]
    #[ignore = "requires isolated PostgreSQL via DGX_TEST_POSTGRES_URL"]
    async fn incoming_request_uses_verified_backend_and_correlates_response() {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let directory = tempfile::tempdir().unwrap();
        identity(directory.path());
        let config = Arc::new(configuration(directory.path()));
        let store = Store::connect(
            &std::env::var("DGX_TEST_POSTGRES_URL").unwrap(),
            "testnet11".into(),
        )
        .await
        .unwrap();
        let peer: SocketAddr = "127.254.0.2:8444".parse().unwrap();
        store.seen(peer, "fixture", None).await.unwrap();
        dg_xch_servers::discovery::sqlx::query("UPDATE dg_discovery.peers SET verified_at=floor(extract(epoch FROM clock_timestamp()))::bigint,verified_until=floor(extract(epoch FROM clock_timestamp()))::bigint+60 WHERE network=$1 AND ip=$2::inet AND port=$3").bind(&store.network).bind(peer.ip().to_string()).bind(i32::from(peer.port())).execute(&store.pool).await.unwrap();
        let metrics = Metrics::new(&config).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let acceptor = TlsAcceptor::from(config.tls.public_server().unwrap());
        let tls = config.tls.client().unwrap();
        let config_task = config.clone();
        let store_task = store.clone();
        let metrics_task = metrics.clone();
        let task = tokio::spawn(async move {
            let (tcp, remote) = listener.accept().await.unwrap();
            session(tcp, remote, config_task, acceptor, store_task, metrics_task)
                .await
                .unwrap();
        });
        let tcp = TcpStream::connect(addr).await.unwrap();
        let tls = TlsConnector::from(tls)
            .connect(ServerName::try_from("chia.net").unwrap(), tcp)
            .await
            .unwrap();
        let (mut socket, _) = tokio_tungstenite::client_async(format!("wss://{addr}/ws"), tls)
            .await
            .unwrap();
        socket
            .send(
                encode(
                    ProtocolMessageTypes::Handshake,
                    &handshake("testnet11", NodeType::FullNode, 0),
                    None,
                )
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            read_message(&mut socket).await.unwrap().msg_type,
            ProtocolMessageTypes::Handshake
        );
        socket
            .send(
                encode(
                    ProtocolMessageTypes::RequestPeersIntroducer,
                    &RequestPeersIntroducer {},
                    Some(7),
                )
                .unwrap(),
            )
            .await
            .unwrap();
        let message = read_message(&mut socket).await.unwrap();
        assert_eq!(message.id, Some(7));
        let peers: RespondPeersIntroducer = decode_exact(message.data.as_slice()).unwrap();
        assert!(
            peers
                .peer_list
                .iter()
                .any(|p| p.host == peer.ip().to_string() && p.port == 8444)
        );
        let _ = socket.next().await;
        task.await.unwrap();
        assert_eq!(
            metrics.probe_attempts.get(),
            0,
            "request handler must not probe inline"
        );
        dg_xch_servers::discovery::sqlx::query(
            "DELETE FROM dg_discovery.peers WHERE network=$1 AND ip=$2::inet AND port=$3",
        )
        .bind(&store.network)
        .bind(peer.ip().to_string())
        .bind(i32::from(peer.port()))
        .execute(&store.pool)
        .await
        .unwrap();
    }
    #[tokio::test]
    #[ignore = "requires isolated PostgreSQL via DGX_TEST_POSTGRES_URL"]
    async fn background_verifier_retries_and_records_real_tls_metadata() {
        use dg_xch_core::{
            constants::{CHIA_CA_CRT, CHIA_CA_KEY},
            ssl::{
                generate_ca_signed_cert_data, load_certs_from_bytes, load_private_key_from_bytes,
            },
        };
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let directory = tempfile::tempdir().unwrap();
        identity(directory.path());
        let config = Arc::new(configuration(directory.path()));
        let store = Store::connect(
            &std::env::var("DGX_TEST_POSTGRES_URL").unwrap(),
            "testnet11".into(),
        )
        .await
        .unwrap();
        store
            .configure(
                Role::Introducer,
                Policy {
                    crawler: None,
                    concurrency: 1,
                    interval_ms: 500,
                    timeout_seconds: 3,
                    revisit_seconds: 30,
                    cooldown_seconds: 60,
                    allow_private: true,
                },
            )
            .await
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        store.enqueue_verification(addr).await.unwrap();
        let metrics = Metrics::new(&config).unwrap();
        let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
        let task = tokio::spawn(verify_loop(
            config.clone(),
            config.tls.client().unwrap(),
            store.clone(),
            metrics.clone(),
            stop_rx,
        ));
        let start = Instant::now();
        while !dg_xch_servers::discovery::sqlx::query_scalar::<_, bool>("SELECT last_error IS NOT NULL FROM dg_discovery.jobs WHERE network=$1 AND role='introducer' AND ip=$2::inet AND port=$3")
            .bind(&store.network).bind(addr.ip().to_string()).bind(i32::from(addr.port())).fetch_one(&store.pool).await.unwrap() {
            assert!(start.elapsed() < Duration::from_secs(10));
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert!(
            store
                .peer(addr)
                .await
                .unwrap()
                .unwrap()
                .verified_until
                .is_none()
        );
        let listener = TcpListener::bind(addr).await.unwrap();
        let (cert, key) =
            generate_ca_signed_cert_data(CHIA_CA_CRT.as_bytes(), CHIA_CA_KEY.as_bytes()).unwrap();
        let tls = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                load_certs_from_bytes(&cert).unwrap(),
                load_private_key_from_bytes(&key).unwrap(),
            )
            .unwrap();
        let node = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let tls = TlsAcceptor::from(Arc::new(tls)).accept(tcp).await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(tls).await.unwrap();
            let _ = read_message(&mut socket).await.unwrap();
            socket
                .send(
                    encode(
                        ProtocolMessageTypes::Handshake,
                        &handshake("testnet11", NodeType::FullNode, addr.port()),
                        None,
                    )
                    .unwrap(),
                )
                .await
                .unwrap();
            let _ = socket.next().await;
        });
        dg_xch_servers::discovery::sqlx::query("UPDATE dg_discovery.jobs SET next_attempt=0 WHERE network=$1 AND role='introducer' AND ip=$2::inet AND port=$3").bind(&store.network).bind(addr.ip().to_string()).bind(i32::from(addr.port())).execute(&store.pool).await.unwrap();
        dg_xch_servers::discovery::sqlx::query("DELETE FROM dg_discovery.cooldowns WHERE network=$1 AND role='introducer' AND ip=$2::inet").bind(&store.network).bind(addr.ip().to_string()).execute(&store.pool).await.unwrap();
        let start = Instant::now();
        loop {
            if store
                .peer(addr)
                .await
                .unwrap()
                .unwrap()
                .verified_until
                .is_some()
            {
                break;
            }
            assert!(start.elapsed() < Duration::from_secs(10));
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let peer = store.peer(addr).await.unwrap().unwrap();
        assert_eq!(peer.node_id.unwrap().len(), 64);
        assert!(peer.software_version.is_some());
        assert!(peer.first_success.is_some());
        assert!(
            store
                .response("127.0.0.2:8444".parse().unwrap(), true)
                .await
                .unwrap()
                .iter()
                .any(|p| p.0 == addr)
        );
        let _ = stop_tx.send(true);
        task.await.unwrap();
        node.await.unwrap();
        dg_xch_servers::discovery::sqlx::query(
            "DELETE FROM dg_discovery.peers WHERE network=$1 AND ip=$2::inet AND port=$3",
        )
        .bind(&store.network)
        .bind(addr.ip().to_string())
        .bind(i32::from(addr.port()))
        .execute(&store.pool)
        .await
        .unwrap();
    }
}
