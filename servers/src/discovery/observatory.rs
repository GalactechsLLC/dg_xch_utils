use super::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CrawlControls {
    pub subnet_interval_ms: u64,
    pub subnet_concurrency: u32,
    pub verification_concurrency: u32,
    pub adaptive: bool,
    pub history_days: u32,
}
impl Default for CrawlControls {
    fn default() -> Self {
        Self {
            subnet_interval_ms: 1000,
            subnet_concurrency: 2,
            verification_concurrency: 25,
            adaptive: true,
            history_days: 90,
        }
    }
}
impl CrawlControls {
    pub fn validate(&self) -> Result<(), Error> {
        if self.subnet_interval_ms > 60000
            || !(1..=256).contains(&self.subnet_concurrency)
            || self.verification_concurrency > 64
            || !(1..=365).contains(&self.history_days)
        {
            return Err(Error::other("invalid crawler safety/history controls"));
        }
        Ok(())
    }
}
impl Store {
    pub async fn configure_controls(&self, c: &CrawlControls) -> Result<(), Error> {
        c.validate()?;
        sqlx::query("UPDATE dg_discovery.budgets SET subnet_interval_ms=$2,subnet_concurrency=$3,verification_concurrency=$4,adaptive_enabled=$5,history_days=$6,adaptive_multiplier=CASE WHEN $5 THEN adaptive_multiplier ELSE 1 END WHERE network=$1 AND role='crawler'")
            .bind(&self.network).bind(c.subnet_interval_ms as i64).bind(c.subnet_concurrency as i32).bind(c.verification_concurrency as i32).bind(c.adaptive).bind(c.history_days as i32).execute(&self.pool).await.map_err(Error::other)?;
        Ok(())
    }
    /// Transfer a live discovery lease into the small, globally bounded slow-peak pool.
    pub async fn reserve_verification(&self, claim: &Claim) -> Result<bool, Error> {
        let mut tx = self.pool.begin().await.map_err(Error::other)?;
        let limit: i32 = sqlx::query_scalar("SELECT least(verification_concurrency,concurrency-1) FROM dg_discovery.budgets WHERE network=$1 AND role='crawler' FOR UPDATE")
            .bind(&self.network).fetch_one(&mut *tx).await.map_err(Error::other)?;
        let active: i64 = sqlx::query_scalar("SELECT count(*) FROM dg_discovery.jobs WHERE network=$1 AND role='crawler' AND verifying AND lease_until>extract(epoch FROM clock_timestamp())")
            .bind(&self.network).fetch_one(&mut *tx).await.map_err(Error::other)?;
        if active >= i64::from(limit) {
            return Ok(false);
        }
        let updated = sqlx::query("UPDATE dg_discovery.jobs SET verifying=true WHERE network=$1 AND role='crawler' AND ip=$2::inet AND port=$3 AND lease_token=$4 AND lease_until>extract(epoch FROM clock_timestamp()) AND NOT verifying")
            .bind(&self.network).bind(claim.endpoint.ip().to_string()).bind(i32::from(claim.endpoint.port())).bind(claim.token).execute(&mut *tx).await.map_err(Error::other)?.rows_affected() == 1;
        tx.commit().await.map_err(Error::other)?;
        Ok(updated)
    }
    pub async fn history_days(&self) -> Result<i32, Error> {
        sqlx::query_scalar(
            "SELECT history_days FROM dg_discovery.budgets WHERE network=$1 AND role='crawler'",
        )
        .bind(&self.network)
        .fetch_one(&self.pool)
        .await
        .map_err(Error::other)
    }
    pub async fn observatory_maintenance(&self, recent: u64) -> Result<(), Error> {
        // One controller decision per minute, independent of the number of replicas.
        let mut tx = self.pool.begin().await.map_err(Error::other)?;
        let acquired: bool =
            sqlx::query_scalar("SELECT pg_try_advisory_xact_lock(hashtextextended($1,73921))")
                .bind(&self.network)
                .fetch_one(&mut *tx)
                .await
                .map_err(Error::other)?;
        if !acquired {
            return Ok(());
        }
        let now = self.clock().await? as i64;
        let inserted = sqlx::query("INSERT INTO dg_discovery.distribution_history(network,bucket,data) VALUES($1,$2,'{}') ON CONFLICT DO NOTHING")
            .bind(&self.network).bind(now/60).execute(&mut *tx).await.map_err(Error::other)?.rows_affected();
        if inserted == 0 {
            return Ok(());
        }
        let r=sqlx::query("SELECT count(*) AS n,count(*) FILTER(WHERE error_phase='tcp_connect' AND (lower(error) LIKE '%refused%' OR lower(error) LIKE '%timed out%')) AS remote_failures FROM dg_discovery.observations WHERE network=$1 AND role='crawler' AND finished_at>=$2")
            .bind(&self.network).bind(now-300).fetch_one(&mut *tx).await.map_err(Error::other)?;
        let n = r.get::<i64, _>("n");
        let failures = r.get::<i64, _>("remote_failures");
        // A degraded path slows discovery; recovery is deliberately gradual.
        if n >= 50 {
            sqlx::query("UPDATE dg_discovery.budgets SET adaptive_multiplier=CASE WHEN NOT adaptive_enabled THEN 1 WHEN adaptive_baseline IS NOT NULL AND $2>=0.6 AND $2>adaptive_baseline+0.15 THEN least(16,adaptive_multiplier*2) WHEN adaptive_baseline IS NOT NULL AND $2<=adaptive_baseline+0.05 THEN greatest(1,adaptive_multiplier-1) ELSE adaptive_multiplier END,adaptive_baseline=coalesce(adaptive_baseline*0.95+$2*0.05,$2) WHERE network=$1 AND role='crawler'")
                .bind(&self.network).bind(failures as f64/n as f64).execute(&mut *tx).await.map_err(Error::other)?;
        }
        sqlx::query("INSERT INTO dg_discovery.probe_history(network,bucket,completed,handshakes,responses,failures,connect_ms,request_ms) SELECT network,finished_at/60,count(*),count(*) FILTER(WHERE handshake_completed),count(*) FILTER(WHERE peer_response),count(*) FILTER(WHERE NOT successful),avg(connect_ms),avg(peer_request_ms) FROM dg_discovery.observations WHERE network=$1 AND role='crawler' AND finished_at>=$2 GROUP BY network,finished_at/60 ON CONFLICT(network,bucket) DO UPDATE SET completed=excluded.completed,handshakes=excluded.handshakes,responses=excluded.responses,failures=excluded.failures,connect_ms=excluded.connect_ms,request_ms=excluded.request_ms")
            .bind(&self.network).bind(now-180).execute(&mut *tx).await.map_err(Error::other)?;
        sqlx::query("DELETE FROM dg_discovery.probe_history WHERE network=$1 AND bucket<$2")
            .bind(&self.network)
            .bind((now - i64::from(self.history_days().await?) * 86400) / 60)
            .execute(&mut *tx)
            .await
            .map_err(Error::other)?;
        let distribution = self.distributions(recent).await?;
        sqlx::query(
            "UPDATE dg_discovery.distribution_history SET data=$3 WHERE network=$1 AND bucket=$2",
        )
        .bind(&self.network)
        .bind(now / 60)
        .bind(distribution)
        .execute(&mut *tx)
        .await
        .map_err(Error::other)?;
        // Preserve hourly snapshots after one day. Current minute data remains useful for rollouts.
        sqlx::query("DELETE FROM dg_discovery.distribution_history WHERE network=$1 AND (bucket<$2 OR (bucket<$3 AND bucket%60<>0))")
            .bind(&self.network).bind((now-i64::from(self.history_days().await?)*86400)/60).bind((now-86400)/60).execute(&mut *tx).await.map_err(Error::other)?;
        sqlx::query("DELETE FROM dg_discovery.subnet_cooldowns WHERE network=$1 AND next_at<clock_timestamp()-interval '1 day'")
            .bind(&self.network).execute(&mut *tx).await.map_err(Error::other)?;
        sqlx::query("DELETE FROM dg_discovery.geography g WHERE g.network=$1 AND NOT EXISTS(SELECT 1 FROM dg_discovery.peers p WHERE p.network=g.network AND p.ip=g.ip)")
            .bind(&self.network).execute(&mut *tx).await.map_err(Error::other)?;
        tx.commit().await.map_err(Error::other)
    }
    pub async fn distributions(&self, recent: u64) -> Result<Value, Error> {
        let cutoff = self.clock().await?.saturating_sub(recent) as i64;
        let versions=sqlx::query("SELECT coalesce(software_version,'unknown') AS label,count(*) AS count FROM dg_discovery.peers WHERE network=$1 AND last_success>=$2 GROUP BY label ORDER BY count DESC,label LIMIT 32")
            .bind(&self.network).bind(cutoff).fetch_all(&self.pool).await.map_err(Error::other)?;
        let countries=sqlx::query("SELECT coalesce(g.country,'unknown') AS label,count(DISTINCT p.ip) AS count FROM dg_discovery.peers p LEFT JOIN dg_discovery.geography g ON g.network=p.network AND g.ip=p.ip WHERE p.network=$1 AND p.last_success>=$2 GROUP BY label ORDER BY count DESC,label")
            .bind(&self.network).bind(cutoff).fetch_all(&self.pool).await.map_err(Error::other)?;
        let asns=sqlx::query("SELECT g.asn,coalesce(g.organization,'unknown') AS organization,count(DISTINCT p.ip) AS count FROM dg_discovery.peers p LEFT JOIN dg_discovery.geography g ON g.network=p.network AND g.ip=p.ip WHERE p.network=$1 AND p.last_success>=$2 GROUP BY g.asn,g.organization ORDER BY count DESC,g.asn LIMIT 100")
            .bind(&self.network).bind(cutoff).fetch_all(&self.pool).await.map_err(Error::other)?;
        let pairs = |rows: Vec<PgRow>| {
            rows.into_iter()
                .map(
                    |r| json!({"label":r.get::<String,_>("label"),"count":r.get::<i64,_>("count")}),
                )
                .collect::<Vec<_>>()
        };
        Ok(
            json!({"versions":pairs(versions),"countries":pairs(countries),"asns":asns.into_iter().map(|r|json!({"asn":r.get::<Option<i64>,_>("asn"),"organization":r.get::<String,_>("organization"),"count":r.get::<i64,_>("count")})).collect::<Vec<_>>(),"recent_seconds":recent}),
        )
    }
    pub async fn insights(
        &self,
        recent: u64,
        days: u32,
        trusted_height: Option<u32>,
    ) -> Result<Value, Error> {
        let now = self.clock().await? as i64;
        let r=sqlx::query("SELECT count(*) AS completed,percentile_cont(0.5) WITHIN GROUP(ORDER BY connect_ms) AS connect_p50,percentile_cont(0.95) WITHIN GROUP(ORDER BY connect_ms) AS connect_p95,percentile_cont(0.95) WITHIN GROUP(ORDER BY peer_request_ms) AS request_p95,count(*) FILTER(WHERE handshake_completed) AS handshakes FROM dg_discovery.observations WHERE network=$1 AND role='crawler' AND finished_at>=$2")
            .bind(&self.network).bind(now-300).fetch_one(&self.pool).await.map_err(Error::other)?;
        let p=sqlx::query("SELECT count(*) FILTER(WHERE first_seen>=$2) AS discovered,count(*) FILTER(WHERE first_seen>=$2 AND first_source NOT LIKE 'seed:%' AND first_source<>'introducer' AND first_gossiped IS NOT NULL) AS new_gossip,count(*) FILTER(WHERE last_success>=$2) AS fresh,count(*) FILTER(WHERE peak_observed_at>=$2 AND peak_height<=$3) AS height_samples,percentile_cont(0.95) WITHIN GROUP(ORDER BY $3-peak_height) FILTER(WHERE peak_observed_at>=$2 AND peak_height<=$3) AS lag_p95 FROM dg_discovery.peers WHERE network=$1")
            .bind(&self.network).bind(now-300).bind(trusted_height.map(i64::from)).fetch_one(&self.pool).await.map_err(Error::other)?;
        let b=sqlx::query("SELECT adaptive_multiplier,subnet_interval_ms,subnet_concurrency,verification_concurrency FROM dg_discovery.budgets WHERE network=$1 AND role='crawler'")
            .bind(&self.network).fetch_one(&self.pool).await.map_err(Error::other)?;
        let q=sqlx::query("SELECT max($2-j.next_attempt) FILTER(WHERE j.next_attempt<$2 AND j.ignore_until<=$2 AND j.ban_until<=$2 AND (j.lease_until IS NULL OR j.lease_until<=$2)) AS oldest_due FROM dg_discovery.jobs j JOIN dg_discovery.peers p USING(network,ip,port) JOIN dg_discovery.budgets b ON b.network=j.network AND b.role=j.role WHERE j.network=$1 AND j.role='crawler' AND (b.allow_private OR p.publicly_routable) AND (b.crawler_enable_ipv6 OR p.family=4) AND (b.crawler_port IS NULL OR p.port=b.crawler_port)")
            .bind(&self.network).bind(now).fetch_one(&self.pool).await.map_err(Error::other)?;
        let rows=sqlx::query("SELECT DISTINCT ON (bucket/60) bucket,data FROM dg_discovery.distribution_history WHERE network=$1 AND bucket>=$2 ORDER BY bucket/60,bucket DESC")
            .bind(&self.network).bind((now-i64::from(days.clamp(1,90))*86400)/60).fetch_all(&self.pool).await.map_err(Error::other)?;
        let completed = r.get::<i64, _>("completed");
        Ok(
            json!({"timestamp":now,"window_seconds":300,"completed":completed,"probes_per_second":completed as f64/300.,"handshakes":r.get::<i64,_>("handshakes"),"connect_p50_ms":r.get::<Option<f64>,_>("connect_p50"),"connect_p95_ms":r.get::<Option<f64>,_>("connect_p95"),"request_p95_ms":r.get::<Option<f64>,_>("request_p95"),"new_endpoints":p.get::<i64,_>("discovered"),"new_gossip_endpoints_per_probe":(completed>0).then(||p.get::<i64,_>("new_gossip") as f64/completed as f64),"fresh_endpoints":p.get::<i64,_>("fresh"),"peer_height_lag_p95":p.get::<Option<f64>,_>("lag_p95"),"height_samples":p.get::<i64,_>("height_samples"),"trusted_height":trusted_height,"oldest_due_seconds":q.get::<Option<i64>,_>("oldest_due"),"adaptive_multiplier":b.get::<i32,_>("adaptive_multiplier"),"subnet_interval_ms":b.get::<i64,_>("subnet_interval_ms"),"subnet_concurrency":b.get::<i32,_>("subnet_concurrency"),"verification_concurrency":b.get::<i32,_>("verification_concurrency"),"distribution":self.distributions(recent).await?,"history":rows.into_iter().map(|r|json!({"timestamp":r.get::<i64,_>("bucket")*60,"versions":r.get::<Value,_>("data")["versions"]})).collect::<Vec<_>>()}),
        )
    }
}
