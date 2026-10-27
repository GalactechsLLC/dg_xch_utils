use super::*;
use dg_xch_core::protocols::pool_web::{BrowserAction, BrowserProof, BrowserTicket};

const SESSION_SECONDS: u64 = 12 * 60 * 60;
const MAX_LINKED_FARMERS: i64 = 32;

#[derive(Clone)]
pub struct AccountSession {
    pub account: String,
    pub launcher_id: Bytes32,
    pub csrf: String,
    pub expires: u64,
}

#[derive(Clone, Serialize)]
pub struct AccountFarmer {
    pub launcher_id: Bytes32,
    pub name: String,
    pub payout_puzzle_hash: Bytes32,
    pub contract_puzzle_hash: Bytes32,
}

fn token() -> String {
    hex::encode(rand::rng().random::<[u8; 32]>())
}

fn token_hash(value: &str) -> Result<String, Error> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(Error::other("invalid authentication token"));
    }
    Ok(hex::encode(hash_256(value.as_bytes())))
}

async fn account_for(
    connection: &mut SqliteConnection,
    launcher: Bytes32,
    now: i64,
) -> Result<String, Error> {
    if let Some(account) =
        sqlx::query_scalar::<_, String>("SELECT account FROM web_farmers WHERE launcher=?")
            .bind(launcher.to_string())
            .fetch_optional(&mut *connection)
            .await
            .map_err(Error::other)?
    {
        return Ok(account);
    }
    let account = token();
    sqlx::query("INSERT INTO web_accounts(id, created) VALUES (?, ?)")
        .bind(&account)
        .bind(now)
        .execute(&mut *connection)
        .await
        .map_err(Error::other)?;
    sqlx::query("INSERT INTO web_farmers(launcher, account) VALUES (?, ?)")
        .bind(launcher.to_string())
        .bind(&account)
        .execute(&mut *connection)
        .await
        .map_err(Error::other)?;
    Ok(account)
}

async fn revoke_account(connection: &mut SqliteConnection, account: &str) -> Result<(), Error> {
    for query in [
        "DELETE FROM web_sessions WHERE account=?",
        "DELETE FROM web_tickets WHERE account=?",
    ] {
        sqlx::query(query)
            .bind(account)
            .execute(&mut *connection)
            .await
            .map_err(Error::other)?;
    }
    Ok(())
}

impl PoolStore {
    /// Called only after all owner signatures and current on-chain memberships
    /// have been verified. Proof consumption, linking, and tickets are atomic.
    pub async fn issue_browser_ticket(
        &mut self,
        proof: &BrowserProof,
        now: u64,
    ) -> Result<BrowserTicket, Error> {
        dg_xch_core::protocols::pool_web::timestamp(proof.timestamp, now)?;
        let nonce = token_hash(&proof.nonce)?;
        match (&proof.action, proof.linked_launcher_id) {
            (BrowserAction::Link, Some(other)) if other != proof.launcher_id => {}
            (BrowserAction::Login | BrowserAction::Unlink, None) => {}
            _ => return Err(Error::other("invalid account linking request")),
        }
        let now_sql = i64::try_from(now).map_err(Error::other)?;
        let mut transaction = self.connection.begin().await.map_err(Error::other)?;
        for query in [
            "DELETE FROM web_proofs WHERE expires<?",
            "DELETE FROM web_tickets WHERE expires<?",
            "DELETE FROM web_sessions WHERE expires<?",
        ] {
            sqlx::query(query)
                .bind(now_sql)
                .execute(&mut *transaction)
                .await
                .map_err(Error::other)?;
        }
        // Keep a used proof longer than its entire allowed clock-skew window.
        sqlx::query("INSERT INTO web_proofs(nonce, expires) VALUES (?, ?)")
            .bind(nonce)
            .bind(
                now_sql
                    .checked_add(180)
                    .ok_or_else(|| Error::other("time overflow"))?,
            )
            .execute(&mut *transaction)
            .await
            .map_err(|_| Error::other("browser authentication proof was already used"))?;
        let mut account = account_for(&mut transaction, proof.launcher_id, now_sql).await?;
        if let Some(other) = proof.linked_launcher_id {
            let other_account = account_for(&mut transaction, other, now_sql).await?;
            if account != other_account {
                let count: i64 =
                    sqlx::query_scalar("SELECT count(*) FROM web_farmers WHERE account IN (?, ?)")
                        .bind(&account)
                        .bind(&other_account)
                        .fetch_one(&mut *transaction)
                        .await
                        .map_err(Error::other)?;
                if count > MAX_LINKED_FARMERS {
                    return Err(Error::other("an account can link at most 32 PlotNFTs"));
                }
                revoke_account(&mut transaction, &account).await?;
                revoke_account(&mut transaction, &other_account).await?;
                sqlx::query("UPDATE web_farmers SET account=? WHERE account=?")
                    .bind(&account)
                    .bind(&other_account)
                    .execute(&mut *transaction)
                    .await
                    .map_err(Error::other)?;
                sqlx::query("DELETE FROM web_accounts WHERE id=?")
                    .bind(other_account)
                    .execute(&mut *transaction)
                    .await
                    .map_err(Error::other)?;
            }
        } else if proof.action == BrowserAction::Unlink {
            let count: i64 = sqlx::query_scalar("SELECT count(*) FROM web_farmers WHERE account=?")
                .bind(&account)
                .fetch_one(&mut *transaction)
                .await
                .map_err(Error::other)?;
            if count > 1 {
                revoke_account(&mut transaction, &account).await?;
                let new_account = token();
                sqlx::query("INSERT INTO web_accounts(id, created) VALUES (?, ?)")
                    .bind(&new_account)
                    .bind(now_sql)
                    .execute(&mut *transaction)
                    .await
                    .map_err(Error::other)?;
                sqlx::query("UPDATE web_farmers SET account=? WHERE launcher=? AND account=?")
                    .bind(&new_account)
                    .bind(proof.launcher_id.to_string())
                    .bind(&account)
                    .execute(&mut *transaction)
                    .await
                    .map_err(Error::other)?;
                account = new_account;
            }
        }
        let ticket = token();
        let expires = now
            .checked_add(120)
            .ok_or_else(|| Error::other("time overflow"))?;
        // A fresh signed login supersedes outstanding tickets for this identity.
        sqlx::query("DELETE FROM web_tickets WHERE launcher=?")
            .bind(proof.launcher_id.to_string())
            .execute(&mut *transaction)
            .await
            .map_err(Error::other)?;
        sqlx::query(
            "INSERT INTO web_tickets(hash, account, launcher, expires) VALUES (?, ?, ?, ?)",
        )
        .bind(token_hash(&ticket)?)
        .bind(&account)
        .bind(proof.launcher_id.to_string())
        .bind(i64::try_from(expires).map_err(Error::other)?)
        .execute(&mut *transaction)
        .await
        .map_err(Error::other)?;
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM web_farmers WHERE account=?")
            .bind(account)
            .fetch_one(&mut *transaction)
            .await
            .map_err(Error::other)?;
        transaction.commit().await.map_err(Error::other)?;
        Ok(BrowserTicket {
            ticket,
            expires,
            launcher_id: proof.launcher_id,
            linked_farmers: usize::try_from(count).map_err(Error::other)?,
        })
    }

    pub async fn exchange_browser_ticket(
        &mut self,
        ticket: &str,
        now: u64,
    ) -> Result<(String, AccountSession), Error> {
        let hash = token_hash(ticket)?;
        let mut transaction = self.connection.begin().await.map_err(Error::other)?;
        let row = sqlx::query(
            "DELETE FROM web_tickets WHERE hash=? AND expires>? RETURNING account, launcher",
        )
        .bind(hash)
        .bind(i64::try_from(now).map_err(Error::other)?)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(Error::other)?
        .ok_or_else(|| Error::other("login link is expired or already used"))?;
        let account: String = row.try_get("account").map_err(Error::other)?;
        let launcher: String = row.try_get("launcher").map_err(Error::other)?;
        let session = token();
        let csrf = token();
        let expires = now
            .checked_add(SESSION_SECONDS)
            .ok_or_else(|| Error::other("time overflow"))?;
        // Bound active sessions without recording IP addresses or user agents.
        sqlx::query("DELETE FROM web_sessions WHERE account=? AND hash NOT IN (SELECT hash FROM web_sessions WHERE account=? ORDER BY expires DESC LIMIT 15)")
            .bind(&account).bind(&account).execute(&mut *transaction).await.map_err(Error::other)?;
        sqlx::query("INSERT INTO web_sessions(hash, account, launcher, csrf, expires) VALUES (?, ?, ?, ?, ?)")
            .bind(token_hash(&session)?).bind(&account).bind(&launcher).bind(&csrf)
            .bind(i64::try_from(expires).map_err(Error::other)?)
            .execute(&mut *transaction).await.map_err(Error::other)?;
        let account_session = AccountSession {
            account,
            launcher_id: launcher.parse().map_err(Error::other)?,
            csrf,
            expires,
        };
        transaction.commit().await.map_err(Error::other)?;
        Ok((session, account_session))
    }

    pub async fn browser_session(
        &mut self,
        value: &str,
        now: u64,
    ) -> Result<Option<AccountSession>, Error> {
        let row = sqlx::query(
            "SELECT account, launcher, csrf, expires FROM web_sessions WHERE hash=? AND expires>?",
        )
        .bind(token_hash(value)?)
        .bind(i64::try_from(now).map_err(Error::other)?)
        .fetch_optional(&mut self.connection)
        .await
        .map_err(Error::other)?;
        row.map(|row| {
            Ok(AccountSession {
                account: row.try_get("account").map_err(Error::other)?,
                launcher_id: row
                    .try_get::<String, _>("launcher")
                    .map_err(Error::other)?
                    .parse()
                    .map_err(Error::other)?,
                csrf: row.try_get("csrf").map_err(Error::other)?,
                expires: u64::try_from(row.try_get::<i64, _>("expires").map_err(Error::other)?)
                    .map_err(Error::other)?,
            })
        })
        .transpose()
    }

    pub async fn logout_browser(&mut self, value: &str) -> Result<(), Error> {
        sqlx::query("DELETE FROM web_sessions WHERE hash=?")
            .bind(token_hash(value)?)
            .execute(&mut self.connection)
            .await
            .map_err(Error::other)?;
        Ok(())
    }

    pub async fn account_farmers(
        &mut self,
        session: &AccountSession,
    ) -> Result<Vec<AccountFarmer>, Error> {
        let rows = sqlx::query("SELECT web_farmers.launcher, web_farmers.name, farmers.data FROM web_farmers JOIN farmers USING(launcher) WHERE account=? ORDER BY web_farmers.launcher LIMIT 33")
            .bind(&session.account).fetch_all(&mut self.connection).await.map_err(Error::other)?;
        if rows.len() > MAX_LINKED_FARMERS as usize {
            return Err(Error::other("account farmer limit exceeded"));
        }
        rows.into_iter()
            .map(|row| {
                #[derive(Deserialize)]
                struct Identity {
                    payout_puzzle_hash: Bytes32,
                    contract_puzzle_hash: Bytes32,
                }
                let identity: Identity =
                    serde_json::from_str(&row.try_get::<String, _>("data").map_err(Error::other)?)
                        .map_err(Error::other)?;
                Ok(AccountFarmer {
                    launcher_id: row
                        .try_get::<String, _>("launcher")
                        .map_err(Error::other)?
                        .parse()
                        .map_err(Error::other)?,
                    name: row.try_get("name").map_err(Error::other)?,
                    payout_puzzle_hash: identity.payout_puzzle_hash,
                    contract_puzzle_hash: identity.contract_puzzle_hash,
                })
            })
            .collect()
    }

    pub async fn name_account_farmer(
        &mut self,
        session: &AccountSession,
        launcher: Bytes32,
        name: &str,
    ) -> Result<(), Error> {
        let name = name.trim();
        if name.len() > 64 || name.chars().any(char::is_control) {
            return Err(Error::other(
                "farmer name must be at most 64 bytes without control characters",
            ));
        }
        let changed = sqlx::query("UPDATE web_farmers SET name=? WHERE launcher=? AND account=?")
            .bind(name)
            .bind(launcher.to_string())
            .bind(&session.account)
            .execute(&mut self.connection)
            .await
            .map_err(Error::other)?
            .rows_affected();
        if changed != 1 {
            return Err(Error::other("farmer is not linked to this account"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::DashboardQuery;

    fn farmer(index: u8) -> Farmer {
        Farmer {
            version: PoolVersion::V1,
            launcher_id: [index; 32].into(),
            owner_public_key: [index; 48].into(),
            authentication_public_key: [index; 48].into(),
            contract_puzzle_hash: [index + 10; 32].into(),
            payout_puzzle_hash: [9; 32].into(),
            difficulty: 1,
        }
    }
    fn proof(index: u8, action: BrowserAction, other: Option<u8>, now: u64) -> BrowserProof {
        BrowserProof {
            launcher_id: [index; 32].into(),
            linked_launcher_id: other.map(|i| [i; 32].into()),
            action,
            nonce: token(),
            timestamp: now,
        }
    }
    #[tokio::test]
    async fn tickets_are_one_use_links_revoke_sessions_and_unlink_isolates_history() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("pool.sqlite");
        let genesis = [1; 32].into();
        let target = [2; 32].into();
        let mut store = PoolStore::open(&path, genesis, target).await.unwrap();
        for index in 1..=3 {
            store.register(&farmer(index)).await.unwrap();
            store
                .credit_partial([index; 32].into(), &farmer(index), 1000)
                .await
                .unwrap();
        }
        let login = proof(1, BrowserAction::Login, None, 1001);
        let ticket = store.issue_browser_ticket(&login, 1001).await.unwrap();
        assert!(store.issue_browser_ticket(&login, 1001).await.is_err());
        let (first_token, first) = store
            .exchange_browser_ticket(&ticket.ticket, 1001)
            .await
            .unwrap();
        assert!(
            store
                .exchange_browser_ticket(&ticket.ticket, 1001)
                .await
                .is_err()
        );
        assert!(
            store
                .name_account_farmer(&first, [2; 32].into(), "intruder")
                .await
                .is_err()
        );
        let mut query = DashboardQuery {
            launcher: Some([2; 32].into()),
            ..Default::default()
        };
        assert!(
            store
                .dashboard("account/partials", &query, Some(&first), 1001)
                .await
                .is_err()
        );
        assert!(
            store
                .dashboard("account/partials", &query, None, 1001)
                .await
                .is_err()
        );
        let second_ticket = store
            .issue_browser_ticket(&proof(2, BrowserAction::Login, None, 1001), 1001)
            .await
            .unwrap();
        let (second_token, _) = store
            .exchange_browser_ticket(&second_ticket.ticket, 1001)
            .await
            .unwrap();
        let linked = store
            .issue_browser_ticket(&proof(1, BrowserAction::Link, Some(2), 1002), 1002)
            .await
            .unwrap();
        assert_eq!(linked.linked_farmers, 2);
        assert!(
            store
                .browser_session(&first_token, 1002)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .browser_session(&second_token, 1002)
                .await
                .unwrap()
                .is_none()
        );
        let (linked_token, linked) = store
            .exchange_browser_ticket(&linked.ticket, 1002)
            .await
            .unwrap();
        store
            .name_account_farmer(&linked, [2; 32].into(), "Second garden")
            .await
            .unwrap();
        query.launcher = None;
        let history = store
            .dashboard("account/partials", &query, Some(&linked), 1002)
            .await
            .unwrap();
        assert_eq!(history["items"].as_array().unwrap().len(), 2);
        // All three farms share an address. Each account must see only its own exact allocations.
        store
            .prepare_distribution([8; 32].into(), 101, 0, 0, 1002)
            .await
            .unwrap();
        let payouts = store
            .dashboard("account/payouts", &query, Some(&linked), 1002)
            .await
            .unwrap();
        assert_eq!(payouts["items"][0]["amount"], "68");
        assert_eq!(
            payouts["items"][0]["allocations"].as_array().unwrap().len(),
            2
        );
        drop(store);
        let mut store = PoolStore::open(&path, genesis, target).await.unwrap();
        assert!(
            store
                .browser_session(&linked_token, 1003)
                .await
                .unwrap()
                .is_some()
        );
        let separated = store
            .issue_browser_ticket(&proof(2, BrowserAction::Unlink, None, 1003), 1003)
            .await
            .unwrap();
        assert!(
            store
                .browser_session(&linked_token, 1003)
                .await
                .unwrap()
                .is_none()
        );
        let (token, separated) = store
            .exchange_browser_ticket(&separated.ticket, 1003)
            .await
            .unwrap();
        assert_eq!(store.account_farmers(&separated).await.unwrap().len(), 1);
        let payouts = store
            .dashboard("account/payouts", &query, Some(&separated), 1003)
            .await
            .unwrap();
        assert_eq!(payouts["items"][0]["amount"], "34");
        assert!(
            store
                .browser_session(&token, separated.expires)
                .await
                .unwrap()
                .is_none()
        );
        store.logout_browser(&token).await.unwrap();
        assert!(store.browser_session(&token, 1004).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn expiry_filters_and_public_pagination_are_bounded() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = PoolStore::open(
            &directory.path().join("pool.sqlite"),
            [1; 32].into(),
            [2; 32].into(),
        )
        .await
        .unwrap();
        for index in 1..=52 {
            store.register(&farmer(index)).await.unwrap();
        }
        let ticket = store
            .issue_browser_ticket(&proof(1, BrowserAction::Login, None, 1000), 1000)
            .await
            .unwrap();
        assert!(
            store
                .exchange_browser_ticket(&ticket.ticket, 1120)
                .await
                .is_err()
        );
        assert!(
            store
                .issue_browser_ticket(&proof(1, BrowserAction::Login, None, 1000), 1200)
                .await
                .is_err()
        );
        let mut query = DashboardQuery::default();
        let page = store
            .dashboard("farmers", &query, None, 1000)
            .await
            .unwrap();
        assert_eq!(page["items"].as_array().unwrap().len(), 50);
        assert_eq!(page["has_more"], true);
        query.page = 1;
        let page = store
            .dashboard("farmers", &query, None, 1000)
            .await
            .unwrap();
        assert_eq!(page["items"].as_array().unwrap().len(), 2);
        assert_eq!(page["has_more"], false);
        query.search = "' OR 1=1 --".into();
        assert!(
            store
                .dashboard("farmers", &query, None, 1000)
                .await
                .unwrap()["items"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        query.page = 100_001;
        assert!(query.validate().is_err());
    }
}
