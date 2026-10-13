#[cfg(unix)]
#[test]
#[ignore = "requires isolated PostgreSQL via DGX_TEST_POSTGRES_URL"]
fn crawler_postgres_env_routes_and_cursor_sorting_work_end_to_end() {
    use dg_xch_servers::discovery::{Policy, Role, Store, sqlx};
    use std::{
        io::{Read, Write},
        process::{Command, Stdio},
        time::{Duration, Instant},
    };
    struct Guard(std::process::Child);
    impl Drop for Guard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let url = std::env::var("DGX_TEST_POSTGRES_URL").unwrap();
    let temp = tempfile::tempdir().unwrap();
    let network = format!("cli-{}", temp.path().file_name().unwrap().to_string_lossy());
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let store=runtime.block_on(async{let s=Store::connect(&url,network.clone()).await.unwrap();s.configure(Role::Crawler,Policy{concurrency:4,interval_ms:2000,timeout_seconds:1,revisit_seconds:3600,cooldown_seconds:600,allow_private:false}).await.unwrap();for n in 1..=65{s.seen(format!("127.0.0.{n}:8444").parse().unwrap(),"fixture",None).await.unwrap();}s.seen("[::1]:8444".parse().unwrap(),"fixture",None).await.unwrap();
        sqlx::query("UPDATE dg_discovery.jobs SET next_attempt=9999999999 WHERE network=$1").bind(&network).execute(&s.pool).await.unwrap();
        sqlx::query("UPDATE dg_discovery.peers SET software_version=CASE WHEN host(ip)='127.0.0.1' THEN '2.5.1' WHEN host(ip)='127.0.0.2' THEN '2.4.2' END,last_success=CASE WHEN host(ip)='127.0.0.1' THEN 200 WHEN host(ip)='127.0.0.2' THEN 100 END,peer_request_ms=CASE WHEN host(ip)='127.0.0.1' THEN 100 WHEN host(ip)='127.0.0.2' THEN 10 END WHERE network=$1").bind(&network).execute(&s.pool).await.unwrap();
        sqlx::query("UPDATE dg_discovery.jobs SET attempts=1,last_error='connection refused',last_error_phase='tcp_connect',last_error_at=100 WHERE network=$1 AND host(ip)='127.0.0.3'").bind(&network).execute(&s.pool).await.unwrap();s});
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let child=Command::new(env!("CARGO_BIN_EXE_dgx")).args(["--config-dir",temp.path().to_str().unwrap(),"crawler","--listen",&addr.to_string(),"--seed","127.0.0.1:1"])
        .env("DGX_CRAWLER_CONFIG",serde_json::json!({"network":network,"database_url":"invalid-overridden-url","timeout_seconds":1}).to_string()).env("DGX_CRAWLER_DATABASE_URL",&url).stdout(Stdio::null()).stderr(Stdio::inherit()).spawn().unwrap();
    let mut guard = Guard(child);
    let start = Instant::now();
    while std::net::TcpStream::connect(addr).is_err() {
        assert!(guard.0.try_wait().unwrap().is_none());
        assert!(start.elapsed() < Duration::from_secs(30));
        std::thread::sleep(Duration::from_millis(25));
    }
    let request = |path: &str| {
        let mut socket = std::net::TcpStream::connect(addr).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        write!(
            socket,
            "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        let mut result = String::new();
        socket.read_to_string(&mut result).unwrap();
        result
    };
    let json = |path: &str| {
        let response = request(path);
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        serde_json::from_str::<serde_json::Value>(response.split_once("\r\n\r\n").unwrap().1)
            .unwrap()
    };
    let html = request("/");
    assert!(html.contains("Peer ecosystem"));
    assert!(
        !html
            .split("</header>")
            .next()
            .unwrap()
            .contains("href=\"/metrics\"")
    );
    assert!(html.contains("id=\"auto-reload\""));
    assert!(request("/site.css").contains("--bg:#87db94"));
    assert!(request("/style.css").contains(".table-scroll{overflow:auto"));
    for path in ["/health", "/ready", "/api/history"] {
        assert!(request(path).starts_with("HTTP/1.1 200"));
    }
    assert!(request("/get_blockchain_state").starts_with("HTTP/1.1 404"));
    assert!(request("/api/peers?sort=bad").starts_with("HTTP/1.1 400"));
    assert!(request("/api/peers?cursor=not-hex").starts_with("HTTP/1.1 400"));
    assert_eq!(
        json("/api/peers?sort=last_connected&order=desc")["peers"][0]["endpoint"],
        "127.0.0.1:8444"
    );
    assert_eq!(
        json("/api/peers?sort=version&order=asc")["peers"][0]["endpoint"],
        "127.0.0.2:8444"
    );
    assert_eq!(
        json("/api/peers?sort=ping&order=asc")["peers"][0]["endpoint"],
        "127.0.0.2:8444"
    );
    assert_eq!(json("/api/peers?status=failed")["total"], 1);
    assert_eq!(json("/api/peers?family=ipv6")["total"], 1);
    assert_eq!(json("/api/peers?search=refused")["total"], 1);
    let first = json("/api/peers?limit=50");
    let token = first["next_cursor"].as_str().unwrap();
    let next = json(&format!("/api/peers?limit=50&cursor={token}"));
    assert_eq!(next["peers"].as_array().unwrap().len(), 16);
    assert!(next["next_cursor"].is_null());
    let stats = json("/api/stats");
    assert_eq!(stats["known"], 66);
    assert_eq!(stats["failed_latest"], 1);
    assert!(stats.get("max_peers").is_none());
    let metrics = request("/metrics");
    assert!(metrics.contains("chia_crawler_known_endpoints"));
    assert!(metrics.contains(&format!("network=\"{network}\"")));
    Command::new("kill")
        .args(["-TERM", &guard.0.id().to_string()])
        .status()
        .unwrap();
    let start = Instant::now();
    while guard.0.try_wait().unwrap().is_none() {
        assert!(start.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(25));
    }
    runtime.block_on(async {
        sqlx::query("DELETE FROM dg_discovery.peers WHERE network=$1")
            .bind(&network)
            .execute(&store.pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM dg_discovery.budgets WHERE network=$1")
            .bind(&network)
            .execute(&store.pool)
            .await
            .unwrap();
    });
}
