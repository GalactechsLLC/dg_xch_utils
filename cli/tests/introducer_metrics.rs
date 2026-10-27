#[cfg(unix)]
#[test]
#[ignore = "requires isolated PostgreSQL via DGX_TEST_POSTGRES_URL"]
fn introducer_postgres_env_configuration_launches_and_stops() {
    launch();
}
#[cfg(unix)]
fn launch() {
    use dg_xch_core::{
        constants::{CHIA_CA_CRT, CHIA_CA_KEY},
        ssl::generate_ca_signed_cert_data,
    };
    use std::{
        io::{Read, Write},
        net::{TcpListener, TcpStream},
        process::{Command, Stdio},
        time::{Duration, Instant},
    };
    struct ChildGuard(std::process::Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let temp = tempfile::tempdir().unwrap();
    let peer_reservation = TcpListener::bind("127.0.0.1:0").unwrap();
    let peer_addr = peer_reservation.local_addr().unwrap();
    let metrics_reservation = TcpListener::bind("127.0.0.1:0").unwrap();
    let metrics_addr = metrics_reservation.local_addr().unwrap();
    let (cert, key) =
        generate_ca_signed_cert_data(CHIA_CA_CRT.as_bytes(), CHIA_CA_KEY.as_bytes()).unwrap();
    std::fs::write(temp.path().join("cert.pem"), cert).unwrap();
    std::fs::write(temp.path().join("key.pem"), key).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(
        temp.path().join("key.pem"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    std::fs::write(temp.path().join("ca.pem"), CHIA_CA_CRT).unwrap();
    let config = serde_json::json!({"listen":peer_addr,"metrics_listen":null,
        "tls":{"certificate":temp.path().join("cert.pem"),"private_key":temp.path().join("key.pem"),"ca_certificate":temp.path().join("ca.pem")},
        "peer_server_name":"chia.net","max_connections":2,"peer_ttl_seconds":60});
    let config_file = temp.path().join("config.json");
    std::fs::write(&config_file, serde_json::to_vec(&config).unwrap()).unwrap();
    drop(peer_reservation);
    drop(metrics_reservation);
    let mut command = Command::new(env!("CARGO_BIN_EXE_dgx"));
    command.args(["--config-dir", temp.path().to_str().unwrap(), "introducer"]);
    command
        .env(
            "DGX_INTRODUCER_CONFIG",
            serde_json::to_string(&config).unwrap(),
        )
        .env(
            "DGX_INTRODUCER_DATABASE_URL",
            std::env::var("DGX_TEST_POSTGRES_URL").unwrap(),
        );
    let child = command
        .args(["--metrics-listen", &metrics_addr.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let mut guard = ChildGuard(child);
    let start = Instant::now();
    loop {
        if TcpStream::connect(metrics_addr).is_ok() {
            break;
        }
        assert!(
            guard.0.try_wait().unwrap().is_none(),
            "introducer exited before metrics startup"
        );
        assert!(start.elapsed() < Duration::from_secs(15));
        std::thread::sleep(Duration::from_millis(25));
    }
    let request = |path: &str| {
        let mut socket = TcpStream::connect(metrics_addr).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        write!(
            socket,
            "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        let mut text = String::new();
        socket.read_to_string(&mut text).unwrap();
        text
    };
    assert!(request("/health").starts_with("HTTP/1.1 200"));
    let stats = request("/api/stats");
    assert!(stats.starts_with("HTTP/1.1 200"));
    assert!(stats.contains("\"storage\":\"postgres\""));
    let homepage = request("/");
    assert!(homepage.contains("Druid Garden"));
    assert!(homepage.to_lowercase().contains("content-security-policy:"));
    assert!(request("/site.css").contains("--bg:#87db94"));
    assert!(request("/dashboard.js").contains("/api/stats"));
    assert!(request("/get_blockchain_state").starts_with("HTTP/1.1 404"));
    let first = request("/metrics");
    assert!(first.contains("chia_introducer_connections_total{network=\"mainnet\"} 0\n"));
    assert!(first.contains("chia_introducer_verified_peers{network=\"mainnet\"} 0\n"));
    // An incomplete TLS session remains active and another connection from its IP is rejected.
    let held = TcpStream::connect(peer_addr).unwrap();
    let extra = TcpStream::connect(peer_addr).unwrap();
    drop(extra);
    let wait = Instant::now();
    loop {
        let text = request("/metrics");
        if text.contains("chia_introducer_connections_rejected_total{reason=\"duplicate_ip\",network=\"mainnet\"} 1\n")
        {
            assert!(text.contains("chia_introducer_active_connections{network=\"mainnet\"} 1\n"));
            break;
        }
        assert!(wait.elapsed() < Duration::from_secs(5), "{text}");
        std::thread::sleep(Duration::from_millis(25));
    }
    drop(held);
    let wait = Instant::now();
    loop {
        let text = request("/metrics");
        if text.contains("chia_introducer_sessions_total{result=\"error\",network=\"mainnet\"} 1\n")
        {
            assert!(text.contains("chia_introducer_connections_total{network=\"mainnet\"} 2\n"));
            assert!(text.contains("chia_introducer_active_connections{network=\"mainnet\"} 0\n"));
            assert!(text.contains("chia_introducer_peer_requests_total{network=\"mainnet\"} 0\n"));
            break;
        }
        assert!(wait.elapsed() < Duration::from_secs(5), "{text}");
        std::thread::sleep(Duration::from_millis(25));
    }
    Command::new("kill")
        .args(["-TERM", &guard.0.id().to_string()])
        .status()
        .unwrap();
    let wait = Instant::now();
    loop {
        if let Some(status) = guard.0.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(wait.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(25));
    }
}
