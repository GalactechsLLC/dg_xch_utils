#![cfg(unix)]
use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
struct Guard(Child);
impl Drop for Guard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn address() -> SocketAddr {
    let reservation = TcpListener::bind("127.0.0.1:0").unwrap();
    reservation.local_addr().unwrap()
}
fn get(addr: SocketAddr, path: &str) -> String {
    let mut socket = TcpStream::connect(addr).unwrap();
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
}
fn launch(config: &serde_json::Value, args: &[&str], addr: SocketAddr, metric: &str, health: u16) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.json");
    std::fs::write(&path, serde_json::to_vec(config).unwrap()).unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_dgx"))
        .arg("--config-dir")
        .arg(directory.path())
        .args(args)
        .arg("--config")
        .arg(&path)
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let mut guard = Guard(child);
    let start = Instant::now();
    loop {
        if TcpStream::connect(addr).is_ok() {
            break;
        }
        assert!(
            guard.0.try_wait().unwrap().is_none(),
            "service exited before monitoring startup"
        );
        assert!(start.elapsed() < Duration::from_secs(15));
        std::thread::sleep(Duration::from_millis(25));
    }
    let text = get(addr, "/metrics");
    assert!(text.starts_with("HTTP/1.1 200"));
    assert!(text.contains(metric), "{text}");
    assert!(get(addr, "/health").starts_with(&format!("HTTP/1.1 {health}")));
    assert!(get(addr, "/get_blockchain_state").starts_with("HTTP/1.1 404"));
    Command::new("kill")
        .args(["-TERM", &guard.0.id().to_string()])
        .status()
        .unwrap();
    let start = Instant::now();
    loop {
        if let Some(status) = guard.0.try_wait().unwrap() {
            assert!(status.success(), "{status}");
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(25));
    }
}
fn tls(root: &std::path::Path) {
    use dg_xch_core::constants::{CHIA_CA_CRT, CHIA_CA_KEY};
    use std::os::unix::fs::OpenOptionsExt;
    for relative in [
        "ca/private_ca.crt",
        "ca/chia_ca.crt",
        "farmer/private_farmer.crt",
        "farmer/public_farmer.crt",
        "harvester/private_harvester.crt",
    ] {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, CHIA_CA_CRT).unwrap();
    }
    for relative in [
        "farmer/private_farmer.key",
        "farmer/public_farmer.key",
        "harvester/private_harvester.key",
    ] {
        std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(root.join(relative))
            .unwrap()
            .write_all(CHIA_CA_KEY.as_bytes())
            .unwrap();
    }
}
#[test]
fn farmer_cli_exports_farming_metrics_and_runtime_health() {
    use dg_xch_farmer::farmer::config::{Config, FarmingInfo, MetricsConfig};
    let directory = tempfile::tempdir().unwrap();
    tls(directory.path());
    let addr = address();
    let key = blst::min_pk::SecretKey::key_gen(&[42; 32], &[]).unwrap();
    let config = Config::<()> {
        ssl_root_path: Some(directory.path().display().to_string()),
        fullnode_ws_host: "127.0.0.1".into(),
        fullnode_ws_port: 1,
        fullnode_rpc_host: "127.0.0.1".into(),
        fullnode_rpc_port: 1,
        farmer_info: vec![FarmingInfo {
            farmer_secret_key: key.to_bytes().into(),
            ..Default::default()
        }],
        payout_address: hex::encode([3; 32]),
        metrics: Some(MetricsConfig {
            enabled: true,
            port: addr.port(),
        }),
        ..Default::default()
    };
    launch(
        &serde_json::to_value(config).unwrap(),
        &["farmer"],
        addr,
        "chia_farmer_pos2_loaded_plots 0\n",
        200,
    );
}
#[cfg(feature = "timelord")]
#[test]
fn both_timelord_modes_export_metrics_and_disconnected_readiness() {
    let directory = tempfile::tempdir().unwrap();
    tls(directory.path());
    for mode in ["run", "compact"] {
        let addr = address();
        let config = serde_json::json!({"fullnode_host":"127.0.0.1","fullnode_port":1,"server_name":"chia.net",
            "tls":{"certificate":directory.path().join("farmer/public_farmer.crt"),"private_key":directory.path().join("farmer/public_farmer.key"),"ca_certificate":directory.path().join("ca/chia_ca.crt")},
            "max_iterations":1024,"job_timeout_seconds":1,"reconnect_seconds":1,"metrics_listen":addr});
        let name = if mode == "run" { "regular" } else { "compact" };
        launch(
            &config,
            &["timelord", mode],
            addr,
            &format!("chia_timelord_{name}_connected 0\n"),
            503,
        );
    }
}
