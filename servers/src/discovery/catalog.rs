use super::*;
use serde::{Deserialize, Serialize};
use sqlx::{Postgres, QueryBuilder};
#[derive(Serialize, Deserialize)]
struct Cursor {
    network: String,
    sort: Sort,
    order: Direction,
    endpoint: SocketAddr,
    value: serde_json::Value,
}
impl Sort {
    fn column(self) -> &'static str {
        match self {
            Self::Endpoint => "p.ip",
            Self::Version => "p.software_version",
            Self::FirstSeen => "p.first_seen",
            Self::LastConnected => "p.last_success",
            Self::Ping => "p.peer_request_ms",
            Self::LastAttempt => "j.last_attempt",
            Self::NextAttempt => "j.next_attempt",
            Self::Failures => "j.consecutive_failures",
        }
    }
    fn value(self, p: &Peer) -> serde_json::Value {
        match self {
            Self::Endpoint => serde_json::Value::Null,
            Self::Version => serde_json::json!(p.software_version),
            Self::FirstSeen => serde_json::json!(p.first_seen),
            Self::LastConnected => serde_json::json!(p.last_success),
            Self::Ping => serde_json::json!(p.peer_request_ms),
            Self::LastAttempt => serde_json::json!(p.last_attempt),
            Self::NextAttempt => serde_json::json!(p.next_attempt),
            Self::Failures => serde_json::json!(p.consecutive_failures),
        }
    }
}
fn filters(q: &mut QueryBuilder<'_, Postgres>, network: &str, page: &Page) {
    q.push(" WHERE p.network=").push_bind(network.to_owned());
    match page.status {
        Status::All => {}
        Status::Unattempted => {
            q.push(" AND coalesce(j.attempts,0)=0");
        }
        Status::Failed => {
            q.push(" AND j.last_error IS NOT NULL");
        }
        Status::Connected => {
            q.push(" AND p.first_success IS NOT NULL");
        }
    }
    match page.family {
        Family::All => {}
        Family::Ipv4 => {
            q.push(" AND p.family=4");
        }
        Family::Ipv6 => {
            q.push(" AND p.family=6");
        }
    }
    let search: String = page.search.trim().chars().take(128).collect();
    if !search.is_empty() {
        if let Ok(endpoint) = search.parse::<SocketAddr>() {
            q.push(" AND p.ip=")
                .push_bind(endpoint.ip().to_string())
                .push("::inet AND p.port=")
                .push_bind(i32::from(endpoint.port()));
        } else if let Ok(ip) = search.parse::<std::net::IpAddr>() {
            q.push(" AND p.ip=")
                .push_bind(ip.to_string())
                .push("::inet");
        } else if search.len() == 64 && hex::decode(&search).is_ok() {
            q.push(" AND p.node_id=")
                .push_bind(hex::decode(search).unwrap_or_default());
        } else {
            let term = format!(
                "%{}%",
                search
                    .to_lowercase()
                    .replace('\\', "\\\\")
                    .replace('%', "\\%")
                    .replace('_', "\\_")
            );
            q.push(" AND (lower(p.endpoint) LIKE ")
                .push_bind(term.clone())
                .push(" OR lower(p.software_version) LIKE ")
                .push_bind(term.clone())
                .push(" OR lower(j.last_error) LIKE ")
                .push_bind(term)
                .push(")");
        }
    }
}
fn cursor_value(q: &mut QueryBuilder<'_, Postgres>, cursor: &Cursor) -> Result<(), Error> {
    match cursor.sort {
        Sort::Version => {
            q.push_bind(
                cursor
                    .value
                    .as_str()
                    .ok_or_else(|| {
                        Error::new(std::io::ErrorKind::InvalidInput, "invalid version cursor")
                    })?
                    .to_owned(),
            );
        }
        Sort::Ping => {
            let value = cursor
                .value
                .as_f64()
                .filter(|v| v.is_finite() && *v >= 0.0)
                .ok_or_else(|| {
                    Error::new(std::io::ErrorKind::InvalidInput, "invalid latency cursor")
                })?;
            q.push_bind(value);
        }
        _ => {
            let value = cursor
                .value
                .as_u64()
                .and_then(|v| i64::try_from(v).ok())
                .ok_or_else(|| {
                    Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "invalid timestamp/counter cursor",
                    )
                })?;
            q.push_bind(value);
        }
    };
    Ok(())
}
impl Store {
    pub async fn peer(&self, endpoint: SocketAddr) -> Result<Option<Peer>, Error> {
        let row = sqlx::query(&format!(
            "SELECT {PEER_COLUMNS}{PEER_FROM} WHERE p.network=$1 AND p.ip=$2::inet AND p.port=$3"
        ))
        .bind(&self.network)
        .bind(endpoint.ip().to_string())
        .bind(i32::from(endpoint.port()))
        .fetch_optional(&self.pool)
        .await
        .map_err(Error::other)?;
        row.map(read_peer).transpose()
    }
    pub async fn page(&self, page: &Page) -> Result<PeerPage, Error> {
        let mut count = QueryBuilder::new(format!("SELECT count(*){PEER_FROM}"));
        filters(&mut count, &self.network, page);
        let total: i64 = count
            .build_query_scalar()
            .fetch_one(&self.pool)
            .await
            .map_err(Error::other)?;
        let mut q = QueryBuilder::new(format!("SELECT {PEER_COLUMNS}{PEER_FROM}"));
        filters(&mut q, &self.network, page);
        if let Some(token) = &page.cursor {
            if token.len() > 2048 {
                return Err(Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "cursor too long",
                ));
            }
            let cursor: Cursor = hex::decode(token)
                .ok()
                .and_then(|v| serde_json::from_slice(&v).ok())
                .ok_or_else(|| {
                    Error::new(std::io::ErrorKind::InvalidInput, "invalid page cursor")
                })?;
            if cursor.network != self.network
                || cursor.sort != page.sort
                || cursor.order != page.order
            {
                return Err(Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "cursor does not match sort order",
                ));
            }
            let comparison = if page.order == Direction::Desc {
                "<"
            } else {
                ">"
            };
            q.push(" AND (");
            if page.sort == Sort::Endpoint {
                q.push("(p.ip,p.port)")
                    .push(comparison)
                    .push("(")
                    .push_bind(cursor.endpoint.ip().to_string())
                    .push("::inet,")
                    .push_bind(i32::from(cursor.endpoint.port()))
                    .push(")");
            } else {
                let column = page.sort.column();
                if cursor.value.is_null() {
                    q.push(column).push(" IS NULL AND ");
                } else {
                    q.push(column).push(comparison);
                    cursor_value(&mut q, &cursor)?;
                    q.push(" OR ")
                        .push(column)
                        .push(" IS NULL OR (")
                        .push(column)
                        .push("=");
                    cursor_value(&mut q, &cursor)?;
                    q.push(" AND ");
                }
                q.push("(p.ip,p.port)>(")
                    .push_bind(cursor.endpoint.ip().to_string())
                    .push("::inet,")
                    .push_bind(i32::from(cursor.endpoint.port()))
                    .push(")");
                if !cursor.value.is_null() {
                    q.push(")");
                }
            }
            q.push(")");
        }
        let direction = if page.order == Direction::Desc {
            " DESC"
        } else {
            " ASC"
        };
        if page.sort == Sort::Endpoint {
            q.push(" ORDER BY p.ip")
                .push(direction)
                .push(",p.port")
                .push(direction);
        } else {
            q.push(" ORDER BY ")
                .push(page.sort.column())
                .push(direction)
                .push(" NULLS LAST,p.ip,p.port");
        }
        let limit = page.limit.clamp(1, 200);
        q.push(" LIMIT ").push_bind((limit + 1) as i64);
        let mut peers = q
            .build()
            .fetch_all(&self.pool)
            .await
            .map_err(Error::other)?
            .into_iter()
            .map(read_peer)
            .collect::<Result<Vec<_>, _>>()?;
        let more = peers.len() > limit;
        peers.truncate(limit);
        let next_cursor = if more {
            peers
                .last()
                .map(|p| {
                    serde_json::to_vec(&Cursor {
                        network: self.network.clone(),
                        sort: page.sort,
                        order: page.order,
                        endpoint: p.endpoint,
                        value: page.sort.value(p),
                    })
                    .map(hex::encode)
                })
                .transpose()
                .map_err(Error::other)?
        } else {
            None
        };
        Ok(PeerPage {
            total: total as usize,
            peers,
            next_cursor,
        })
    }
    pub async fn totals(&self, role: Role) -> Result<Totals, Error> {
        let row=sqlx::query("SELECT coalesce(sum(attempts),0)::bigint AS attempts,coalesce(sum(connections),0)::bigint AS connections,coalesce(sum(peer_responses),0)::bigint AS peer_responses,coalesce(sum(failures),0)::bigint AS failures,coalesce(sum(gossip_received),0)::bigint AS gossip_received,coalesce(sum(gossip_rejected),0)::bigint AS gossip_rejected,coalesce(sum(pruned),0)::bigint AS pruned,coalesce(sum(verification_expired),0)::bigint AS verification_expired,coalesce(sum(connect_seconds_sum),0)::float8 AS connect_seconds_sum,coalesce(sum(request_seconds_sum),0)::float8 AS request_seconds_sum FROM dg_discovery.counters WHERE network=$1 AND role=$2")
            .bind(&self.network).bind(role.as_str()).fetch_one(&self.pool).await.map_err(Error::other)?;
        Ok(Totals {
            attempts: row.get::<i64, _>("attempts") as u64,
            connections: row.get::<i64, _>("connections") as u64,
            peer_responses: row.get::<i64, _>("peer_responses") as u64,
            failures: row.get::<i64, _>("failures") as u64,
            gossip_received: row.get::<i64, _>("gossip_received") as u64,
            gossip_rejected: row.get::<i64, _>("gossip_rejected") as u64,
            pruned: row.get::<i64, _>("pruned") as u64,
            verification_expired: row.get::<i64, _>("verification_expired") as u64,
            connect_seconds_sum: row.get("connect_seconds_sum"),
            request_seconds_sum: row.get("request_seconds_sum"),
        })
    }
    pub async fn summary(&self, recent_seconds: u64) -> Result<Summary, Error> {
        let now = self.clock().await? as i64;
        let cutoff = now - recent_seconds as i64;
        let row=sqlx::query(&format!("SELECT count(*) AS known,count(*) FILTER(WHERE p.first_success IS NOT NULL) AS ever_connected,count(*) FILTER(WHERE p.last_success>=$2) AS recent,count(DISTINCT p.node_id) FILTER(WHERE p.last_success>=$2) AS identities,count(*) FILTER(WHERE j.next_attempt<=$3 AND (j.lease_until IS NULL OR j.lease_until<=$3)) AS due,count(*) FILTER(WHERE j.lease_until>$3) AS inflight,count(*) FILTER(WHERE p.family=4 AND p.last_success>=$2) AS ipv4_recent,count(*) FILTER(WHERE p.family=6 AND p.last_success>=$2) AS ipv6_recent,count(*) FILTER(WHERE j.attempts>0) AS attempted,count(*) FILTER(WHERE j.last_error IS NOT NULL) AS failed_latest,count(*) FILTER(WHERE j.last_error IS NOT NULL AND j.next_attempt>$3 AND (j.lease_until IS NULL OR j.lease_until<=$3)) AS pending_retry,count(*) FILTER(WHERE p.family=4) AS ipv4_known,count(*) FILTER(WHERE p.family=6) AS ipv6_known,count(*) FILTER(WHERE p.family=6 AND j.attempts>0) AS ipv6_attempted,avg(p.connect_ms) FILTER(WHERE p.last_success>=$2) AS connect_ms,avg(p.peer_request_ms) FILTER(WHERE p.last_peer_response>=$2) AS peer_request_ms{PEER_FROM} WHERE p.network=$1"))
            .bind(&self.network).bind(cutoff).bind(now).fetch_one(&self.pool).await.map_err(Error::other)?;
        let n = |name| row.get::<i64, _>(name) as usize;
        let mut errors = std::collections::BTreeMap::new();
        for r in sqlx::query("SELECT last_error_phase,count(*) AS count FROM dg_discovery.jobs WHERE network=$1 AND role='crawler' AND last_error IS NOT NULL GROUP BY last_error_phase")
            .bind(&self.network).fetch_all(&self.pool).await.map_err(Error::other)?{errors.insert(r.get("last_error_phase"),r.get::<i64,_>("count") as usize);}
        let versions = self.distribution("software_version", cutoff).await?;
        let protocols = self.distribution("protocol_version", cutoff).await?;
        let budget=sqlx::query("SELECT concurrency,interval_ms,timeout_seconds FROM dg_discovery.budgets WHERE network=$1 AND role='crawler'").bind(&self.network).fetch_one(&self.pool).await.map_err(Error::other)?;
        Ok(Summary {
            timestamp: now as u64,
            network: self.network.clone(),
            known: n("known"),
            ever_connected: n("ever_connected"),
            recent: n("recent"),
            recent_window_seconds: recent_seconds,
            unique_recent_identities: n("identities"),
            due: n("due"),
            inflight: n("inflight"),
            ipv4_recent: n("ipv4_recent"),
            ipv6_recent: n("ipv6_recent"),
            versions: versions.clone(),
            protocols: protocols.clone(),
            other_versions: n("recent").saturating_sub(versions.values().sum()),
            other_protocols: n("recent").saturating_sub(protocols.values().sum()),
            average_connect_ms: row.get("connect_ms"),
            average_peer_request_ms: row.get("peer_request_ms"),
            totals: self.totals(Role::Crawler).await?,
            storage_error: None,
            seed_error: None,
            attempted: n("attempted"),
            never_attempted: n("known") - n("attempted"),
            failed_latest: n("failed_latest"),
            pending_retry: n("pending_retry"),
            error_phases: errors,
            ipv4_known: n("ipv4_known"),
            ipv6_known: n("ipv6_known"),
            ipv6_attempted: n("ipv6_attempted"),
            configured_concurrency: budget.get::<i32, _>("concurrency") as usize,
            interval_ms: budget.get::<i64, _>("interval_ms") as u64,
            timeout_seconds: budget.get::<i32, _>("timeout_seconds") as u64,
        })
    }
    async fn distribution(
        &self,
        column: &str,
        cutoff: i64,
    ) -> Result<std::collections::BTreeMap<String, usize>, Error> {
        // Column is a private, fixed call-site identifier, never user input.
        let rows=sqlx::query(&format!("SELECT coalesce({column},'') AS label,count(*) AS count FROM dg_discovery.peers WHERE network=$1 AND last_success>=$2 GROUP BY {column} ORDER BY count(*) DESC,label LIMIT 64"))
            .bind(&self.network).bind(cutoff).fetch_all(&self.pool).await.map_err(Error::other)?;
        Ok(rows
            .into_iter()
            .map(|r| (r.get("label"), r.get::<i64, _>("count") as usize))
            .collect())
    }
    pub async fn sample(&self, summary: &Summary) -> Result<(), Error> {
        sqlx::query("INSERT INTO dg_discovery.history(network,bucket,observed_at,known,recent,identities) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT DO NOTHING")
            .bind(&self.network).bind((summary.timestamp/60) as i64).bind(summary.timestamp as i64).bind(summary.known as i64).bind(summary.recent as i64).bind(summary.unique_recent_identities as i64).execute(&self.pool).await.map_err(Error::other)?;
        Ok(())
    }
    pub async fn history(&self) -> Result<Vec<Point>, Error> {
        let rows=sqlx::query("SELECT observed_at,known,recent,identities FROM dg_discovery.history WHERE network=$1 ORDER BY bucket DESC LIMIT 1440").bind(&self.network).fetch_all(&self.pool).await.map_err(Error::other)?;
        Ok(rows
            .into_iter()
            .rev()
            .map(|r| Point {
                timestamp: r.get::<i64, _>("observed_at") as u64,
                known: r.get::<i64, _>("known") as usize,
                recent: r.get::<i64, _>("recent") as usize,
                identities: r.get::<i64, _>("identities") as usize,
            })
            .collect())
    }
    pub async fn counts(&self) -> Result<Counts, Error> {
        let now = self.clock().await? as i64;
        let row=sqlx::query("SELECT count(*) AS known,count(*) FILTER(WHERE verified_until>$2) AS verified,count(*) FILTER(WHERE verified_until>$2 AND family=4) AS ipv4,count(*) FILTER(WHERE verified_until>$2 AND family=6) AS ipv6 FROM dg_discovery.peers WHERE network=$1").bind(&self.network).bind(now).fetch_one(&self.pool).await.map_err(Error::other)?;
        let cooldowns=sqlx::query_scalar("SELECT count(*) FROM dg_discovery.cooldowns WHERE network=$1 AND role='introducer' AND next_at>$2").bind(&self.network).bind(now).fetch_one(&self.pool).await.map_err(Error::other)?;
        let pending_verification=sqlx::query_scalar("SELECT count(*) FROM dg_discovery.jobs j JOIN dg_discovery.peers p ON p.network=j.network AND p.ip=j.ip AND p.port=j.port WHERE j.network=$1 AND j.role='introducer' AND (p.verified_until IS NULL OR p.verified_until<=$2)").bind(&self.network).bind(now).fetch_one(&self.pool).await.map_err(Error::other)?;
        Ok(Counts {
            known: row.get("known"),
            verified: row.get("verified"),
            ipv4: row.get("ipv4"),
            ipv6: row.get("ipv6"),
            cooldowns,
            pending_verification,
        })
    }
    pub async fn response(
        &self,
        requester: SocketAddr,
        allow_private: bool,
    ) -> Result<Vec<(SocketAddr, u64)>, Error> {
        let mut cursor = self.cursor.lock().await;
        let mut result = Vec::new();
        let now = self.clock().await? as i64;
        for wrap in [false, true] {
            let mut q = QueryBuilder::new(
                "SELECT endpoint,verified_at FROM dg_discovery.peers WHERE network=",
            );
            q.push_bind(self.network.clone())
                .push(" AND verified_until>")
                .push_bind(now)
                .push(" AND (")
                .push_bind(allow_private)
                .push(" OR publicly_routable) AND NOT (ip=")
                .push_bind(requester.ip().to_string())
                .push("::inet AND port=")
                .push_bind(i32::from(requester.port()))
                .push(")");
            if let Some(pivot) = *cursor {
                q.push(" AND (ip,port)")
                    .push(if wrap { "<=" } else { ">" })
                    .push("(")
                    .push_bind(pivot.ip().to_string())
                    .push("::inet,")
                    .push_bind(i32::from(pivot.port()))
                    .push(")");
            } else if wrap {
                break;
            }
            q.push(" ORDER BY ip,port LIMIT ")
                .push_bind((100 - result.len()) as i64);
            for r in q
                .build()
                .fetch_all(&self.pool)
                .await
                .map_err(Error::other)?
            {
                result.push((
                    r.get::<String, _>("endpoint")
                        .parse()
                        .map_err(Error::other)?,
                    r.get::<i64, _>("verified_at") as u64,
                ));
            }
            if result.len() == 100 {
                break;
            }
        }
        if let Some((endpoint, _)) = result.last() {
            *cursor = Some(*endpoint);
        }
        Ok(result)
    }
}
