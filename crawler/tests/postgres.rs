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
        crawler: None,
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
        tls_version: Some("TLSv1.3".into()),
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
    // SQL identifiers below come only from this fixed table allowlist; network is bound.
    for table in [
        "peers",
        "cooldowns",
        "counters",
        "history",
        "observations",
        "budgets",
        "geography",
        "subnet_cooldowns",
        "distribution_history",
        "probe_history",
        "node_samples",
        "chain_blocks",
        "chain_status",
    ] {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "DELETE FROM dg_discovery.{table} WHERE network=$1"
        )))
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
    assert_eq!(recovered.tls_version.as_deref(), Some("TLSv1.3"));
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
            "geography",
            "subnet_cooldowns",
            "distribution_history",
            "probe_history",
            "node_samples",
            "chain_blocks",
            "chain_status",
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
    assert!(
        indexes
            .iter()
            .all(|s| s.contains("(network,") || s.contains("(network)"))
    );
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
                // The column and direction are selected only from the literal lists above.
                let query = format!(
                    "EXPLAIN (FORMAT JSON) SELECT p.endpoint,j.last_error FROM dg_discovery.peers p LEFT JOIN dg_discovery.jobs j ON j.network=p.network AND j.ip=p.ip AND j.port=p.port AND j.role='crawler' WHERE p.network=$1 ORDER BY p.{column} {direction} NULLS LAST,p.ip,p.port LIMIT 50"
                );
                let plan: serde_json::Value = sqlx::query_scalar(sqlx::AssertSqlSafe(query))
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

#[tokio::test]
#[ignore = "requires isolated PostgreSQL via DGX_TEST_POSTGRES_URL"]
async fn all_versions_are_pageable_without_the_metrics_cardinality_limit() {
    use dg_xch_servers::discovery::VersionQuery;
    let (a, _) = setup().await;
    for n in 1..=105 {
        let endpoint = format!("127.6.0.{n}:58444").parse().unwrap();
        a.seen(endpoint, "fixture", None).await.unwrap();
        sqlx::query("UPDATE dg_discovery.peers SET last_success=floor(extract(epoch FROM clock_timestamp()))::bigint,software_version=$2 WHERE network=$1 AND ip=$3::inet")
            .bind(&a.network).bind(format!("2.7.4.dev{n:03}")).bind(format!("127.6.0.{n}")).execute(&a.pool).await.unwrap();
    }
    let mut offset = 0;
    let mut versions = std::collections::HashSet::new();
    loop {
        let page = a
            .version_page(86400, &VersionQuery { offset, limit: 17 })
            .await
            .unwrap();
        assert_eq!(page.total_versions, 105);
        assert_eq!(page.total_endpoints, 105);
        for version in page.versions {
            assert_eq!(version.count, 1);
            assert!(versions.insert(version.version));
        }
        match page.next_offset {
            Some(next) => offset = next,
            None => break,
        }
    }
    assert_eq!(versions.len(), 105);
    let summary = a.summary(86400).await.unwrap();
    assert_eq!(summary.versions.len(), 64);
    assert_eq!(summary.other_versions, 41);
    let other = Store::connect(&url(), format!("other-{}", a.network))
        .await
        .unwrap();
    assert_eq!(
        other
            .version_page(
                86400,
                &VersionQuery {
                    offset: 0,
                    limit: 50
                }
            )
            .await
            .unwrap()
            .total_versions,
        0
    );
    cleanup(&a).await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL via DGX_TEST_POSTGRES_URL"]
async fn history_and_diagnostics_use_persisted_network_scoped_probe_results() {
    let (a, b) = setup().await;
    let ipv4 = "127.7.0.1:58444".parse().unwrap();
    let ipv6 = "[::1]:58444".parse().unwrap();
    a.seen(ipv4, "fixture", None).await.unwrap();
    a.seen(ipv6, "fixture", None).await.unwrap();
    let claim = a.claim(Role::Crawler).await.unwrap().unwrap();
    assert!(b.peer(claim.endpoint).await.unwrap().unwrap().inflight);
    a.sample(&a.summary(86400).await.unwrap()).await.unwrap();
    a.finish(claim, success(&a.network), None).await.unwrap();
    reset(&a, Role::Crawler).await;
    let claim = a.claim(Role::Crawler).await.unwrap().unwrap();
    a.sample(&a.summary(86400).await.unwrap()).await.unwrap();
    a.finish(
        claim,
        Observation {
            error: Some("Network is unreachable (os error 101)".into()),
            phase: Phase::TcpConnect,
            ..Observation::default()
        },
        None,
    )
    .await
    .unwrap();
    a.sample(&a.summary(86400).await.unwrap()).await.unwrap();
    let history = b.history().await.unwrap();
    assert_eq!(history.iter().map(|p| p.completed_probes).sum::<usize>(), 2);
    assert_eq!(history.iter().map(|p| p.handshakes).sum::<usize>(), 1);
    assert_eq!(history.iter().map(|p| p.peer_responses).sum::<usize>(), 1);
    assert_eq!(history.iter().map(|p| p.failures).sum::<usize>(), 1);
    assert!(
        history
            .iter()
            .any(|p| p.average_peer_request_ms == Some(20.0))
    );
    let errors = b.diagnostics().await.unwrap();
    assert_eq!(errors.latest_failures, 1);
    assert_eq!(errors.groups.len(), 1);
    assert_eq!(errors.groups[0].reason, "routing");
    assert_eq!(errors.groups[0].ipv6, 1);
    assert_eq!(errors.other_failures, 0);
    let other = Store::connect(&url(), format!("other-{}", a.network))
        .await
        .unwrap();
    assert!(other.history().await.unwrap().is_empty());
    assert_eq!(other.diagnostics().await.unwrap().latest_failures, 0);
    cleanup(&a).await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL via DGX_TEST_POSTGRES_URL"]
async fn crawler_port_fresh_gossip_and_peak_reliability_persist_across_replicas() {
    use dg_xch_servers::discovery::CrawlerPolicy;
    let (a, b) = setup().await;
    let crawler = CrawlerPolicy {
        enable_ipv6: true,
        port: 8444,
        minimum_height: 240000,
        peak_wait_ms: 2500,
        tcp_connect_timeout_seconds: 10,
    };
    a.configure(
        Role::Crawler,
        Policy {
            crawler: Some(crawler),
            interval_ms: 10,
            ..policy()
        },
    )
    .await
    .unwrap();
    let endpoint: SocketAddr = "127.0.0.1:8444".parse().unwrap();
    a.seen("127.0.0.1:8445".parse().unwrap(), "old-catalog", None)
        .await
        .unwrap();
    a.seen(endpoint, "seed", None).await.unwrap();
    let claim = b.claim(Role::Crawler).await.unwrap().unwrap();
    assert_eq!(claim.endpoint, endpoint);
    assert_eq!(
        claim.policy.crawler.unwrap().tcp_connect_timeout_seconds,
        10
    );
    assert_eq!(
        b.summary(5 * 86400)
            .await
            .unwrap()
            .tcp_connect_timeout_seconds,
        10
    );
    let now = a.clock().await.unwrap();
    let mut obs = success(&a.network);
    obs.peak_height = Some(300000);
    obs.peers = [
        ("127.0.0.2", 8444, now),
        ("127.0.0.3", 8445, now),
        ("127.0.0.4", 8444, now - 6 * 86400),
        ("127.0.0.5", 8444, u64::MAX),
        ("127.0.0.6", 8444, 0),
    ]
    .into_iter()
    .map(|(host, port, timestamp)| TimestampedPeerInfo {
        host: host.into(),
        port,
        timestamp,
    })
    .collect();
    let duplicate = claim.clone();
    assert!(a.finish(claim, obs, None).await.unwrap());
    assert!(
        !b.finish(duplicate, success(&a.network), None)
            .await
            .unwrap()
    );
    let peer = b.peer(endpoint).await.unwrap().unwrap();
    assert!(peer.reliable);
    assert_eq!(peer.reliability.tries, 1);
    assert_eq!(peer.reliability.successes, 1);
    assert_eq!(b.totals(Role::Crawler).await.unwrap().gossip_rejected, 4);
    assert_eq!(b.counts().await.unwrap().known, 3);
    assert_eq!(b.summary(5 * 86400).await.unwrap().reliable, 1);
    let replica = Store::connect(&url(), a.network.clone()).await.unwrap();
    assert_eq!(
        replica
            .peer(endpoint)
            .await
            .unwrap()
            .unwrap()
            .reliability
            .successes,
        1
    );
    // A fresh observation from a later scan must not lower the best gossip timestamp.
    a.seen(
        "127.0.0.2:8444".parse().unwrap(),
        "older-gossip",
        Some(now - 60),
    )
    .await
    .unwrap();
    assert_eq!(
        b.peer("127.0.0.2:8444".parse().unwrap())
            .await
            .unwrap()
            .unwrap()
            .reported_gossip_timestamp,
        Some(now)
    );
    sqlx::query("DELETE FROM dg_discovery.peers WHERE network=$1 AND ip<>$2::inet")
        .bind(&a.network)
        .bind(endpoint.ip().to_string())
        .execute(&a.pool)
        .await
        .unwrap();
    // A historically failing node is suspended even if somebody makes its job due.
    due(&a).await;
    let claim = b.claim(Role::Crawler).await.unwrap().unwrap();
    sqlx::query("UPDATE dg_discovery.jobs SET reliability_weight=ARRAY[1.,1.,1.,1.,1.]::float8[],reliability_count=ARRAY[40.,40.,40.,40.,40.]::float8[],reliability_value=ARRAY[0.,0.,0.,0.,0.]::float8[],reliability_tries=40,reliability_successes=0,reliability_at=$2 WHERE network=$1 AND port=8444")
        .bind(&a.network).bind(now as i64 - 1000).execute(&a.pool).await.unwrap();
    let mut low_peak = success(&a.network);
    low_peak.peak_height = Some(42);
    b.finish(claim, low_peak, None).await.unwrap();
    let peer = a.peer(endpoint).await.unwrap().unwrap();
    assert!(!peer.reliable);
    assert!(peer.reliability.ban_until > now);
    assert!(peer.next_attempt >= peer.reliability.ban_until);
    due(&a).await;
    assert!(replica.claim(Role::Crawler).await.unwrap().is_none());
    assert_eq!(a.summary(5 * 86400).await.unwrap().banned, 1);
    cleanup(&a).await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL via DGX_TEST_POSTGRES_URL"]
async fn introducer_referrals_without_timestamps_are_queued_with_address_and_port_checks() {
    use dg_xch_servers::discovery::CrawlerPolicy;
    let (a, b) = setup().await;
    a.configure(
        Role::Crawler,
        Policy {
            crawler: Some(CrawlerPolicy {
                enable_ipv6: true,
                port: 58444,
                minimum_height: 240000,
                peak_wait_ms: 2500,
                tcp_connect_timeout_seconds: 2,
            }),
            allow_private: false,
            ..policy()
        },
    )
    .await
    .unwrap();
    let endpoint: SocketAddr = "8.8.8.8:58444".parse().unwrap();
    a.seen(endpoint, "seed", None).await.unwrap();
    let claim = a.claim(Role::Crawler).await.unwrap().unwrap();
    let mut obs = success(&a.network);
    obs.handshake.as_mut().unwrap().node_type = NodeType::Introducer as u8;
    obs.peers = [
        ("1.1.1.1", 58444, 0),
        ("9.9.9.9", 58444, u64::MAX),
        ("2606:4700::1111", 58444, 0),
        ("1.0.0.1", 8444, 0),
        ("127.0.0.1", 58444, 0),
        ("invalid-host", 58444, 0),
    ]
    .into_iter()
    .map(|(host, port, timestamp)| TimestampedPeerInfo {
        host: host.into(),
        port,
        timestamp,
    })
    .collect();
    assert!(a.finish(claim, obs, None).await.unwrap());
    assert_eq!(b.counts().await.unwrap().known, 4);
    assert_eq!(b.totals(Role::Crawler).await.unwrap().gossip_rejected, 3);
    for endpoint in ["1.1.1.1:58444", "9.9.9.9:58444", "[2606:4700::1111]:58444"] {
        let peer = b.peer(endpoint.parse().unwrap()).await.unwrap().unwrap();
        assert_eq!(peer.first_source, "8.8.8.8:58444");
        assert_eq!(peer.last_gossiped_by.as_deref(), Some("8.8.8.8:58444"));
        assert!(peer.first_gossiped.is_some());
        assert!(peer.last_gossiped.is_some());
        assert_eq!(peer.reported_gossip_timestamp, None);
        assert_eq!(peer.attempts, 0);
    }
    assert_eq!(b.summary(86400).await.unwrap().total_nodes_5_days, 0);
    reset(&a, Role::Crawler).await;
    let referral = b.claim(Role::Crawler).await.unwrap().unwrap();
    assert_ne!(referral.endpoint, endpoint);
    assert_eq!(referral.endpoint.port(), 58444);
    cleanup(&a).await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL via DGX_TEST_POSTGRES_URL"]
async fn good_peak_is_reliable_even_when_peer_discovery_fails_and_introducers_are_excluded() {
    use dg_xch_servers::discovery::CrawlerPolicy;
    let (a, b) = setup().await;
    a.configure(
        Role::Crawler,
        Policy {
            crawler: Some(CrawlerPolicy {
                enable_ipv6: true,
                port: 8444,
                minimum_height: 240000,
                peak_wait_ms: 2500,
                tcp_connect_timeout_seconds: 2,
            }),
            ..policy()
        },
    )
    .await
    .unwrap();
    let endpoint: SocketAddr = "127.0.0.1:8444".parse().unwrap();
    a.seen(endpoint, "seed", None).await.unwrap();
    let claim = a.claim(Role::Crawler).await.unwrap().unwrap();
    let mut obs = success(&a.network);
    obs.peak_height = Some(300000);
    obs.peer_request_ms = None;
    obs.error = Some("peer request timed out".into());
    b.finish(claim, obs, None).await.unwrap();
    assert!(a.peer(endpoint).await.unwrap().unwrap().reliable);
    assert_eq!(a.totals(Role::Crawler).await.unwrap().peer_responses, 0);
    due(&a).await;
    let claim = a.claim(Role::Crawler).await.unwrap().unwrap();
    let mut obs = success(&a.network);
    obs.handshake.as_mut().unwrap().node_type = NodeType::Introducer as u8;
    obs.peak_height = None;
    b.finish(claim, obs, None).await.unwrap();
    assert_eq!(a.summary(5 * 86400).await.unwrap().reliable, 0);
    assert!(!a.peer(endpoint).await.unwrap().unwrap().reliable);
    cleanup(&a).await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL via DGX_TEST_POSTGRES_URL"]
async fn diagnostics_merge_error_variants_and_category_filters_match_every_endpoint() {
    let (a, b) = setup().await;
    let mut failures: Vec<(SocketAddr, Phase, String)> = (1..=14)
        .map(|n| {
            (
                format!("10.90.0.{n}:8444").parse().unwrap(),
                Phase::TcpConnect,
                format!("TCP connection timed out after {n} seconds"),
            )
        })
        .collect();
    failures.extend([
        (
            "[2606:4700::1111]:8444".parse().unwrap(),
            Phase::TcpConnect,
            "Connection timed out (os error 110)".into(),
        ),
        (
            "10.90.1.1:8444".parse().unwrap(),
            Phase::TcpConnect,
            "Network is unreachable (os error 101)".into(),
        ),
        (
            "10.90.1.2:8444".parse().unwrap(),
            Phase::TcpConnect,
            "No route to host (os error 113)".into(),
        ),
        (
            "10.90.1.3:8444".parse().unwrap(),
            Phase::Tls,
            "TLS handshake timed out".into(),
        ),
    ]);
    for (endpoint, phase, error) in &failures {
        a.seen(*endpoint, "fixture", None).await.unwrap();
        sqlx::query("UPDATE dg_discovery.jobs SET attempts=1,last_error=$4,last_error_phase=$5,last_error_at=1 WHERE network=$1 AND role='crawler' AND ip=$2::inet AND port=$3")
            .bind(&a.network).bind(endpoint.ip().to_string()).bind(i32::from(endpoint.port())).bind(error).bind(phase.as_str()).execute(&a.pool).await.unwrap();
    }
    let diagnostics = b.diagnostics().await.unwrap();
    assert_eq!(diagnostics.latest_failures, 18);
    assert_eq!(diagnostics.groups.len(), 3);
    assert_eq!(diagnostics.other_failures, 0);
    let tcp_timeouts = diagnostics
        .groups
        .iter()
        .find(|g| g.phase == Phase::TcpConnect && g.reason == "timeout")
        .unwrap();
    assert_eq!(
        (
            tcp_timeouts.count,
            tcp_timeouts.ipv4,
            tcp_timeouts.ipv6,
            tcp_timeouts.error_variants
        ),
        (15, 14, 1, 15)
    );
    let routing = diagnostics
        .groups
        .iter()
        .find(|g| g.reason == "routing")
        .unwrap();
    assert_eq!((routing.count, routing.error_variants), (2, 2));
    for group in &diagnostics.groups {
        let page = b
            .page(&Page {
                limit: 50,
                failure_phase: Some(group.phase),
                failure_reason: Some(group.reason.into()),
                status: Status::Failed,
                ..Page::default()
            })
            .await
            .unwrap();
        assert_eq!(page.total, group.count);
        assert_eq!(page.peers.len(), group.count);
        for peer in page.peers {
            assert_eq!(peer.last_error_phase, Some(group.phase));
            assert_eq!(
                dg_xch_servers::discovery::failure_reason(
                    group.phase,
                    peer.last_error.as_deref().unwrap()
                ),
                group.reason
            );
        }
    }
    let ipv6 = b
        .page(&Page {
            failure_phase: Some(Phase::TcpConnect),
            failure_reason: Some("timeout".into()),
            family: Family::Ipv6,
            ..Page::default()
        })
        .await
        .unwrap();
    assert_eq!(ipv6.total, 1);
    // Filtering only by reason includes both TCP and TLS timeout categories.
    assert_eq!(
        b.page(&Page {
            failure_reason: Some("timeout".into()),
            ..Page::default()
        })
        .await
        .unwrap()
        .total,
        16
    );
    cleanup(&a).await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL via DGX_TEST_POSTGRES_URL"]
async fn peak_wait_lease_covers_the_full_deadline_and_last_known_height_survives_failures() {
    use dg_xch_servers::discovery::CrawlerPolicy;
    let (a, b) = setup().await;
    a.configure(
        Role::Crawler,
        Policy {
            crawler: Some(CrawlerPolicy {
                enable_ipv6: true,
                port: 8444,
                minimum_height: 0,
                peak_wait_ms: 60000,
                tcp_connect_timeout_seconds: 2,
            }),
            ..policy()
        },
    )
    .await
    .unwrap();
    let endpoint: SocketAddr = "127.0.0.1:8444".parse().unwrap();
    a.seen(endpoint, "fixture", None).await.unwrap();
    let claim = b.claim(Role::Crawler).await.unwrap().unwrap();
    let lease: i64 = sqlx::query_scalar(
        "SELECT lease_until FROM dg_discovery.jobs WHERE network=$1 AND role='crawler'",
    )
    .bind(&a.network)
    .fetch_one(&a.pool)
    .await
    .unwrap();
    assert!(lease >= claim.started_at as i64 + policy().timeout_seconds as i64 + 3 + 60 + 15);
    assert_eq!(b.summary(86400).await.unwrap().peak_wait_ms, 60000);
    let mut observation = success(&a.network);
    observation.peak_height = Some(300000);
    a.finish(claim, observation, None).await.unwrap();
    due(&a).await;
    let claim = a.claim(Role::Crawler).await.unwrap().unwrap();
    let mut observation = success(&a.network);
    observation.tls_version = Some("TLSv1.2".into());
    observation.peak_height = Some(299999);
    b.finish(claim, observation, None).await.unwrap();
    let last = a.peer(endpoint).await.unwrap().unwrap();
    assert_eq!(
        last.peak_height,
        Some(299999),
        "Last Known Height must be the latest report, not the maximum ever seen"
    );
    let observed_at = last.peak_observed_at;
    assert_eq!(last.tls_version.as_deref(), Some("TLSv1.2"));
    due(&a).await;
    let claim = a.claim(Role::Crawler).await.unwrap().unwrap();
    let mut observation = success(&a.network);
    observation.tls_version = None;
    observation.error = Some("NewPeak wait timed out".into());
    observation.phase = Phase::Verification;
    b.finish(claim, observation, None).await.unwrap();
    let last = a.peer(endpoint).await.unwrap().unwrap();
    assert_eq!(last.peak_height, Some(299999));
    assert_eq!(last.peak_observed_at, observed_at);
    assert_eq!(last.last_error_phase, Some(Phase::Verification));
    assert_eq!(last.tls_version.as_deref(), Some("TLSv1.2"));
    let metrics = dg_xch_crawler::metrics::render(&a.summary(86400).await.unwrap()).unwrap();
    assert!(metrics.lines().any(|line| {
        line.starts_with("chia_crawler_latest_failed_phase{")
            && line.contains("phase=\"verification\"")
            && line.ends_with(" 1")
    }));
    cleanup(&a).await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL via DGX_TEST_POSTGRES_URL"]
async fn due_summary_matches_address_port_suspension_and_ip_cooldown_eligibility() {
    use dg_xch_servers::discovery::CrawlerPolicy;
    let (a, b) = setup().await;
    let restricted = Policy {
        crawler: Some(CrawlerPolicy {
            enable_ipv6: true,
            port: 8444,
            minimum_height: 0,
            peak_wait_ms: 100,
            tcp_connect_timeout_seconds: 2,
        }),
        allow_private: false,
        ..policy()
    };
    a.configure(Role::Crawler, restricted).await.unwrap();
    for endpoint in [
        "1.1.1.1:8444",
        "9.9.9.9:8444",
        "8.8.8.8:8444",
        "127.0.0.1:8444",
        "1.0.0.1:8445",
    ] {
        a.seen(endpoint.parse().unwrap(), "fixture", None)
            .await
            .unwrap();
    }
    let later = a.clock().await.unwrap() as i64 + 3600;
    sqlx::query(
        "UPDATE dg_discovery.jobs SET ban_until=$2 WHERE network=$1 AND ip='8.8.8.8'::inet",
    )
    .bind(&a.network)
    .bind(later)
    .execute(&a.pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO dg_discovery.cooldowns(network,role,ip,next_at) VALUES($1,'crawler','9.9.9.9'::inet,$2),($1,'introducer','1.1.1.1'::inet,$2)")
        .bind(&a.network).bind(later).execute(&a.pool).await.unwrap();
    assert_eq!(b.summary(86400).await.unwrap().due, 1);
    let claim = b.claim(Role::Crawler).await.unwrap().unwrap();
    assert_eq!(claim.endpoint, "1.1.1.1:8444".parse().unwrap());
    assert_eq!(a.summary(86400).await.unwrap().due, 0);
    a.configure(
        Role::Crawler,
        Policy {
            allow_private: true,
            ..restricted
        },
    )
    .await
    .unwrap();
    assert_eq!(a.summary(86400).await.unwrap().due, 1);
    cleanup(&a).await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL via DGX_TEST_POSTGRES_URL"]
async fn retention_catches_up_with_probe_rate_without_crossing_network_or_role() {
    let (a, _) = setup().await;
    a.configure(
        Role::Crawler,
        Policy {
            interval_ms: 10,
            ..policy()
        },
    )
    .await
    .unwrap();
    let other = Store::connect(&url(), format!("other-{}", a.network))
        .await
        .unwrap();
    let now = a.clock().await.unwrap() as i64;
    for (network, role, count, finished_at) in [
        (&a.network, "crawler", 6001, 1),
        (&a.network, "introducer", 3, 1),
        (&other.network, "crawler", 2, 1),
        (&a.network, "crawler", 10, now),
    ] {
        sqlx::query("INSERT INTO dg_discovery.observations(network,role,attempt_id,worker_id,ip,port,started_at,finished_at,successful,handshake_completed,peer_response,received_peers) SELECT $1,$2,md5($1||':'||$2||':'||($3::bigint)::text||':'||n::text)::uuid,$4,'127.0.0.1'::inet,8444,$3,$3,false,false,false,0 FROM generate_series(1,$5::integer) n")
            .bind(network).bind(role).bind(finished_at).bind(a.worker_id).bind(count).execute(&a.pool).await.unwrap();
    }
    a.maintenance(Role::Crawler, 90).await.unwrap();
    let old: i64 = sqlx::query_scalar("SELECT count(*) FROM dg_discovery.observations WHERE network=$1 AND role='crawler' AND finished_at=1")
        .bind(&a.network).fetch_one(&a.pool).await.unwrap();
    assert_eq!(
        old, 1,
        "one maintenance pass is bounded but keeps up with twice the maximum arrival rate"
    );
    for (network, role, expected) in [
        (&a.network, "crawler", 11i64),
        (&a.network, "introducer", 3),
        (&other.network, "crawler", 2),
    ] {
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM dg_discovery.observations WHERE network=$1 AND role=$2",
        )
        .bind(network)
        .bind(role)
        .fetch_one(&a.pool)
        .await
        .unwrap();
        assert_eq!(count, expected);
    }
    sqlx::query("UPDATE dg_discovery.budgets SET maintenance_after=now()-interval '1 second' WHERE network=$1 AND role='crawler'")
        .bind(&a.network).execute(&a.pool).await.unwrap();
    a.maintenance(Role::Crawler, 90).await.unwrap();
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM dg_discovery.observations WHERE network=$1 AND role='crawler'",
    )
    .bind(&a.network)
    .fetch_one(&a.pool)
    .await
    .unwrap();
    assert_eq!(count, 10);
    cleanup(&a).await;
    cleanup(&other).await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL via DGX_TEST_POSTGRES_URL"]
async fn invalid_page_cursors_are_rejected_before_accessing_postgres() {
    let (a, _) = setup().await;
    cleanup(&a).await;
    a.pool.close().await;
    let tokens = [
        "x".repeat(2049),
        "not-hex".into(),
        hex::encode(b"invalid-json"),
        hex::encode(
            serde_json::to_vec(&serde_json::json!({
                "network": "wrong-network", "sort": "last_connected", "order": "desc",
                "endpoint": "1.1.1.1:8444", "value": 1
            }))
            .unwrap(),
        ),
        hex::encode(
            serde_json::to_vec(&serde_json::json!({
                "network": a.network, "sort": "last_connected", "order": "desc",
                "endpoint": "1.1.1.1:8444", "value": -1
            }))
            .unwrap(),
        ),
    ];
    for token in tokens {
        let result = a
            .page(&Page {
                cursor: Some(token),
                ..Page::default()
            })
            .await;
        assert!(matches!(result, Err(error) if error.kind() == std::io::ErrorKind::InvalidInput));
    }
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL via DGX_TEST_POSTGRES_URL"]
async fn ipv6_disable_skips_existing_jobs_and_resumes_without_affecting_other_roles_or_networks() {
    use dg_xch_servers::discovery::CrawlerPolicy;
    let (a, b) = setup().await;
    let ipv6: SocketAddr = "[::1]:8444".parse().unwrap();
    a.seen(ipv6, "existing-catalog", None).await.unwrap();
    let disabled = CrawlerPolicy {
        enable_ipv6: false,
        port: 8444,
        minimum_height: 0,
        peak_wait_ms: 100,
        tcp_connect_timeout_seconds: 2,
    };
    a.configure(
        Role::Crawler,
        Policy {
            crawler: Some(disabled),
            ..policy()
        },
    )
    .await
    .unwrap();
    assert!(b.claim(Role::Crawler).await.unwrap().is_none());
    let summary = b.summary(86400).await.unwrap();
    assert!(!summary.enable_ipv6);
    let metrics = dg_xch_crawler::metrics::render(&summary).unwrap();
    assert!(
        metrics
            .lines()
            .any(|line| { line.starts_with("chia_crawler_ipv6_enabled{") && line.ends_with(" 0") })
    );
    assert_eq!(summary.due, 0);
    assert_eq!(summary.known, 1);
    assert_eq!(summary.ipv6_known, 1);
    assert_eq!(summary.totals.attempts, 0);
    let peer = b.peer(ipv6).await.unwrap().unwrap();
    assert_eq!(peer.attempts, 0);
    assert_eq!(peer.last_attempt, None);
    assert_eq!(peer.reliability.tries, 0);
    assert!(!peer.inflight);

    // The separate introducer verifier still accepts an IPv6 job on this network.
    a.enqueue_verification(ipv6).await.unwrap();
    let claim = b.claim(Role::Introducer).await.unwrap().unwrap();
    assert_eq!(claim.endpoint, ipv6);
    b.finish(claim, success(&a.network), Some(60))
        .await
        .unwrap();
    assert_eq!(a.totals(Role::Crawler).await.unwrap().attempts, 0);
    assert_eq!(a.peer(ipv6).await.unwrap().unwrap().attempts, 0);

    let other = Store::connect(&url(), format!("other-{}", a.network))
        .await
        .unwrap();
    let enabled = CrawlerPolicy {
        enable_ipv6: true,
        ..disabled
    };
    other
        .configure(
            Role::Crawler,
            Policy {
                crawler: Some(enabled),
                ..policy()
            },
        )
        .await
        .unwrap();
    other.seen(ipv6, "fixture", None).await.unwrap();
    assert!(other.summary(86400).await.unwrap().enable_ipv6);
    assert_eq!(
        other.claim(Role::Crawler).await.unwrap().unwrap().endpoint,
        ipv6
    );
    assert!(b.claim(Role::Crawler).await.unwrap().is_none());

    // Re-enabling uses the preserved queue and is immediately visible to replicas.
    a.configure(
        Role::Crawler,
        Policy {
            crawler: Some(enabled),
            ..policy()
        },
    )
    .await
    .unwrap();
    let summary = b.summary(86400).await.unwrap();
    assert!(summary.enable_ipv6);
    assert_eq!(summary.due, 1);
    let metrics = dg_xch_crawler::metrics::render(&summary).unwrap();
    assert!(
        metrics
            .lines()
            .any(|line| { line.starts_with("chia_crawler_ipv6_enabled{") && line.ends_with(" 1") })
    );
    let claim = b.claim(Role::Crawler).await.unwrap().unwrap();
    assert_eq!(claim.endpoint, ipv6);
    assert!(claim.policy.crawler.unwrap().enable_ipv6);
    assert_eq!(a.totals(Role::Crawler).await.unwrap().attempts, 1);
    cleanup(&a).await;
    cleanup(&other).await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL via DGX_TEST_POSTGRES_URL"]
async fn ipv6_gossip_and_failure_history_are_preserved_when_crawling_is_disabled() {
    use dg_xch_servers::discovery::CrawlerPolicy;
    let (a, b) = setup().await;
    let disabled = CrawlerPolicy {
        enable_ipv6: false,
        port: 8444,
        minimum_height: 0,
        peak_wait_ms: 100,
        tcp_connect_timeout_seconds: 2,
    };
    a.configure(
        Role::Crawler,
        Policy {
            crawler: Some(disabled),
            ..policy()
        },
    )
    .await
    .unwrap();
    a.seen("127.0.0.1:8444".parse().unwrap(), "seed", None)
        .await
        .unwrap();
    let claim = b.claim(Role::Crawler).await.unwrap().unwrap();
    assert!(!claim.policy.crawler.unwrap().enable_ipv6);
    let timestamp = a.clock().await.unwrap();
    let mut observation = success(&a.network);
    observation.peak_height = Some(1);
    observation.peers.push(TimestampedPeerInfo {
        host: "::1".into(),
        port: 8444,
        timestamp,
    });
    a.finish(claim, observation, None).await.unwrap();
    let ipv6: SocketAddr = "[::1]:8444".parse().unwrap();
    let peer = b.peer(ipv6).await.unwrap().unwrap();
    assert_eq!(peer.reported_gossip_timestamp, Some(timestamp));
    assert_eq!(peer.first_source, "127.0.0.1:8444");
    assert_eq!(peer.attempts, 0);
    assert_eq!(b.summary(86400).await.unwrap().due, 0);
    reset(&a, Role::Crawler).await;
    assert!(b.claim(Role::Crawler).await.unwrap().is_none());
    assert_eq!(b.totals(Role::Crawler).await.unwrap().attempts, 1);

    a.configure(
        Role::Crawler,
        Policy {
            crawler: Some(CrawlerPolicy {
                enable_ipv6: true,
                ..disabled
            }),
            ..policy()
        },
    )
    .await
    .unwrap();
    let claim = b.claim(Role::Crawler).await.unwrap().unwrap();
    assert_eq!(claim.endpoint, ipv6);
    a.finish(
        claim,
        Observation {
            error: Some("Network is unreachable (os error 101)".into()),
            phase: Phase::TcpConnect,
            ..Observation::default()
        },
        None,
    )
    .await
    .unwrap();
    assert_eq!(b.summary(86400).await.unwrap().pending_retry, 1);
    a.configure(
        Role::Crawler,
        Policy {
            crawler: Some(disabled),
            ..policy()
        },
    )
    .await
    .unwrap();
    let summary = b.summary(86400).await.unwrap();
    assert_eq!(summary.pending_retry, 0);
    assert_eq!(summary.failed_latest, 1);
    assert_eq!(summary.ipv6_attempted, 1);
    assert_eq!(summary.ipv6_known, 1);
    assert_eq!(summary.totals.attempts, 2);
    assert_eq!(summary.totals.failures, 1);
    assert_eq!(b.diagnostics().await.unwrap().groups[0].ipv6, 1);
    cleanup(&a).await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL via DGX_TEST_POSTGRES_URL"]
async fn five_day_gossip_counts_unique_ips_by_reported_age_and_preserves_history() {
    let (a, b) = setup().await;
    let now = a.clock().await.unwrap();
    for (endpoint, timestamp) in [
        ("127.10.0.1:8444", Some(now)),
        ("127.10.0.1:8445", Some(now - 60)),
        ("127.10.0.2:8444", Some(now - 6 * 86400)),
        ("[::1]:8444", Some(now)),
        ("127.10.0.4:8444", None),
        ("127.10.0.5:8444", Some(0)),
        ("[::2]:8444", Some(0)),
    ] {
        a.seen(endpoint.parse().unwrap(), "fixture", timestamp)
            .await
            .unwrap();
    }
    // A fresh handshake and a fresh local receipt of stale gossip are not recent advertisements.
    sqlx::query("UPDATE dg_discovery.peers SET first_success=$2,last_success=$2 WHERE network=$1 AND ip='127.10.0.4'::inet")
        .bind(&a.network).bind(now as i64).execute(&a.pool).await.unwrap();
    for recent_window in [60, 7 * 86400] {
        let summary = b.summary(recent_window).await.unwrap();
        assert_eq!(summary.known, 7);
        assert_eq!(
            (
                summary.total_nodes_5_days,
                summary.ipv4_nodes_5_days,
                summary.ipv6_nodes_5_days
            ),
            (2, 1, 1)
        );
    }
    let other = Store::connect(&url(), format!("other-{}", a.network))
        .await
        .unwrap();
    other.configure(Role::Crawler, policy()).await.unwrap();
    other
        .seen("127.11.0.9:8444".parse().unwrap(), "fixture", Some(now))
        .await
        .unwrap();
    assert_eq!(other.summary(86400).await.unwrap().total_nodes_5_days, 1);

    // Retry only if the wall clock crosses a second between fixture setup and summary.
    let mut boundary_summary = None;
    for _ in 0..10 {
        let now = a.clock().await.unwrap();
        sqlx::query("UPDATE dg_discovery.peers SET reported_gossip_timestamp=$2::bigint-432000-CASE WHEN family=6 THEN 1 ELSE 0 END WHERE network=$1 AND ip IN ('127.10.0.5'::inet,'::2'::inet)")
            .bind(&a.network).bind(now as i64).execute(&a.pool).await.unwrap();
        let summary = b.summary(60).await.unwrap();
        if summary.timestamp == now {
            boundary_summary = Some(summary);
            break;
        }
    }
    let summary = boundary_summary.expect("fixture and summary should complete within one second");
    assert_eq!(
        (
            summary.total_nodes_5_days,
            summary.ipv4_nodes_5_days,
            summary.ipv6_nodes_5_days
        ),
        (3, 2, 1)
    );
    a.sample(&summary).await.unwrap();
    b.sample(&summary).await.unwrap();
    sqlx::query("INSERT INTO dg_discovery.history(network,bucket,observed_at,known,recent,identities) VALUES($1,$2,$3,7,1,0)")
        .bind(&a.network).bind((summary.timestamp / 60) as i64 - 2).bind(summary.timestamp as i64 - 120).execute(&a.pool).await.unwrap();
    sqlx::query("UPDATE dg_discovery.peers SET reported_gossip_timestamp=0 WHERE network=$1")
        .bind(&a.network)
        .execute(&a.pool)
        .await
        .unwrap();
    let history = b.history().await.unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(
        (
            history[0].total_nodes_5_days,
            history[0].ipv4_nodes_5_days,
            history[0].ipv6_nodes_5_days
        ),
        (None, None, None)
    );
    assert_eq!(
        (
            history[1].total_nodes_5_days,
            history[1].ipv4_nodes_5_days,
            history[1].ipv6_nodes_5_days
        ),
        (Some(3), Some(2), Some(1))
    );
    assert!(other.history().await.unwrap().is_empty());
    cleanup(&a).await;
    cleanup(&other).await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL via DGX_TEST_POSTGRES_URL"]
async fn concurrent_gossip_keeps_one_endpoint_max_timestamp_and_only_the_latest_advertiser() {
    let (a, b) = setup().await;
    for endpoint in ["127.12.0.1:8444", "127.12.0.2:8444"] {
        a.seen(endpoint.parse().unwrap(), "seed", None)
            .await
            .unwrap();
    }
    let ca = a.claim(Role::Crawler).await.unwrap().unwrap();
    reset(&a, Role::Crawler).await;
    let cb = b.claim(Role::Crawler).await.unwrap().unwrap();
    let advertisers = [ca.endpoint, cb.endpoint];
    let now = a.clock().await.unwrap();
    let target: SocketAddr = "127.12.0.20:8444".parse().unwrap();
    let observation = |newest| {
        let mut obs = success(&a.network);
        obs.peers = [
            ("127.12.0.20", newest),
            ("127.12.0.20", newest - 60),
            ("2001:4860:4860::8888", newest),
            ("2001:4860:4860:0:0:0:0:8888", newest - 60),
        ]
        .into_iter()
        .map(|(host, timestamp)| TimestampedPeerInfo {
            host: host.into(),
            port: 8444,
            timestamp,
        })
        .collect();
        obs
    };
    let (ra, rb) = tokio::join!(
        a.finish(ca, observation(now), None),
        b.finish(cb, observation(now - 120), None)
    );
    assert!(ra.unwrap());
    assert!(rb.unwrap());
    let initial = a.peer(target).await.unwrap().unwrap();
    assert_eq!(initial.reported_gossip_timestamp, Some(now));
    let initial_source = initial.last_gossiped_by.as_deref().unwrap();
    assert!(
        advertisers
            .iter()
            .any(|source| source.to_string() == initial_source)
    );
    assert_eq!(a.counts().await.unwrap().known, 4);
    let ipv6 = a
        .peer("[2001:4860:4860::8888]:8444".parse().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ipv6.reported_gossip_timestamp, Some(now));

    let latest = *advertisers
        .iter()
        .find(|source| source.to_string() != initial_source)
        .unwrap();
    sqlx::query("UPDATE dg_discovery.jobs SET next_attempt=CASE WHEN ip=$2::inet THEN 0 ELSE $3 END WHERE network=$1 AND role='crawler'")
        .bind(&a.network).bind(latest.ip().to_string()).bind(now as i64 + 86400).execute(&a.pool).await.unwrap();
    reset(&a, Role::Crawler).await;
    let claim = b.claim(Role::Crawler).await.unwrap().unwrap();
    assert_eq!(claim.endpoint, latest);
    a.finish(claim, observation(now - 3600), None)
        .await
        .unwrap();
    let peer = b.peer(target).await.unwrap().unwrap();
    assert_eq!(peer.last_gossiped_by, Some(latest.to_string()));
    assert_eq!(peer.reported_gossip_timestamp, Some(now));
    assert_eq!(peer.first_source, initial.first_source);
    assert_eq!(peer.first_seen, initial.first_seen);

    // Seed refresh and a direct verification probe must not masquerade as advertisements.
    a.seen(target, "seed-refresh", None).await.unwrap();
    a.enqueue_verification(target).await.unwrap();
    let claim = b.claim(Role::Introducer).await.unwrap().unwrap();
    assert_eq!(claim.endpoint, target);
    a.finish(claim, success(&a.network), Some(60))
        .await
        .unwrap();
    assert_eq!(
        b.peer(target).await.unwrap().unwrap().last_gossiped_by,
        Some(latest.to_string())
    );

    // A later timestamp-free introducer referral updates provenance without erasing remote age.
    let introducer = *advertisers
        .iter()
        .find(|endpoint| **endpoint != latest)
        .unwrap();
    sqlx::query("UPDATE dg_discovery.jobs SET next_attempt=CASE WHEN ip=$2::inet THEN 0 ELSE $3 END WHERE network=$1 AND role='crawler'")
        .bind(&a.network).bind(introducer.ip().to_string()).bind(now as i64 + 86400).execute(&a.pool).await.unwrap();
    reset(&a, Role::Crawler).await;
    let claim = b.claim(Role::Crawler).await.unwrap().unwrap();
    assert_eq!(claim.endpoint, introducer);
    let mut referral = success(&a.network);
    referral.handshake.as_mut().unwrap().node_type = NodeType::Introducer as u8;
    referral.peers = vec![TimestampedPeerInfo {
        host: target.ip().to_string(),
        port: target.port(),
        timestamp: 0,
    }];
    a.finish(claim, referral, None).await.unwrap();
    let peer = b.peer(target).await.unwrap().unwrap();
    assert_eq!(peer.last_gossiped_by, Some(introducer.to_string()));
    assert_eq!(peer.reported_gossip_timestamp, Some(now));
    assert!(peer.last_gossiped >= initial.last_gossiped);
    assert_eq!(b.counts().await.unwrap().known, 4);
    let summary = b.summary(86400).await.unwrap();
    assert_eq!(
        (
            summary.total_nodes_5_days,
            summary.ipv4_nodes_5_days,
            summary.ipv6_nodes_5_days
        ),
        (2, 1, 1)
    );
    cleanup(&a).await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL via DGX_TEST_POSTGRES_URL"]
async fn v7_migration_keeps_existing_rows_and_marks_unknown_gossip_history_as_null() {
    let (a, _) = setup().await;
    // All SQL below interpolates only this generated identifier (fixed prefix + UUID hex).
    let schema = format!("migration_{}", uuid::Uuid::new_v4().simple());
    let mut tx = a.pool.begin().await.unwrap();
    for migration in [
        include_str!("../../servers/src/discovery.sql"),
        include_str!("../../servers/src/discovery_v2.sql"),
        include_str!("../../servers/src/discovery_v3.sql"),
        include_str!("../../servers/src/discovery_v4.sql"),
        include_str!("../../servers/src/discovery_v5.sql"),
        include_str!("../../servers/src/discovery_v6.sql"),
    ] {
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            migration.replace("dg_discovery", &schema),
        ))
        .execute(&mut *tx)
        .await
        .unwrap();
    }
    sqlx::query(sqlx::AssertSqlSafe(format!("INSERT INTO {schema}.peers(network,ip,port,publicly_routable,first_seen,last_seen,first_source,first_gossiped,last_gossiped,reported_gossip_timestamp) VALUES('mainnet','1.1.1.1'::inet,8444,true,1,3,'9.9.9.9:8444',2,3,123)")))
        .execute(&mut *tx).await.unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!("INSERT INTO {schema}.history(network,bucket,observed_at,known,recent,identities) VALUES('mainnet',1,60,1,1,1)")))
        .execute(&mut *tx).await.unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        include_str!("../../servers/src/discovery_v7.sql").replace("dg_discovery", &schema),
    ))
    .execute(&mut *tx)
    .await
    .unwrap();
    let row: (Option<String>, String, String) = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT last_gossiped_by,first_source,reported_gossip_timestamp::text FROM {schema}.peers"
    )))
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    assert_eq!(row, (None, "9.9.9.9:8444".into(), "123".into()));
    let row: (i64, Option<i64>, Option<i64>, Option<i64>) = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT known,total_nodes_5_days,ipv4_nodes_5_days,ipv6_nodes_5_days FROM {schema}.history"
    )))
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    assert_eq!(row, (1, None, None, None));
    let marker: String = sqlx::query_scalar(
        "SELECT obj_description(oid,'pg_namespace') FROM pg_namespace WHERE nspname=$1",
    )
    .bind(&schema)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    assert_eq!(marker, "dgx-discovery-typed-v7");
    tx.rollback().await.unwrap();
    let replica = Store::connect(&url(), a.network.clone()).await.unwrap();
    assert_eq!(replica.counts().await.unwrap().known, 0);
    cleanup(&a).await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL via DGX_TEST_POSTGRES_URL"]
async fn observatory_subnet_budgets_and_slow_pool_are_shared_and_fenced() {
    use dg_xch_servers::discovery::CrawlControls;
    let (a, b) = setup().await;
    a.configure(
        Role::Crawler,
        Policy {
            concurrency: 3,
            interval_ms: 1,
            ..policy()
        },
    )
    .await
    .unwrap();
    a.configure_controls(&CrawlControls {
        subnet_interval_ms: 0,
        subnet_concurrency: 1,
        verification_concurrency: 1,
        ..Default::default()
    })
    .await
    .unwrap();
    for ip in [
        "8.8.8.1:8444",
        "8.8.8.2:8444",
        "9.9.9.1:8444",
        "1.1.1.1:8444",
    ] {
        a.seen(ip.parse().unwrap(), "seed", None).await.unwrap();
    }
    let first = a.claim(Role::Crawler).await.unwrap().unwrap();
    reset(&a, Role::Crawler).await;
    let second = b.claim(Role::Crawler).await.unwrap().unwrap();
    reset(&a, Role::Crawler).await;
    assert!(
        a.claim(Role::Crawler).await.unwrap().is_none(),
        "fast pool must leave room for verification"
    );
    assert!(a.reserve_verification(&first).await.unwrap());
    assert!(!b.reserve_verification(&second).await.unwrap());
    let third = b.claim(Role::Crawler).await.unwrap().unwrap();
    reset(&a, Role::Crawler).await;
    assert!(a.claim(Role::Crawler).await.unwrap().is_none());
    assert!(
        a.finish(first.clone(), success(&a.network), None)
            .await
            .unwrap()
    );
    assert!(
        !b.reserve_verification(&first).await.unwrap(),
        "completed lease cannot enter slow pool"
    );
    assert!(b.reserve_verification(&second).await.unwrap());
    a.finish(third, success(&a.network), None).await.unwrap();
    cleanup(&a).await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL via DGX_TEST_POSTGRES_URL"]
async fn ipv6_revisit_honors_longer_operator_delay() {
    let (a, _) = setup().await;
    let mut p = policy();
    p.crawler = Some(dg_xch_servers::discovery::CrawlerPolicy {
        enable_ipv6: true,
        port: 8444,
        minimum_height: 0,
        peak_wait_ms: 100,
        tcp_connect_timeout_seconds: 2,
    });
    a.configure(Role::Crawler, p).await.unwrap();
    let endpoint = "[::1]:8444".parse().unwrap();
    a.seen(endpoint, "seed", None).await.unwrap();
    let claim = a.claim(Role::Crawler).await.unwrap().unwrap();
    let started = claim.started_at;
    let mut obs = success(&a.network);
    obs.peak_height = Some(42);
    a.finish(claim, obs, None).await.unwrap();
    let peer = a.peer(endpoint).await.unwrap().unwrap();
    assert!(peer.next_attempt >= started + 3600);
    cleanup(&a).await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL via DGX_TEST_POSTGRES_URL"]
async fn observatory_history_geography_and_adaptive_pacing_preserve_unknowns() {
    use dg_xch_servers::discovery::CrawlControls;
    let (a, b) = setup().await;
    a.configure_controls(&CrawlControls::default())
        .await
        .unwrap();
    let endpoint = "8.8.8.8:8444".parse().unwrap();
    a.seen(endpoint, "seed", None).await.unwrap();
    let claim = a.claim(Role::Crawler).await.unwrap().unwrap();
    let mut obs = success(&a.network);
    obs.peak_height = Some(100);
    a.finish(claim, obs, None).await.unwrap();
    let now = a.clock().await.unwrap() as i64;
    sqlx::query("INSERT INTO dg_discovery.geography(network,ip,country,asn,organization,database_epoch,updated_at) VALUES($1,'8.8.8.8','US',15169,'Example',1,$2)").bind(&a.network).bind(now).execute(&a.pool).await.unwrap();
    a.observatory_maintenance(86400).await.unwrap();
    b.observatory_maintenance(86400).await.unwrap();
    let d = a.insights(86400, 7, Some(110)).await.unwrap();
    assert_eq!(d["distribution"]["countries"][0]["label"], "US");
    assert_eq!(d["distribution"]["asns"][0]["asn"], 15169);
    assert_eq!(d["peer_height_lag_p95"], 10.0);
    assert_eq!(d["history"].as_array().unwrap().len(), 1);
    let summary = a.summary(86400).await.unwrap();
    a.sample(&summary).await.unwrap();
    let mut old = summary.clone();
    old.timestamp = (now - 2 * 86400) as u64;
    a.sample(&old).await.unwrap();
    let history = a.history_range(7).await.unwrap();
    assert_eq!(history.len(), 2);
    assert!(!history[0].activity_observed);
    assert!(history[1].activity_observed);
    // A sharp rise in refusals relative to baseline slows all replicas.
    sqlx::query("UPDATE dg_discovery.budgets SET adaptive_baseline=0.1 WHERE network=$1")
        .bind(&a.network)
        .execute(&a.pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM dg_discovery.distribution_history WHERE network=$1")
        .bind(&a.network)
        .execute(&a.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO dg_discovery.observations(network,role,attempt_id,worker_id,ip,port,started_at,finished_at,successful,handshake_completed,peer_response,error,error_phase,received_peers) SELECT $1,'crawler',gen_random_uuid(),gen_random_uuid(),'8.8.4.4',8444,$2,$2,false,false,false,'connection refused','tcp_connect',0 FROM generate_series(1,60)").bind(&a.network).bind(now).execute(&a.pool).await.unwrap();
    a.observatory_maintenance(86400).await.unwrap();
    assert_eq!(
        b.insights(86400, 1, None).await.unwrap()["adaptive_multiplier"],
        2
    );
    cleanup(&a).await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL via DGX_TEST_POSTGRES_URL"]
async fn v9_upgrade_preserves_catalog_leases_and_adds_observatory_tables() {
    let pool = sqlx::PgPool::connect(&url()).await.unwrap();
    // All SQL below interpolates only this generated identifier (fixed prefix + UUID hex).
    let schema = format!("upgrade_{}", uuid::Uuid::new_v4().simple());
    for migration in [
        include_str!("../../servers/src/discovery.sql"),
        include_str!("../../servers/src/discovery_v2.sql"),
        include_str!("../../servers/src/discovery_v3.sql"),
        include_str!("../../servers/src/discovery_v4.sql"),
        include_str!("../../servers/src/discovery_v5.sql"),
        include_str!("../../servers/src/discovery_v6.sql"),
        include_str!("../../servers/src/discovery_v7.sql"),
        include_str!("../../servers/src/discovery_v8.sql"),
    ] {
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            migration.replace("dg_discovery", &schema),
        ))
        .execute(&pool)
        .await
        .unwrap();
    }
    sqlx::query(sqlx::AssertSqlSafe(format!("INSERT INTO {schema}.peers(network,ip,port,publicly_routable,first_seen,last_seen,first_source) VALUES('mainnet','8.8.8.8',8444,true,1,2,'fixture')"))).execute(&pool).await.unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!("INSERT INTO {schema}.jobs(network,role,ip,port,next_attempt,lease_token,lease_until) VALUES('mainnet','crawler','8.8.8.8',8444,100,gen_random_uuid(),100)"))).execute(&pool).await.unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        include_str!("../../servers/src/discovery_v9.sql").replace("dg_discovery", &schema),
    ))
    .execute(&pool)
    .await
    .unwrap();
    use sqlx::Row;
    let row=sqlx::query(sqlx::AssertSqlSafe(format!("SELECT subnet::text AS subnet,verifying,lease_token IS NOT NULL AS leased FROM {schema}.jobs"))).fetch_one(&pool).await.unwrap();
    assert_eq!(row.get::<String, _>("subnet"), "8.8.8.0/24");
    assert!(!row.get::<bool, _>("verifying"));
    assert!(row.get::<bool, _>("leased"));
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE")))
        .execute(&pool)
        .await
        .unwrap();
}
