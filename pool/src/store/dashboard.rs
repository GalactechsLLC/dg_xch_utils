use super::*;
use serde_json::{Value, json};

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DashboardQuery {
    pub page: u32,
    pub days: u32,
    pub search: String,
    pub launcher: Option<Bytes32>,
    pub status: String,
    pub harvester: String,
    pub sort: String,
}
impl DashboardQuery {
    pub fn validate(&self) -> Result<(), Error> {
        if self.page > 100_000
            || self.search.len() > 128
            || self.harvester.len() > 66
            || !matches!(self.days, 0 | 1 | 7 | 30 | 90 | 365)
            || !matches!(
                self.status.as_str(),
                "" | "accepted" | "duplicate" | "invalid"
            )
            || !matches!(self.sort.as_str(), "" | "points" | "blocks" | "recent")
        {
            return Err(Error::other("invalid dashboard filters"));
        }
        Ok(())
    }
    fn since(&self, now: u64) -> i64 {
        now.saturating_sub(u64::from(if self.days == 0 { 1 } else { self.days }) * 86400) as i64
    }
}
fn page(rows: Vec<Value>, number: u32) -> Value {
    let has_more = rows.len() > 50;
    json!({"items": rows.into_iter().take(50).collect::<Vec<_>>(), "page": number, "has_more": has_more})
}
impl PoolStore {
    #[allow(clippy::too_many_arguments)]
    pub async fn record_partial_event(
        &mut self,
        launcher: Bytes32,
        received: u64,
        status: &str,
        harvester: Bytes32,
        points: u64,
        verification_ms: u64,
        partial_id: Option<Bytes32>,
    ) -> Result<(), Error> {
        sqlx::query("INSERT INTO partial_events(launcher,received,status,harvester,points,verification_ms,partial_id) VALUES (?,?,?,?,?,?,?)")
            .bind(launcher.to_string()).bind(i64::try_from(received).map_err(Error::other)?).bind(status)
            .bind(harvester.to_string()).bind(points.to_string()).bind(i64::try_from(verification_ms).map_err(Error::other)?)
            .bind(partial_id.map(|id|id.to_string()))
            .execute(&mut self.connection).await.map_err(Error::other)?;
        Ok(())
    }

    pub async fn dashboard(
        &mut self,
        kind: &str,
        query: &DashboardQuery,
        session: Option<&AccountSession>,
        now: u64,
    ) -> Result<Value, Error> {
        query.validate()?;
        let account = session
            .map(|session| session.account.as_str())
            .unwrap_or("");
        let launcher = query.launcher.map(|id| id.to_string()).unwrap_or_default();
        if kind.starts_with("account/") {
            let session = session.ok_or_else(|| Error::other("login required"))?;
            if let Some(launcher) = query.launcher
                && !self
                    .account_farmers(session)
                    .await?
                    .iter()
                    .any(|farmer| farmer.launcher_id == launcher)
            {
                return Err(Error::other("farmer is not linked to this account"));
            }
        }
        let since = query.since(now);
        let offset = i64::from(query.page) * 50;
        match kind {
            "overview" | "account/statistics" => {
                let private = kind.starts_with("account/");
                let rows = sqlx::query("SELECT (received / ?) * ? AS time, count(*) AS partials, CAST(sum(CAST(points AS INTEGER)) AS TEXT) AS points FROM partials WHERE received>=? AND (?=0 OR launcher IN (SELECT launcher FROM web_farmers WHERE account=?)) AND (?='' OR launcher=?) GROUP BY time ORDER BY time")
                    .bind(if query.days <= 1 { 3600 } else { 86400 }).bind(if query.days <= 1 { 3600 } else { 86400 }).bind(since)
                    .bind(private).bind(account).bind(&launcher).bind(&launcher).fetch_all(&mut self.connection).await.map_err(Error::other)?;
                let history = rows.into_iter().map(|row| Ok(json!({"time": row.try_get::<i64,_>("time").map_err(Error::other)?, "partials": row.try_get::<i64,_>("partials").map_err(Error::other)?, "points": row.try_get::<String,_>("points").map_err(Error::other)?}))).collect::<Result<Vec<_>,Error>>()?;
                let activity = sqlx::query("SELECT count(DISTINCT launcher) AS active, max(received) AS last_seen FROM partials WHERE received>=? AND (?=0 OR launcher IN (SELECT launcher FROM web_farmers WHERE account=?)) AND (?='' OR launcher=?)")
                    .bind(since).bind(private).bind(account).bind(&launcher).bind(&launcher).fetch_one(&mut self.connection).await.map_err(Error::other)?;
                let diagnostics = sqlx::query("SELECT status,count(*) AS count FROM partial_events WHERE received>=? AND (?=0 OR launcher IN (SELECT launcher FROM web_farmers WHERE account=?)) AND (?='' OR launcher=?) GROUP BY status")
                    .bind(since).bind(private).bind(account).bind(&launcher).bind(&launcher).fetch_all(&mut self.connection).await.map_err(Error::other)?;
                let diagnostics = diagnostics.into_iter().map(|row| Ok(json!({"status":row.try_get::<String,_>("status").map_err(Error::other)?,"count":row.try_get::<i64,_>("count").map_err(Error::other)?}))).collect::<Result<Vec<_>,Error>>()?;
                Ok(
                    json!({"totals": self.stats().await?, "history": history, "since": since, "now": now,
                    "active_farmers":activity.try_get::<i64,_>("active").map_err(Error::other)?,
                    "last_partial":activity.try_get::<Option<i64>,_>("last_seen").map_err(Error::other)?,"diagnostics":diagnostics}),
                )
            }
            "farmers" | "leaderboards" => {
                let ordering = match query.sort.as_str() {
                    "blocks" => "blocks DESC, points DESC",
                    "recent" => "last_seen DESC",
                    _ => "points DESC, blocks DESC",
                };
                let mut sql = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
                    "SELECT f.launcher, coalesce(w.name,'') AS name, coalesce(p.points,0) AS points, coalesce(p.partials,0) AS partials, p.last_seen, coalesce(b.blocks,0) AS blocks FROM farmers f LEFT JOIN web_farmers w ON w.launcher=f.launcher LEFT JOIN (SELECT launcher,sum(CAST(points AS INTEGER)) AS points,count(*) AS partials,max(received) AS last_seen FROM partials WHERE received>=? GROUP BY launcher) p ON p.launcher=f.launcher LEFT JOIN (SELECT json_extract(data,'$.launcher_id') AS launcher,count(*) AS blocks FROM reward_claims WHERE json_extract(data,'$.cutoff')>=? GROUP BY launcher) b ON b.launcher=f.launcher WHERE instr(lower(f.launcher),lower(?))>0 OR instr(lower(coalesce(w.name,'')),lower(?))>0 ORDER BY ",
                );
                sql.push(ordering).push(",f.launcher LIMIT 51 OFFSET ?");
                let rows = sql
                    .build()
                    .bind(since)
                    .bind(since)
                    .bind(&query.search)
                    .bind(&query.search)
                    .bind(offset)
                    .fetch_all(&mut self.connection)
                    .await
                    .map_err(Error::other)?;
                let rows = rows.into_iter().map(|r| Ok(json!({"launcher_id": r.try_get::<String,_>("launcher").map_err(Error::other)?, "name":r.try_get::<String,_>("name").map_err(Error::other)?, "points":r.try_get::<i64,_>("points").map_err(Error::other)?.to_string(), "partials":r.try_get::<i64,_>("partials").map_err(Error::other)?, "last_seen":r.try_get::<Option<i64>,_>("last_seen").map_err(Error::other)?, "blocks":r.try_get::<i64,_>("blocks").map_err(Error::other)?}))).collect::<Result<Vec<_>,Error>>()?;
                Ok(page(rows, query.page))
            }
            "blocks" | "account/blocks" => {
                let rows = sqlx::query("SELECT data FROM reward_claims WHERE json_extract(data,'$.cutoff')>=? AND (?=0 OR json_extract(data,'$.launcher_id') IN (SELECT launcher FROM web_farmers WHERE account=?)) AND (?='' OR json_extract(data,'$.launcher_id')=?) ORDER BY json_extract(data,'$.cutoff') DESC,id LIMIT 51 OFFSET ?")
                    .bind(since).bind(kind.starts_with("account/")).bind(account).bind(&launcher).bind(&launcher).bind(offset).fetch_all(&mut self.connection).await.map_err(Error::other)?;
                let rows = rows.into_iter().map(|r| { let claim: RewardClaim=serde_json::from_str(&r.try_get::<String,_>("data").map_err(Error::other)?).map_err(Error::other)?; Ok(json!({"launcher_id":claim.launcher_id,"reward_coin":claim.reward.coin_id(),"amount":claim.reward.amount.to_string(),"time":claim.cutoff})) }).collect::<Result<Vec<_>,Error>>()?;
                Ok(page(rows, query.page))
            }
            "payouts" | "account/payouts" => {
                let private = kind.starts_with("account/");
                let rows = sqlx::query("SELECT b.reward_coin,b.snapshot,b.distribution,b.confirmed_height,r.parameters FROM batches b JOIN batch_requests r USING(reward_coin) WHERE json_extract(r.parameters,'$[3]')>=? AND (?=0 OR EXISTS(SELECT 1 FROM json_each(b.snapshot) s JOIN web_farmers w ON w.launcher=json_extract(s.value,'$.launcher_id') WHERE w.account=? AND (?='' OR w.launcher=?))) ORDER BY json_extract(r.parameters,'$[3]') DESC,b.reward_coin LIMIT 51 OFFSET ?")
                    .bind(since).bind(private).bind(account).bind(&launcher).bind(&launcher).bind(offset).fetch_all(&mut self.connection).await.map_err(Error::other)?;
                let owned = if let Some(session) = session {
                    self.account_farmers(session)
                        .await?
                        .into_iter()
                        .map(|f| f.launcher_id)
                        .collect::<Vec<_>>()
                } else {
                    Vec::new()
                };
                let rows = rows.into_iter().map(|r| {
                    let distribution: Distribution=serde_json::from_str(&r.try_get::<String,_>("distribution").map_err(Error::other)?).map_err(Error::other)?;
                    let (reward,fee,tx,cutoff):(u64,u16,u64,u64)=serde_json::from_str(&r.try_get::<String,_>("parameters").map_err(Error::other)?).map_err(Error::other)?;
                    let mut amount=0u64;
                    let mut allocations=Vec::new();
                    if private {
                        let shares:Vec<RewardShare>=serde_json::from_str(&r.try_get::<String,_>("snapshot").map_err(Error::other)?).map_err(Error::other)?;
                        // Same largest-remainder calculation, with destinations replaced by
                        // launchers solely to expose the exact allocation before address grouping.
                        let individual:Vec<_>=shares.iter().map(|s| RewardShare{payout_puzzle_hash:s.launcher_id,..s.clone()}).collect();
                        for payout in distribute(reward,fee,tx,&individual)?.payouts {
                            if owned.contains(&payout.puzzle_hash) && query.launcher.is_none_or(|id|id==payout.puzzle_hash) {
                                amount=amount.checked_add(payout.amount).ok_or_else(||Error::other("payout overflow"))?;
                                let share=shares.iter().find(|s|s.launcher_id==payout.puzzle_hash).ok_or_else(||Error::other("missing reward share"))?;
                                allocations.push(json!({"launcher_id":share.launcher_id,"address":share.payout_puzzle_hash,"amount":payout.amount.to_string()}));
                            }
                        }
                    } else { for payout in distribution.payouts { amount=amount.checked_add(payout.amount).ok_or_else(||Error::other("payout overflow"))?; } }
                    Ok(json!({"reward_coin":r.try_get::<String,_>("reward_coin").map_err(Error::other)?,"amount":amount.to_string(),"time":cutoff,"confirmed_height":r.try_get::<Option<i64>,_>("confirmed_height").map_err(Error::other)?,"allocations":allocations}))
                }).collect::<Result<Vec<_>,Error>>()?;
                Ok(page(rows, query.page))
            }
            "account/partials" => {
                // Canonical credited history is retained even before diagnostics existed.
                let rows = if query.status.is_empty() || query.status == "accepted" {
                    sqlx::query("SELECT id,launcher,received,points FROM partials WHERE launcher IN (SELECT launcher FROM web_farmers WHERE account=?) AND (?='' OR launcher=?) AND received>=? AND (?='' OR EXISTS(SELECT 1 FROM partial_events e WHERE e.partial_id=partials.id AND e.harvester=?)) ORDER BY received DESC,id LIMIT 51 OFFSET ?")
                        .bind(account).bind(&launcher).bind(&launcher).bind(since).bind(&query.harvester).bind(&query.harvester).bind(offset).fetch_all(&mut self.connection).await.map_err(Error::other)?
                        .into_iter().map(|r|Ok(json!({"id":r.try_get::<String,_>("id").map_err(Error::other)?,"launcher_id":r.try_get::<String,_>("launcher").map_err(Error::other)?,"time":r.try_get::<i64,_>("received").map_err(Error::other)?,"points":r.try_get::<String,_>("points").map_err(Error::other)?,"status":"accepted"}))).collect::<Result<Vec<_>,Error>>()?
                } else {
                    sqlx::query("SELECT id,launcher,received,status,harvester,verification_ms FROM partial_events WHERE launcher IN (SELECT launcher FROM web_farmers WHERE account=?) AND (?='' OR launcher=?) AND received>=? AND status=? AND (?='' OR harvester=?) ORDER BY received DESC,id DESC LIMIT 51 OFFSET ?")
                        .bind(account).bind(&launcher).bind(&launcher).bind(since).bind(&query.status).bind(&query.harvester).bind(&query.harvester).bind(offset).fetch_all(&mut self.connection).await.map_err(Error::other)?
                        .into_iter().map(|r|Ok(json!({"id":r.try_get::<i64,_>("id").map_err(Error::other)?,"launcher_id":r.try_get::<String,_>("launcher").map_err(Error::other)?,"time":r.try_get::<i64,_>("received").map_err(Error::other)?,"status":r.try_get::<String,_>("status").map_err(Error::other)?,"harvester":r.try_get::<String,_>("harvester").map_err(Error::other)?,"verification_ms":r.try_get::<i64,_>("verification_ms").map_err(Error::other)?}))).collect::<Result<Vec<_>,Error>>()?
                };
                Ok(page(rows, query.page))
            }
            "account/harvesters" => {
                let rows=sqlx::query("SELECT launcher,harvester,count(*) AS requests,sum(status='accepted') AS accepted,max(received) AS last_seen,avg(verification_ms) AS verification_ms FROM partial_events WHERE launcher IN (SELECT launcher FROM web_farmers WHERE account=?) AND (?='' OR launcher=?) AND received>=? GROUP BY launcher,harvester ORDER BY last_seen DESC,harvester LIMIT 51 OFFSET ?")
                    .bind(account).bind(&launcher).bind(&launcher).bind(since).bind(offset).fetch_all(&mut self.connection).await.map_err(Error::other)?;
                let rows=rows.into_iter().map(|r|Ok(json!({"launcher_id":r.try_get::<String,_>("launcher").map_err(Error::other)?,"harvester":r.try_get::<String,_>("harvester").map_err(Error::other)?,"requests":r.try_get::<i64,_>("requests").map_err(Error::other)?,"accepted":r.try_get::<i64,_>("accepted").map_err(Error::other)?,"last_seen":r.try_get::<i64,_>("last_seen").map_err(Error::other)?,"verification_ms":r.try_get::<f64,_>("verification_ms").map_err(Error::other)?}))).collect::<Result<Vec<_>,Error>>()?;
                Ok(page(rows, query.page))
            }
            _ => Err(Error::other("unknown dashboard dataset")),
        }
    }
}
