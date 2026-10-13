use dg_xch_core::{
    blockchain::peer_info::TimestampedPeerInfo,
    protocols::{NodeType, shared::Handshake},
};
use dg_xch_servers::discovery::{
    Direction, Family, Observation, Page, Phase, Policy, Role, Sort, Status, Store, sqlx,
};
use std::{net::SocketAddr, time::Duration};
fn url() -> String {
    std::env::var("DGX_TEST_POSTGRES_URL")
        .expect("set DGX_TEST_POSTGRES_URL to an isolated test database")
}
fn policy() -> Policy {
    Policy {
        concurrency: 4,
        interval_ms: 2000,
        timeout_seconds: 15,
        revisit_seconds: 3600,
        cooldown_seconds: 600,
        allow_private: true,
    }
}
async fn setup() -> (Store, Store) {
    let network = format!("test-{}", uuid::Uuid::new_v4());
    let a = Store::connect(&url(), network.clone()).await.unwrap();
    let b = Store::connect(&url(), network).await.unwrap();
    a.configure(Role::Crawler, policy()).await.unwrap();
    a.configure(
        Role::Introducer,
        Policy {
            timeout_seconds: 3,
            revisit_seconds: 30,
            cooldown_seconds: 60,
            ..policy()
        },
    )
    .await
    .unwrap();
    (a, b)
}
fn success(network: &str) -> Observation {
    Observation {
        handshake: Some(Handshake {
            network_id: network.into(),
            protocol_version: "0.0.37".into(),
            software_version: "2.5.1".into(),
            server_port: 8444,
            node_type: NodeType::FullNode as u8,
            capabilities: vec![(1, "1".into())],
        }),
        node_id: Some("a".repeat(64)),
        connect_ms: Some(10.0),
        peer_request_ms: Some(20.0),
        phase: Phase::PeerRequest,
        ..Observation::default()
    }
}
async fn reset(s: &Store, role: Role) {
    sqlx::query("UPDATE dg_discovery.budgets SET next_start=now()-interval '1 second' WHERE network=$1 AND role=$2").bind(&s.network).bind(role.as_str()).execute(&s.pool).await.unwrap();
    sqlx::query("DELETE FROM dg_discovery.cooldowns WHERE network=$1 AND role=$2")
        .bind(&s.network)
        .bind(role.as_str())
        .execute(&s.pool)
        .await
        .unwrap();
}
async fn due(s: &Store) {
    sqlx::query("UPDATE dg_discovery.jobs SET next_attempt=0 WHERE network=$1 AND role='crawler'")
        .bind(&s.network)
        .execute(&s.pool)
        .await
        .unwrap();
    reset(s, Role::Crawler).await;
}
async fn cleanup(s: &Store) {
    for table in [
        "peers",
        "cooldowns",
        "counters",
        "history",
        "observations",
        "budgets",
    ] {
        sqlx::query(&format!(
            "DELETE FROM dg_discovery.{table} WHERE network=$1"
        ))
        .bind(&s.network)
        .execute(&s.pool)
        .await
        .unwrap();
    }
}
#[tokio::test]
#[ignore = "requires isolated PostgreSQL via DGX_TEST_POSTGRES_URL"]
async fn shared_seen_metadata_errors_retries_and_completion_fencing() {
    let (a, b) = setup().await;
    let endpoint: SocketAddr = "[::1]:8444".parse().unwrap();
    a.seen(endpoint, "seed", None).await.unwrap();
    let first = a.peer(endpoint).await.unwrap().unwrap().first_seen;
    b.seen(endpoint, "gossip", Some(u64::MAX)).await.unwrap();
    let peer = a.peer(endpoint).await.unwrap().unwrap();
    assert_eq!(peer.first_seen, first);
    assert_eq!(peer.first_source, "seed");
    assert_eq!(peer.reported_gossip_timestamp, Some(u64::MAX));
    let (c1, c2) = tokio::join!(a.claim(Role::Crawler), b.claim(Role::Crawler));
    let c1 = c1.unwrap();
    let c2 = c2.unwrap();
    assert_eq!(usize::from(c1.is_some()) + usize::from(c2.is_some()), 1);
    let old = c1.or(c2).unwrap();
    reset(&a, Role::Crawler).await;
    assert!(b.claim(Role::Crawler).await.unwrap().is_none());
    sqlx::query("UPDATE dg_discovery.jobs SET lease_until=0,next_attempt=0 WHERE network=$1")
        .bind(&a.network)
        .execute(&a.pool)
        .await
        .unwrap();
    let new = b.claim(Role::Crawler).await.unwrap().unwrap();
    assert!(!a.finish(old, success(&a.network), None).await.unwrap());
    let repeated = new.clone();
    assert!(
        b.finish(
            new,
            Observation {
                error: Some("connection refused".into()),
                phase: Phase::TcpConnect,
                ..Observation::default()
            },
            None
        )
        .await
        .unwrap()
    );
    assert!(!a.finish(repeated, success(&a.network), None).await.unwrap());
    let peer = a.peer(endpoint).await.unwrap().unwrap();
    assert_eq!(peer.last_error_phase, Some(Phase::TcpConnect));
    assert_eq!(peer.consecutive_failures, 1);
    assert!(peer.next_attempt >= b.clock().await.unwrap() + 600);
    assert_eq!(a.totals(Role::Crawler).await.unwrap().attempts, 2);
    assert_eq!(a.totals(Role::Crawler).await.unwrap().failures, 1);
    due(&a).await;
    let recovery = a.claim(Role::Crawler).await.unwrap().unwrap();
    let mut obs = success(&a.network);
    obs.peak_height = Some(42);
    obs.peers = vec![
        TimestampedPeerInfo {
            host: "127.0.0.2".into(),
            port: 8444,
            timestamp: 123,
        },
        TimestampedPeerInfo {
            host: "not-an-ip".into(),
            port: 8444,
            timestamp: 1,
        },
    ];
    assert!(a.finish(recovery, obs, None).await.unwrap());
    let recovered = b.peer(endpoint).await.unwrap().unwrap();
    assert!(recovered.last_error.is_none());
    assert!(recovered.last_error_phase.is_none());
    assert!(recovered.last_error_at.is_none());
    assert_eq!(recovered.consecutive_failures, 0);
    assert_eq!(recovered.node_id.as_deref(), Some("a".repeat(64).as_str()));
    assert_eq!(recovered.peak_height, Some(42));
    assert_eq!(b.counts().await.unwrap().known, 2);
    assert_eq!(b.counts().await.unwrap().verified, 0);
    assert_eq!(b.totals(Role::Crawler).await.unwrap().gossip_rejected, 1);
    let events: i64 =
        sqlx::query_scalar("SELECT count(*) FROM dg_discovery.observations WHERE network=$1")
            .bind(&a.network)
            .fetch_one(&a.pool)
            .await
            .unwrap();
    assert_eq!(
        events, 2,
        "duplicate/stale completion must not add a history record"
    );
    let summary = b.summary(86400).await.unwrap();
    assert_eq!(summary.known, 2);
    assert_eq!(summary.never_attempted, 1);
    assert_eq!(summary.recent, 1);
    a.sample(&summary).await.unwrap();
    b.sample(&summary).await.unwrap();
    assert_eq!(b.history().await.unwrap().len(), 1);
    cleanup(&a).await;
}
#[tokio::test]
#[ignore = "requires isolated PostgreSQL via DGX_TEST_POSTGRES_URL"]
async fn budgets_cooldowns_and_verified_eligibility_are_network_and_role_scoped() {
    let (a, b) = setup().await;
    for peer in ["127.0.0.1:8444", "127.0.0.1:8445", "127.0.0.2:8444"] {
        a.seen(peer.parse().unwrap(), "seed", None).await.unwrap();
    }
    let first = a.claim(Role::Crawler).await.unwrap().unwrap();
    assert!(b.claim(Role::Crawler).await.unwrap().is_none());
    a.configure(
        Role::Crawler,
        Policy {
            concurrency: 1,
            ..policy()
        },
    )
    .await
    .unwrap();
    sqlx::query(
        "UPDATE dg_discovery.budgets SET next_start=now()-interval '1 second' WHERE network=$1",
    )
    .bind(&a.network)
    .execute(&a.pool)
    .await
    .unwrap();
    assert!(b.claim(Role::Crawler).await.unwrap().is_none());
    a.configure(Role::Crawler, policy()).await.unwrap();
    let second = b.claim(Role::Crawler).await.unwrap().unwrap();
    assert_ne!(first.endpoint.ip(), second.endpoint.ip());
    a.enqueue_verification(first.endpoint).await.unwrap();
    let verification = b.claim(Role::Introducer).await.unwrap().unwrap();
    assert_eq!(verification.endpoint, first.endpoint);
    assert!(a.response(second.endpoint, true).await.unwrap().is_empty());
    b.finish(verification, success(&b.network), Some(60))
        .await
        .unwrap();
    assert_eq!(a.counts().await.unwrap().verified, 1);
    assert_eq!(
        a.response(second.endpoint, true).await.unwrap()[0].0,
        first.endpoint
    );
    assert!(a.response(first.endpoint, true).await.unwrap().is_empty());
    sqlx::query("UPDATE dg_discovery.peers SET verified_until=0 WHERE network=$1")
        .bind(&a.network)
        .execute(&a.pool)
        .await
        .unwrap();
    assert!(b.response(second.endpoint, true).await.unwrap().is_empty());
    assert_eq!(a.counts().await.unwrap().known, 3);
    let other = Store::connect(&url(), format!("other-{}", a.network))
        .await
        .unwrap();
    other.configure(Role::Crawler, policy()).await.unwrap();
    other
        .seen(first.endpoint, "different-network", None)
        .await
        .unwrap();
    assert!(other.claim(Role::Crawler).await.unwrap().is_some());
    assert_eq!(other.counts().await.unwrap().known, 1);
    assert_eq!(other.totals(Role::Crawler).await.unwrap().attempts, 1);
    assert_eq!(a.totals(Role::Crawler).await.unwrap().attempts, 2);
    cleanup(&a).await;
    cleanup(&other).await;
}
#[tokio::test]
#[ignore = "requires isolated PostgreSQL via DGX_TEST_POSTGRES_URL"]
async fn indexed_cursor_pagination_sorts_nulls_last_without_duplicates() {
    let (a, _) = setup().await;
    for n in 1..=125 {
        a.seen(format!("10.0.0.{n}:8444").parse().unwrap(), "fixture", None)
            .await
            .unwrap();
    }
    sqlx::query("UPDATE dg_discovery.peers SET software_version=CASE WHEN port=8444 AND split_part(host(ip),'.',4)::integer%3=0 THEN NULL ELSE '2.5.1' END,peer_request_ms=CASE WHEN split_part(host(ip),'.',4)::integer%3=0 THEN NULL ELSE 10 END,last_success=CASE WHEN split_part(host(ip),'.',4)::integer%3=0 THEN NULL ELSE 100 END WHERE network=$1").bind(&a.network).execute(&a.pool).await.unwrap();
    for sort in [
        Sort::Endpoint,
        Sort::Version,
        Sort::LastConnected,
        Sort::Ping,
        Sort::FirstSeen,
        Sort::LastAttempt,
        Sort::NextAttempt,
        Sort::Failures,
    ] {
        for order in [Direction::Asc, Direction::Desc] {
            let mut page = Page {
                sort,
                order,
                limit: 17,
                ..Page::default()
            };
            let mut seen = std::collections::BTreeSet::new();
            let mut missing = false;
            loop {
                let result = a.page(&page).await.unwrap();
                assert_eq!(result.total, 125);
                for p in result.peers {
                    assert!(seen.insert(p.endpoint));
                    let null = match sort {
                        Sort::Version => p.software_version.is_none(),
                        Sort::LastConnected => p.last_success.is_none(),
                        Sort::Ping => p.peer_request_ms.is_none(),
                        _ => false,
                    };
                    assert!(!missing || null);
                    missing |= null;
                }
                page.cursor = result.next_cursor;
                if page.cursor.is_none() {
                    break;
                }
            }
            assert_eq!(seen.len(), 125);
        }
    }
    a.seen("[::1]:8444".parse().unwrap(), "v6", None)
        .await
        .unwrap();
    assert_eq!(
        a.page(&Page {
            family: Family::Ipv6,
            ..Page::default()
        })
        .await
        .unwrap()
        .total,
        1
    );
    assert_eq!(
        a.page(&Page {
            status: Status::Unattempted,
            ..Page::default()
        })
        .await
        .unwrap()
        .total,
        126
    );
    assert_eq!(
        a.page(&Page {
            search: "10.0.0.1".into(),
            ..Page::default()
        })
        .await
        .unwrap()
        .total,
        1
    );
    let token = a
        .page(&Page {
            limit: 1,
            ..Page::default()
        })
        .await
        .unwrap()
        .next_cursor
        .unwrap();
    let other = Store::connect(&url(), format!("other-{}", a.network))
        .await
        .unwrap();
    assert!(
        other
            .page(&Page {
                cursor: Some(token),
                ..Page::default()
            })
            .await
            .is_err()
    );
    cleanup(&a).await;
}
#[tokio::test]
#[ignore = "requires isolated PostgreSQL via DGX_TEST_POSTGRES_URL"]
async fn typed_schema_partitions_and_indexes_exclude_chain_store_tables() {
    let (a, _) = setup().await;
    let tables: Vec<String> =
        sqlx::query_scalar("SELECT tablename FROM pg_tables WHERE schemaname='dg_discovery'")
            .fetch_all(&a.pool)
            .await
            .unwrap();
    assert!(tables.iter().all(|name| {
        [
            "budgets",
            "peers",
            "jobs",
            "cooldowns",
            "counters",
            "history",
            "observations",
        ]
        .iter()
        .any(|p| name.starts_with(p))
    }));
    let json_columns:Vec<String>=sqlx::query_scalar("SELECT table_name||'.'||column_name FROM information_schema.columns WHERE table_schema='dg_discovery' AND data_type='jsonb' AND table_name IN ('peers','jobs','budgets','counters','history','observations')").fetch_all(&a.pool).await.unwrap();
    assert_eq!(json_columns, vec!["peers.capabilities"]);
    let indexes: Vec<String> =
        sqlx::query_scalar("SELECT indexdef FROM pg_indexes WHERE schemaname='dg_discovery'")
            .fetch_all(&a.pool)
            .await
            .unwrap();
    assert!(indexes.iter().all(|s| s.contains("(network,")));
    for network in ["mainnet", "testnet11"] {
        let plan:serde_json::Value=sqlx::query_scalar("EXPLAIN (FORMAT JSON) SELECT endpoint FROM dg_discovery.peers WHERE network=$1 ORDER BY last_success DESC NULLS LAST,ip,port LIMIT 50").bind(network).fetch_one(&a.pool).await.unwrap();
        let plan = plan.to_string();
        assert!(plan.contains(&format!("peers_{network}")));
        assert!(!plan.contains(if network == "mainnet" {
            "peers_testnet11"
        } else {
            "peers_mainnet"
        }));
        assert!(!plan.contains("peers_custom"));
    }
    cleanup(&a).await;
}
#[tokio::test]
#[ignore = "requires isolated PostgreSQL via DGX_TEST_POSTGRES_URL"]
async fn replica_startup_and_retention_do_not_block_or_delete_live_work() {
    let (a, _) = setup().await;
    let endpoint: SocketAddr = "127.0.0.1:8444".parse().unwrap();
    a.seen(endpoint, "seed", None).await.unwrap();
    let claim = a.claim(Role::Crawler).await.unwrap().unwrap();
    sqlx::query("UPDATE dg_discovery.peers SET first_seen=1,last_seen=1 WHERE network=$1")
        .bind(&a.network)
        .execute(&a.pool)
        .await
        .unwrap();
    a.maintenance(Role::Crawler, 90).await.unwrap();
    assert!(a.peer(endpoint).await.unwrap().is_some());
    let mut writer = a.pool.begin().await.unwrap();
    sqlx::query("UPDATE dg_discovery.jobs SET attempts=attempts WHERE network=$1")
        .bind(&a.network)
        .execute(&mut *writer)
        .await
        .unwrap();
    let replica = tokio::time::timeout(
        Duration::from_secs(2),
        Store::connect(&url(), a.network.clone()),
    )
    .await
    .unwrap()
    .unwrap();
    writer.rollback().await.unwrap();
    replica
        .finish(
            claim,
            Observation {
                error: Some("refused".into()),
                phase: Phase::TcpConnect,
                ..Observation::default()
            },
            None,
        )
        .await
        .unwrap();
    sqlx::query("UPDATE dg_discovery.budgets SET maintenance_after=now()-interval '1 second' WHERE network=$1").bind(&a.network).execute(&a.pool).await.unwrap();
    a.maintenance(Role::Crawler, 90).await.unwrap();
    assert!(a.peer(endpoint).await.unwrap().is_none());
    assert_eq!(a.totals(Role::Crawler).await.unwrap().pruned, 1);
    cleanup(&a).await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL via DGX_TEST_POSTGRES_URL"]
async fn overlapping_gossip_completes_without_deadlocks_and_wrong_network_is_rejected() {
    let (a, b) = setup().await;
    for endpoint in ["127.1.0.1:8444", "127.1.0.2:8444"] {
        a.seen(endpoint.parse().unwrap(), "seed", None)
            .await
            .unwrap();
    }
    let ca = a.claim(Role::Crawler).await.unwrap().unwrap();
    reset(&a, Role::Crawler).await;
    let cb = b.claim(Role::Crawler).await.unwrap().unwrap();
    let mut first = success(&a.network);
    first.peers = [ca.endpoint, cb.endpoint, "127.1.0.3:8444".parse().unwrap()]
        .into_iter()
        .map(|ep| TimestampedPeerInfo {
            host: ep.ip().to_string(),
            port: ep.port(),
            timestamp: 42,
        })
        .collect();
    let mut second = success(&a.network);
    second.peers = first.peers.clone();
    let (ra, rb) = tokio::join!(a.finish(ca, first, None), b.finish(cb, second, None));
    assert!(ra.unwrap());
    assert!(rb.unwrap());
    assert_eq!(a.summary(86400).await.unwrap().known, 3);
    due(&a).await;
    let claim = a.claim(Role::Crawler).await.unwrap().unwrap();
    a.finish(claim.clone(), success("wrong-network"), None)
        .await
        .unwrap();
    let peer = a.peer(claim.endpoint).await.unwrap().unwrap();
    assert_eq!(peer.last_error_phase, Some(Phase::Handshake));
    assert!(peer.last_error.unwrap().contains("network"));
    cleanup(&a).await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL via DGX_TEST_POSTGRES_URL"]
async fn hot_dashboard_sorts_use_partition_indexes_with_large_catalogs() {
    let (a, _) = setup().await;
    let mut tx = a.pool.begin().await.unwrap();
    sqlx::query("SET LOCAL statement_timeout='30s'")
        .execute(&mut *tx)
        .await
        .unwrap();
    for network in ["mainnet", "testnet11"] {
        sqlx::query("INSERT INTO dg_discovery.peers(network,ip,port,publicly_routable,first_seen,last_seen,first_source,last_success,software_version,peer_request_ms) SELECT $1,'198.18.0.0'::inet+n,8444,false,1,1,'query-plan-fixture',n,'2.5.'||(n%10)::text,n::double precision FROM generate_series(1,20000) n ON CONFLICT DO NOTHING")
            .bind(network).execute(&mut *tx).await.unwrap();
    }
    sqlx::query("ANALYZE dg_discovery.peers")
        .execute(&mut *tx)
        .await
        .unwrap();
    for network in ["mainnet", "testnet11"] {
        for column in ["last_success", "software_version", "peer_request_ms"] {
            for direction in ["ASC", "DESC"] {
                let query = format!(
                    "EXPLAIN (FORMAT JSON) SELECT p.endpoint,j.last_error FROM dg_discovery.peers p LEFT JOIN dg_discovery.jobs j ON j.network=p.network AND j.ip=p.ip AND j.port=p.port AND j.role='crawler' WHERE p.network=$1 ORDER BY p.{column} {direction} NULLS LAST,p.ip,p.port LIMIT 50"
                );
                let plan: serde_json::Value = sqlx::query_scalar(&query)
                    .bind(network)
                    .fetch_one(&mut *tx)
                    .await
                    .unwrap();
                let plan = plan.to_string();
                assert!(
                    plan.contains("Index Scan") || plan.contains("Index Only Scan"),
                    "{column} {direction}: {plan}"
                );
                assert!(
                    !plan.contains("\"Node Type\":\"Sort\""),
                    "{column} {direction}: {plan}"
                );
                assert!(!plan.contains(if network == "mainnet" {
                    "peers_testnet11"
                } else {
                    "peers_mainnet"
                }));
                assert!(!plan.contains("peers_custom"));
            }
        }
    }
    tx.rollback().await.unwrap();
    cleanup(&a).await;
}
