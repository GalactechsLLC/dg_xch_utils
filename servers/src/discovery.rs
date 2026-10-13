//! PostgreSQL discovery storage. All data access is scoped to one immutable network.
#[path = "discovery/catalog.rs"]
mod catalog;
#[path = "discovery/models.rs"]
mod models;
#[path = "discovery/work.rs"]
mod work;
pub use models::*;
pub use sqlx;
use sqlx::{
    PgPool, Row,
    postgres::{PgPoolOptions, PgRow},
};
use std::{io::Error, net::SocketAddr, sync::Arc, time::Duration};
use tokio::sync::Mutex;

#[derive(Clone)]
pub struct Store {
    pub pool: PgPool,
    pub network: String,
    pub worker_id: uuid::Uuid,
    cursor: Arc<Mutex<Option<SocketAddr>>>,
}
impl Store {
    pub async fn connect(url: &str, network: String) -> Result<Self, Error> {
        if network.is_empty() || network.len() > 64 {
            return Err(Error::other("invalid discovery network"));
        }
        let pool = PgPoolOptions::new()
            .max_connections(8)
            .acquire_timeout(Duration::from_secs(5))
            .after_connect(|conn, _| {
                Box::pin(async move {
                    sqlx::query("SET statement_timeout = '5s'")
                        .execute(conn)
                        .await?;
                    Ok(())
                })
            })
            .connect(url)
            .await
            .map_err(|_| Error::other("cannot connect to discovery PostgreSQL backend"))?;
        let mut tx = pool.begin().await.map_err(Error::other)?;
        sqlx::query("SELECT pg_advisory_xact_lock(734829103)")
            .execute(&mut *tx)
            .await
            .map_err(Error::other)?;
        let marker:Option<String>=sqlx::query_scalar("SELECT obj_description(oid,'pg_namespace') FROM pg_namespace WHERE nspname='dg_discovery'")
            .fetch_optional(&mut *tx).await.map_err(Error::other)?.flatten();
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM pg_namespace WHERE nspname='dg_discovery')",
        )
        .fetch_one(&mut *tx)
        .await
        .map_err(Error::other)?;
        if exists && marker.as_deref() != Some("dgx-discovery-first-release-typed-v1") {
            return Err(Error::other(
                "incompatible discovery schema; reset the discovery database before starting this first-release build",
            ));
        }
        if !exists {
            sqlx::raw_sql(include_str!("discovery.sql"))
                .execute(&mut *tx)
                .await
                .map_err(Error::other)?;
        }
        tx.commit().await.map_err(Error::other)?;
        Ok(Self {
            pool,
            network,
            worker_id: uuid::Uuid::new_v4(),
            cursor: Arc::new(Mutex::new(None)),
        })
    }
    pub async fn healthy(&self) -> bool {
        sqlx::query("SELECT 1").execute(&self.pool).await.is_ok()
    }
    pub async fn clock(&self) -> Result<u64, Error> {
        let now: i64 =
            sqlx::query_scalar("SELECT floor(extract(epoch FROM clock_timestamp()))::bigint")
                .fetch_one(&self.pool)
                .await
                .map_err(Error::other)?;
        Ok(now as u64)
    }
    pub async fn configure(&self, role: Role, policy: Policy) -> Result<(), Error> {
        sqlx::query("INSERT INTO dg_discovery.budgets(network,role,concurrency,interval_ms,timeout_seconds,revisit_seconds,cooldown_seconds,allow_private) VALUES($1,$2,$3,$4,$5,$6,$7,$8) ON CONFLICT(network,role) DO UPDATE SET concurrency=excluded.concurrency,interval_ms=excluded.interval_ms,timeout_seconds=excluded.timeout_seconds,revisit_seconds=excluded.revisit_seconds,cooldown_seconds=excluded.cooldown_seconds,allow_private=excluded.allow_private")
            .bind(&self.network).bind(role.as_str()).bind(policy.concurrency as i32).bind(policy.interval_ms as i64).bind(policy.timeout_seconds as i32).bind(policy.revisit_seconds as i64).bind(policy.cooldown_seconds as i32).bind(policy.allow_private).execute(&self.pool).await.map_err(Error::other)?;
        Ok(())
    }
    pub async fn seen(
        &self,
        endpoint: SocketAddr,
        source: &str,
        gossip: Option<u64>,
    ) -> Result<(), Error> {
        let mut tx = self.pool.begin().await.map_err(Error::other)?;
        self.seen_batch(&mut tx, &[(endpoint, gossip)], source)
            .await?;
        tx.commit().await.map_err(Error::other)?;
        Ok(())
    }
    pub(super) async fn seen_batch(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        peers: &[(SocketAddr, Option<u64>)],
        source: &str,
    ) -> Result<(), Error> {
        if peers.is_empty() {
            return Ok(());
        }
        let mut sorted = std::collections::BTreeMap::new();
        for &(endpoint, gossip) in peers {
            sorted.insert(endpoint, gossip);
        }
        let ips: Vec<_> = sorted.keys().map(|p| p.ip().to_string()).collect();
        let ports: Vec<_> = sorted.keys().map(|p| i32::from(p.port())).collect();
        let timestamps: Vec<_> = sorted.values().map(|p| p.map(|v| v.to_string())).collect();
        let public: Vec<_> = sorted.keys().map(|p| admissible(*p, false)).collect();
        sqlx::query("WITH input AS (SELECT ip::inet AS ip,port,reported::numeric AS reported,public FROM unnest($2::text[],$3::integer[],$4::text[],$5::boolean[]) AS x(ip,port,reported,public)), seen AS (INSERT INTO dg_discovery.peers(network,ip,port,publicly_routable,first_seen,last_seen,first_source,first_gossiped,last_gossiped,reported_gossip_timestamp) SELECT $1,ip,port,public,floor(extract(epoch FROM clock_timestamp()))::bigint,floor(extract(epoch FROM clock_timestamp()))::bigint,$6,CASE WHEN reported IS NOT NULL THEN floor(extract(epoch FROM clock_timestamp()))::bigint END,CASE WHEN reported IS NOT NULL THEN floor(extract(epoch FROM clock_timestamp()))::bigint END,reported FROM input ORDER BY ip,port ON CONFLICT(network,ip,port) DO UPDATE SET last_seen=greatest(dg_discovery.peers.last_seen,excluded.last_seen),first_gossiped=coalesce(dg_discovery.peers.first_gossiped,excluded.first_gossiped),last_gossiped=greatest(dg_discovery.peers.last_gossiped,excluded.last_gossiped),reported_gossip_timestamp=coalesce(excluded.reported_gossip_timestamp,dg_discovery.peers.reported_gossip_timestamp) RETURNING network,ip,port) INSERT INTO dg_discovery.jobs(network,role,ip,port,next_attempt) SELECT network,'crawler',ip,port,floor(extract(epoch FROM clock_timestamp()))::bigint FROM seen ORDER BY ip,port ON CONFLICT DO NOTHING")
            .bind(&self.network).bind(ips).bind(ports).bind(timestamps).bind(public).bind(source.chars().take(100).collect::<String>()).execute(&mut **tx).await.map_err(Error::other)?;
        Ok(())
    }
    pub async fn enqueue_verification(&self, endpoint: SocketAddr) -> Result<(), Error> {
        self.seen(endpoint, "introducer", None).await?;
        sqlx::query("INSERT INTO dg_discovery.jobs(network,role,ip,port,next_attempt) VALUES($1,'introducer',$2::inet,$3,floor(extract(epoch FROM clock_timestamp()))::bigint) ON CONFLICT DO NOTHING")
            .bind(&self.network).bind(endpoint.ip().to_string()).bind(i32::from(endpoint.port())).execute(&self.pool).await.map_err(Error::other)?;
        Ok(())
    }
}
const PEER_COLUMNS: &str = "p.endpoint,p.first_seen,p.last_seen,p.first_source,p.first_gossiped,p.last_gossiped,p.reported_gossip_timestamp::text AS reported,p.first_success,p.last_success,encode(p.node_id,'hex') AS node_id,p.software_version,p.protocol_version,p.node_type,p.advertised_port,p.capabilities,p.connect_ms,p.peer_request_ms,p.last_peer_response,p.peak_height,p.peak_observed_at,p.verified_at,p.verified_until,j.last_attempt,coalesce(j.next_attempt,p.first_seen) AS next_attempt,coalesce(j.attempts,0) AS attempts,coalesce(j.successes,0) AS successes,coalesce(j.consecutive_failures,0) AS consecutive_failures,j.last_error,j.last_error_phase,j.last_error_at";
const PEER_FROM: &str = " FROM dg_discovery.peers p LEFT JOIN dg_discovery.jobs j ON j.network=p.network AND j.ip=p.ip AND j.port=p.port AND j.role='crawler' ";
fn read_peer(row: PgRow) -> Result<Peer, Error> {
    let opt_time = |name| -> Result<Option<u64>, Error> {
        Ok(row
            .try_get::<Option<i64>, _>(name)
            .map_err(Error::other)?
            .map(|v| v as u64))
    };
    Ok(Peer {
        endpoint: row
            .try_get::<String, _>("endpoint")
            .map_err(Error::other)?
            .parse()
            .map_err(Error::other)?,
        first_seen: row.try_get::<i64, _>("first_seen").map_err(Error::other)? as u64,
        last_seen: row.try_get::<i64, _>("last_seen").map_err(Error::other)? as u64,
        first_source: row.try_get("first_source").map_err(Error::other)?,
        first_gossiped: opt_time("first_gossiped")?,
        last_gossiped: opt_time("last_gossiped")?,
        reported_gossip_timestamp: row
            .try_get::<Option<String>, _>("reported")
            .map_err(Error::other)?
            .map(|v| v.parse())
            .transpose()
            .map_err(Error::other)?,
        first_success: opt_time("first_success")?,
        last_success: opt_time("last_success")?,
        node_id: row.try_get("node_id").map_err(Error::other)?,
        software_version: row.try_get("software_version").map_err(Error::other)?,
        protocol_version: row.try_get("protocol_version").map_err(Error::other)?,
        node_type: row
            .try_get::<Option<i16>, _>("node_type")
            .map_err(Error::other)?
            .map(|v| v as u8),
        advertised_port: row
            .try_get::<Option<i32>, _>("advertised_port")
            .map_err(Error::other)?
            .map(|v| v as u16),
        capabilities: serde_json::from_value(row.try_get("capabilities").map_err(Error::other)?)
            .map_err(Error::other)?,
        connect_ms: row.try_get("connect_ms").map_err(Error::other)?,
        peer_request_ms: row.try_get("peer_request_ms").map_err(Error::other)?,
        last_peer_response: opt_time("last_peer_response")?,
        peak_height: row
            .try_get::<Option<i64>, _>("peak_height")
            .map_err(Error::other)?
            .map(|v| v as u32),
        peak_observed_at: opt_time("peak_observed_at")?,
        verified_at: opt_time("verified_at")?,
        verified_until: opt_time("verified_until")?,
        last_attempt: opt_time("last_attempt")?,
        next_attempt: row
            .try_get::<i64, _>("next_attempt")
            .map_err(Error::other)? as u64,
        attempts: row.try_get::<i64, _>("attempts").map_err(Error::other)? as u64,
        successes: row.try_get::<i64, _>("successes").map_err(Error::other)? as u64,
        consecutive_failures: row
            .try_get::<i32, _>("consecutive_failures")
            .map_err(Error::other)? as u32,
        last_error: row.try_get("last_error").map_err(Error::other)?,
        last_error_phase: row
            .try_get::<Option<String>, _>("last_error_phase")
            .map_err(Error::other)?
            .map(|v| Phase::parse(&v))
            .transpose()?,
        last_error_at: opt_time("last_error_at")?,
    })
}
