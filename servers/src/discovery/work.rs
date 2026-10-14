use super::*;
impl Store {
    pub async fn claim(&self, role: Role) -> Result<Option<Claim>, Error> {
        let mut tx = self.pool.begin().await.map_err(Error::other)?;
        let Some(budget)=sqlx::query("SELECT concurrency,interval_ms,timeout_seconds,revisit_seconds,cooldown_seconds,allow_private,crawler_port,minimum_height,peak_wait_ms,tcp_connect_timeout_seconds,crawler_enable_ipv6,next_start<=clock_timestamp() AS ready FROM dg_discovery.budgets WHERE network=$1 AND role=$2 FOR UPDATE")
            .bind(&self.network).bind(role.as_str()).fetch_optional(&mut *tx).await.map_err(Error::other)? else{return Err(Error::other("discovery role has no configured network budget"));};
        if !budget.get::<bool, _>("ready") {
            return Ok(None);
        }
        let active:i64=sqlx::query_scalar("SELECT count(*) FROM dg_discovery.jobs WHERE network=$1 AND role=$2 AND lease_token IS NOT NULL AND lease_until>floor(extract(epoch FROM clock_timestamp()))::bigint")
            .bind(&self.network).bind(role.as_str()).fetch_one(&mut *tx).await.map_err(Error::other)?;
        let policy = Policy {
            crawler: budget
                .get::<Option<i32>, _>("crawler_port")
                .map(|port| CrawlerPolicy {
                    enable_ipv6: budget.get("crawler_enable_ipv6"),
                    port: port as u16,
                    minimum_height: budget.get::<i64, _>("minimum_height") as u32,
                    peak_wait_ms: budget.get::<i64, _>("peak_wait_ms") as u64,
                    tcp_connect_timeout_seconds: budget.get::<i32, _>("tcp_connect_timeout_seconds")
                        as u64,
                }),
            concurrency: budget.get::<i32, _>("concurrency") as usize,
            interval_ms: budget.get::<i64, _>("interval_ms") as u64,
            timeout_seconds: budget.get::<i32, _>("timeout_seconds") as u64,
            revisit_seconds: budget.get::<i64, _>("revisit_seconds") as u64,
            cooldown_seconds: budget.get::<i32, _>("cooldown_seconds") as u64,
            allow_private: budget.get("allow_private"),
        };
        if active >= policy.concurrency as i64 {
            return Ok(None);
        }
        let now: i64 =
            sqlx::query_scalar("SELECT floor(extract(epoch FROM clock_timestamp()))::bigint")
                .fetch_one(&mut *tx)
                .await
                .map_err(Error::other)?;
        let Some(row)=sqlx::query("SELECT host(j.ip) AS ip,j.port,j.attempts,j.consecutive_failures FROM dg_discovery.jobs j JOIN dg_discovery.peers p ON p.network=j.network AND p.ip=j.ip AND p.port=j.port WHERE j.network=$1 AND j.role=$2 AND j.next_attempt<=$3 AND (j.lease_until IS NULL OR j.lease_until<=$3) AND ($4 OR p.publicly_routable) AND ($6 OR p.family=4) AND ($5::integer IS NULL OR (j.port=$5 AND j.ignore_until<=$3 AND j.ban_until<=$3)) AND NOT EXISTS(SELECT 1 FROM dg_discovery.cooldowns c WHERE c.network=j.network AND c.role=j.role AND c.ip=j.ip AND c.next_at>$3) ORDER BY j.next_attempt,j.ip,j.port LIMIT 1 FOR UPDATE OF p SKIP LOCKED")
            .bind(&self.network).bind(role.as_str()).bind(now).bind(policy.allow_private).bind(policy.crawler.map(|c| i32::from(c.port))).bind(role != Role::Crawler || policy.crawler.is_none_or(|c| c.enable_ipv6)).fetch_optional(&mut *tx).await.map_err(Error::other)? else{return Ok(None);};
        let endpoint = SocketAddr::new(
            row.get::<String, _>("ip").parse().map_err(Error::other)?,
            row.get::<i32, _>("port") as u16,
        );
        let token = uuid::Uuid::new_v4();
        let peak_wait_seconds = policy
            .crawler
            .map_or(0, |c| 3 + c.peak_wait_ms.div_ceil(1000));
        let until = now + (policy.timeout_seconds + peak_wait_seconds + 15) as i64;
        sqlx::query("UPDATE dg_discovery.jobs SET lease_token=$5,lease_until=$6,next_attempt=$6,last_attempt=$7,attempts=attempts+1 WHERE network=$1 AND role=$2 AND ip=$3::inet AND port=$4")
            .bind(&self.network).bind(role.as_str()).bind(endpoint.ip().to_string()).bind(i32::from(endpoint.port())).bind(token).bind(until).bind(now).execute(&mut *tx).await.map_err(Error::other)?;
        sqlx::query("INSERT INTO dg_discovery.cooldowns(network,role,ip,next_at) VALUES($1,$2,$3::inet,$4) ON CONFLICT(network,role,ip) DO UPDATE SET next_at=excluded.next_at")
            .bind(&self.network).bind(role.as_str()).bind(endpoint.ip().to_string()).bind(now+if policy.crawler.is_some() { if endpoint.is_ipv6() { policy.revisit_seconds.min(600) } else { policy.revisit_seconds } } else { policy.cooldown_seconds } as i64).execute(&mut *tx).await.map_err(Error::other)?;
        sqlx::query("UPDATE dg_discovery.budgets SET next_start=clock_timestamp()+make_interval(secs=>$3) WHERE network=$1 AND role=$2")
            .bind(&self.network).bind(role.as_str()).bind(policy.interval_ms as f64/1000.0).execute(&mut *tx).await.map_err(Error::other)?;
        sqlx::query("INSERT INTO dg_discovery.counters(network,role,worker_id,attempts) VALUES($1,$2,$3,1) ON CONFLICT(network,role,worker_id) DO UPDATE SET attempts=dg_discovery.counters.attempts+1")
            .bind(&self.network).bind(role.as_str()).bind(self.worker_id).execute(&mut *tx).await.map_err(Error::other)?;
        tx.commit().await.map_err(Error::other)?;
        Ok(Some(Claim {
            endpoint,
            token,
            role,
            policy,
            failures: row.get::<i32, _>("consecutive_failures") as u32,
            attempt: row.get::<i64, _>("attempts") as u64 + 1,
            started_at: now as u64,
        }))
    }
    /// Returns false for a stale/duplicate completion. No network work happens inside this transaction.
    pub async fn finish(
        &self,
        claim: Claim,
        mut obs: Observation,
        verification_ttl: Option<u64>,
    ) -> Result<bool, Error> {
        if let Some(hello) = &obs.handshake {
            let role_ok = if claim.role == Role::Introducer {
                hello.node_type == dg_xch_core::protocols::NodeType::FullNode as u8
            } else {
                [
                    dg_xch_core::protocols::NodeType::FullNode as u8,
                    dg_xch_core::protocols::NodeType::Introducer as u8,
                ]
                .contains(&hello.node_type)
            };
            if hello.network_id != self.network || !role_ok {
                obs.error =
                    Some("peer handshake has wrong network or unsupported node type".into());
                obs.phase = Phase::Handshake;
                obs.handshake = None;
                obs.peer_request_ms = None;
                obs.connect_ms = None;
                obs.peers.clear();
            }
        }
        if let Some(crawler) = claim.policy.crawler
            && obs.error.is_none()
            && obs
                .handshake
                .as_ref()
                .is_some_and(|h| h.node_type == dg_xch_core::protocols::NodeType::FullNode as u8)
            && !obs
                .peak_height
                .is_some_and(|height| height >= crawler.minimum_height)
        {
            obs.error = Some("no qualifying block peak received".into());
            obs.phase = Phase::Verification;
        }
        let successful = obs.error.is_none()
            && obs.handshake.is_some()
            && (claim.role == Role::Introducer || obs.peer_request_ms.is_some());
        let reachable = claim.policy.crawler.is_some_and(|c| {
            obs.handshake
                .as_ref()
                .is_some_and(|h| h.node_type == dg_xch_core::protocols::NodeType::FullNode as u8)
                && obs
                    .peak_height
                    .is_some_and(|height| height >= c.minimum_height)
        });
        let delay = if claim.policy.crawler.is_some() {
            if claim.endpoint.is_ipv6() {
                claim.policy.revisit_seconds.min(600)
            } else {
                claim.policy.revisit_seconds
            }
        } else if successful {
            claim.policy.revisit_seconds
        } else {
            claim
                .policy
                .cooldown_seconds
                .saturating_mul(1u64 << claim.failures.min(12))
                .min(86400)
        };
        let jitter = (claim.started_at
            ^ u64::from(claim.endpoint.port())
            ^ claim.attempt.wrapping_mul(7919))
            % (delay / 10 + 1);
        let error = (!successful).then(|| {
            obs.error
                .as_deref()
                .unwrap_or("probe did not complete")
                .chars()
                .take(256)
                .collect::<String>()
        });
        let mut tx = self.pool.begin().await.map_err(Error::other)?;
        // Lock all affected peers in a consistent address order before merging overlapping gossip.
        let mut gossip = std::collections::BTreeMap::<SocketAddr, Option<u64>>::new();
        let mut rejected = 0i64;
        let observed_at: i64 =
            sqlx::query_scalar("SELECT floor(extract(epoch FROM clock_timestamp()))::bigint")
                .fetch_one(&mut *tx)
                .await
                .map_err(Error::other)?;
        let observed_at = observed_at as u64;
        if obs.handshake.is_some() && obs.peer_request_ms.is_some() && claim.role == Role::Crawler {
            let introducer = obs
                .handshake
                .as_ref()
                .is_some_and(|h| h.node_type == dg_xch_core::protocols::NodeType::Introducer as u8);
            for p in &obs.peers {
                match p.host.parse().map(|ip| SocketAddr::new(ip, p.port)) {
                    Ok(endpoint)
                        if admissible(endpoint, claim.policy.allow_private)
                            && claim.policy.crawler.is_none_or(|c| {
                                endpoint.port() == c.port
                                    && (introducer
                                        || (p.timestamp > observed_at.saturating_sub(5 * 86400)
                                            && p.timestamp <= observed_at.saturating_add(600)))
                            }) =>
                    {
                        // Chia introducers deliberately omit gossip timestamps for vetted
                        // referrals. Record the local sighting without inventing a remote age.
                        let reported = (!introducer).then_some(p.timestamp);
                        gossip
                            .entry(endpoint)
                            .and_modify(|existing| *existing = (*existing).max(reported))
                            .or_insert(reported);
                    }
                    _ => {
                        rejected += 1;
                    }
                }
            }
        }
        let mut locks: Vec<_> = gossip.keys().copied().collect();
        locks.push(claim.endpoint);
        locks.sort();
        locks.dedup();
        let ips: Vec<_> = locks.iter().map(|p| p.ip().to_string()).collect();
        let ports: Vec<_> = locks.iter().map(|p| i32::from(p.port())).collect();
        sqlx::query("SELECT p.ip FROM dg_discovery.peers p JOIN unnest($2::text[],$3::integer[]) x(ip,port) ON p.ip=x.ip::inet AND p.port=x.port WHERE p.network=$1 ORDER BY p.ip,p.port FOR UPDATE OF p")
            .bind(&self.network).bind(ips).bind(ports).fetch_all(&mut *tx).await.map_err(Error::other)?;
        let now: i64 =
            sqlx::query_scalar("SELECT floor(extract(epoch FROM clock_timestamp()))::bigint")
                .fetch_one(&mut *tx)
                .await
                .map_err(Error::other)?;
        let accepted=sqlx::query("UPDATE dg_discovery.jobs SET next_attempt=$6,last_error=$7,last_error_phase=$8,last_error_at=$9,successes=successes+$10,consecutive_failures=CASE WHEN $10=1 THEN 0 ELSE consecutive_failures+1 END,lease_token=NULL,lease_until=NULL WHERE network=$1 AND role=$2 AND ip=$3::inet AND port=$4 AND lease_token=$5 AND lease_until>$11")
            .bind(&self.network).bind(claim.role.as_str()).bind(claim.endpoint.ip().to_string()).bind(i32::from(claim.endpoint.port())).bind(claim.token).bind(now+(delay+jitter) as i64).bind(&error).bind(error.as_ref().map(|_|obs.phase.as_str())).bind(error.as_ref().map(|_|now)).bind(i64::from(successful)).bind(now).execute(&mut *tx).await.map_err(Error::other)?;
        if accepted.rows_affected() == 0 {
            return Ok(false);
        }
        if claim.policy.crawler.is_some() {
            let row = sqlx::query("SELECT reliability_weight,reliability_count,reliability_value,reliability_tries,reliability_successes,reliability_at,ignore_until,ban_until FROM dg_discovery.jobs WHERE network=$1 AND role=$2 AND ip=$3::inet AND port=$4")
                .bind(&self.network).bind(claim.role.as_str()).bind(claim.endpoint.ip().to_string()).bind(i32::from(claim.endpoint.port())).fetch_one(&mut *tx).await.map_err(Error::other)?;
            let mut reliability = read_reliability(&row)?;
            // Introducers supply discovery data but are never scored as full nodes.
            if obs
                .handshake
                .as_ref()
                .is_none_or(|h| h.node_type != dg_xch_core::protocols::NodeType::Introducer as u8)
            {
                reliability.update(reachable, now as u64);
            }
            sqlx::query("UPDATE dg_discovery.jobs SET reliability_weight=$5,reliability_count=$6,reliability_value=$7,reliability_tries=$8,reliability_successes=$9,reliability_at=$10,reliable=$11,ignore_until=$12,ban_until=$13,next_attempt=greatest(next_attempt,$12,$13) WHERE network=$1 AND role=$2 AND ip=$3::inet AND port=$4")
                .bind(&self.network).bind(claim.role.as_str()).bind(claim.endpoint.ip().to_string()).bind(i32::from(claim.endpoint.port()))
                .bind(reliability.stats.iter().map(|s| s.weight).collect::<Vec<_>>()).bind(reliability.stats.iter().map(|s| s.count).collect::<Vec<_>>()).bind(reliability.stats.iter().map(|s| s.reliability).collect::<Vec<_>>())
                .bind(reliability.tries as i64).bind(reliability.successes as i64).bind(reliability.updated_at.map(|v| v as i64)).bind(reliability.is_reliable() && obs.handshake.as_ref().is_none_or(|h| h.node_type != dg_xch_core::protocols::NodeType::Introducer as u8)).bind(reliability.ignore_until as i64).bind(reliability.ban_until as i64).execute(&mut *tx).await.map_err(Error::other)?;
        }
        if let Some(hello) = &obs.handshake {
            let node_id = obs
                .node_id
                .as_deref()
                .map(hex::decode)
                .transpose()
                .map_err(Error::other)?;
            let capabilities: Vec<_> = hello
                .capabilities
                .iter()
                .take(32)
                .map(|(id, v)| (*id, v.chars().take(32).collect::<String>()))
                .collect();
            sqlx::query("UPDATE dg_discovery.peers SET first_success=coalesce(first_success,$4),last_success=greatest(last_success,$4),last_seen=greatest(last_seen,$4),metadata_at=$4,node_id=coalesce($5,node_id),software_version=nullif($6,''),protocol_version=nullif($7,''),tls_version=coalesce($15,tls_version),node_type=$8,advertised_port=$9,capabilities=$10,connect_ms=$11,peer_request_ms=coalesce($12,peer_request_ms),last_peer_response=CASE WHEN $12 IS NOT NULL THEN $4 ELSE last_peer_response END,peak_height=coalesce($13,peak_height),peak_observed_at=CASE WHEN $13 IS NOT NULL THEN $4 ELSE peak_observed_at END,verified_at=CASE WHEN $14 IS NOT NULL THEN $4 ELSE verified_at END,verified_until=coalesce($14,verified_until) WHERE network=$1 AND ip=$2::inet AND port=$3 AND (metadata_at IS NULL OR metadata_at<=$4)")
                .bind(&self.network).bind(claim.endpoint.ip().to_string()).bind(i32::from(claim.endpoint.port())).bind(now).bind(node_id).bind(hello.software_version.chars().take(128).collect::<String>()).bind(hello.protocol_version.chars().take(32).collect::<String>()).bind(i16::from(hello.node_type)).bind(i32::from(hello.server_port)).bind(serde_json::to_value(capabilities).map_err(Error::other)?).bind(obs.connect_ms).bind(obs.peer_request_ms).bind(obs.peak_height.map(i64::from)).bind(verification_ttl.filter(|_|successful&&claim.role==Role::Introducer).map(|ttl|now+ttl as i64)).bind(obs.tls_version.as_ref().map(|version| version.chars().take(32).collect::<String>())).execute(&mut *tx).await.map_err(Error::other)?;
        }
        self.seen_batch(
            &mut tx,
            &gossip.into_iter().collect::<Vec<_>>(),
            &claim.endpoint.to_string(),
            Some(claim.endpoint),
        )
        .await?;
        sqlx::query("INSERT INTO dg_discovery.observations(network,role,attempt_id,worker_id,ip,port,started_at,finished_at,successful,handshake_completed,peer_response,connect_ms,peer_request_ms,node_id,software_version,error,error_phase,received_peers) VALUES($1,$2,$3,$4,$5::inet,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18)")
            .bind(&self.network).bind(claim.role.as_str()).bind(claim.token).bind(self.worker_id).bind(claim.endpoint.ip().to_string()).bind(i32::from(claim.endpoint.port())).bind(claim.started_at as i64).bind(now).bind(successful).bind(obs.handshake.is_some()).bind(obs.peer_request_ms.is_some()&&claim.role==Role::Crawler).bind(obs.connect_ms).bind(obs.peer_request_ms).bind(obs.node_id.as_deref().map(hex::decode).transpose().map_err(Error::other)?).bind(obs.handshake.as_ref().map(|h|h.software_version.chars().take(128).collect::<String>())).bind(&error).bind(error.as_ref().map(|_|obs.phase.as_str())).bind(obs.peers.len().min(1000) as i32).execute(&mut *tx).await.map_err(Error::other)?;
        sqlx::query("INSERT INTO dg_discovery.counters(network,role,worker_id,connections,peer_responses,failures,gossip_received,gossip_rejected,connect_seconds_sum,request_seconds_sum) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10) ON CONFLICT(network,role,worker_id) DO UPDATE SET connections=dg_discovery.counters.connections+excluded.connections,peer_responses=dg_discovery.counters.peer_responses+excluded.peer_responses,failures=dg_discovery.counters.failures+excluded.failures,gossip_received=dg_discovery.counters.gossip_received+excluded.gossip_received,gossip_rejected=dg_discovery.counters.gossip_rejected+excluded.gossip_rejected,connect_seconds_sum=dg_discovery.counters.connect_seconds_sum+excluded.connect_seconds_sum,request_seconds_sum=dg_discovery.counters.request_seconds_sum+excluded.request_seconds_sum")
            .bind(&self.network).bind(claim.role.as_str()).bind(self.worker_id).bind(i64::from(obs.handshake.is_some())).bind(i64::from(obs.peer_request_ms.is_some()&&claim.role==Role::Crawler)).bind(i64::from(!successful)).bind(if obs.peer_request_ms.is_some(){obs.peers.len() as i64}else{0}).bind(rejected).bind(obs.connect_ms.unwrap_or(0.0)/1000.0).bind(obs.peer_request_ms.unwrap_or(0.0)/1000.0).execute(&mut *tx).await.map_err(Error::other)?;
        tx.commit().await.map_err(Error::other)?;
        Ok(true)
    }
    pub async fn maintenance(&self, role: Role, retention_days: u64) -> Result<u64, Error> {
        let leader=sqlx::query("UPDATE dg_discovery.budgets SET maintenance_after=clock_timestamp()+interval '30 seconds' WHERE network=$1 AND role=$2 AND maintenance_after<=clock_timestamp() RETURNING interval_ms")
            .bind(&self.network).bind(role.as_str()).fetch_optional(&self.pool).await.map_err(Error::other)?;
        let Some(leader) = leader else {
            return Ok(0);
        };
        // Retire at least two maintenance periods' maximum probe volume, so
        // retention can catch up even at the supported 1 ms start interval.
        let observation_limit = (60_000 / leader.get::<i64, _>("interval_ms")).max(1000);
        let now = self.clock().await? as i64;
        let mut expired = 0;
        if role == Role::Introducer {
            let mut tx = self.pool.begin().await.map_err(Error::other)?;
            expired=sqlx::query("UPDATE dg_discovery.peers SET verified_until=NULL WHERE (network,ip,port) IN (SELECT network,ip,port FROM dg_discovery.peers WHERE network=$1 AND verified_until<=$2 ORDER BY verified_until,ip,port LIMIT 1000 FOR UPDATE SKIP LOCKED)")
                .bind(&self.network).bind(now).execute(&mut *tx).await.map_err(Error::other)?.rows_affected();
            if expired > 0 {
                sqlx::query("INSERT INTO dg_discovery.counters(network,role,worker_id,verification_expired) VALUES($1,'introducer',$2,$3) ON CONFLICT(network,role,worker_id) DO UPDATE SET verification_expired=dg_discovery.counters.verification_expired+excluded.verification_expired")
                .bind(&self.network).bind(self.worker_id).bind(expired as i64).execute(&mut *tx).await.map_err(Error::other)?;
            }
            tx.commit().await.map_err(Error::other)?;
        }
        if role == Role::Crawler {
            let mut tx = self.pool.begin().await.map_err(Error::other)?;
            let pruned=sqlx::query("DELETE FROM dg_discovery.peers WHERE (network,ip,port) IN (SELECT p.network,p.ip,p.port FROM dg_discovery.peers p WHERE p.network=$1 AND p.last_seen<$2 AND (p.verified_until IS NULL OR p.verified_until<=$3) AND NOT EXISTS(SELECT 1 FROM dg_discovery.jobs j WHERE j.network=p.network AND j.ip=p.ip AND j.port=p.port AND j.lease_until>$3) ORDER BY p.last_seen,p.ip,p.port LIMIT 1000 FOR UPDATE OF p SKIP LOCKED)")
                .bind(&self.network).bind(now-(retention_days*86400) as i64).bind(now).execute(&mut *tx).await.map_err(Error::other)?.rows_affected();
            if pruned > 0 {
                sqlx::query("INSERT INTO dg_discovery.counters(network,role,worker_id,pruned) VALUES($1,'crawler',$2,$3) ON CONFLICT(network,role,worker_id) DO UPDATE SET pruned=dg_discovery.counters.pruned+excluded.pruned").bind(&self.network).bind(self.worker_id).bind(pruned as i64).execute(&mut *tx).await.map_err(Error::other)?;
            }
            tx.commit().await.map_err(Error::other)?;
            sqlx::query("DELETE FROM dg_discovery.history WHERE network=$1 AND bucket<$2")
                .bind(&self.network)
                .bind((now - 86400) / 60)
                .execute(&self.pool)
                .await
                .map_err(Error::other)?;
        }
        sqlx::query("DELETE FROM dg_discovery.observations WHERE (network,attempt_id) IN (SELECT network,attempt_id FROM dg_discovery.observations WHERE network=$1 AND role=$2 AND finished_at<$3 ORDER BY finished_at LIMIT $4)")
            .bind(&self.network).bind(role.as_str()).bind(now-(retention_days*86400) as i64).bind(observation_limit).execute(&self.pool).await.map_err(Error::other)?;
        sqlx::query(
            "DELETE FROM dg_discovery.cooldowns WHERE network=$1 AND role=$2 AND next_at<$3",
        )
        .bind(&self.network)
        .bind(role.as_str())
        .bind(now - 86400)
        .execute(&self.pool)
        .await
        .map_err(Error::other)?;
        Ok(expired)
    }
}
