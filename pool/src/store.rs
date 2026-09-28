use crate::accounting::{Distribution, RewardShare, distribute};
use dg_xch_core::blockchain::coin::Coin;
use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes48};
use dg_xch_core::blockchain::spend_bundle::SpendBundle;
use dg_xch_core::traits::SizedBytes;
use dg_xch_core::utils::hash_256;
use rand::RngExt;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqliteSynchronous};
use sqlx::{Connection, Row, SqliteConnection};
use std::io::Error;
use std::path::Path;
use std::time::Duration;

pub use dg_xch_core::protocols::pool::PoolVersion;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Farmer {
    pub version: PoolVersion,
    pub launcher_id: Bytes32,
    pub owner_public_key: Bytes48,
    pub authentication_public_key: Bytes48,
    pub contract_puzzle_hash: Bytes32,
    pub payout_puzzle_hash: Bytes32,
    pub difficulty: u64,
}

pub struct PoolStore {
    connection: SqliteConnection,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PayoutBatch {
    pub reward_coin: Bytes32,
    pub distribution: Distribution,
    pub bundle: Option<SpendBundle>,
    pub confirmed_height: Option<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RewardClaim {
    pub launcher_id: Bytes32,
    pub reward: Coin,
    pub payout_coin: Coin,
    pub cutoff: u64,
    pub bundle: SpendBundle,
}

#[derive(Clone, Debug, Serialize)]
pub struct PoolStats {
    pub farmers: u64,
    pub accepted_partials: u64,
    pub reward_claims: u64,
    pub signed_payouts: u64,
    pub confirmed_payouts: u64,
}

impl PoolStore {
    pub async fn stats(&mut self) -> Result<PoolStats, Error> {
        let row = sqlx::query("SELECT (SELECT count(*) FROM farmers) AS farmers, (SELECT count(*) FROM partials) AS partials, (SELECT count(*) FROM reward_claims) AS claims, (SELECT count(*) FROM batches WHERE bundle IS NOT NULL) AS signed, (SELECT count(*) FROM batches WHERE confirmed_height IS NOT NULL) AS confirmed")
            .fetch_one(&mut self.connection).await.map_err(Error::other)?;
        let count = |name| -> Result<u64, Error> {
            u64::try_from(row.try_get::<i64, _>(name).map_err(Error::other)?).map_err(Error::other)
        };
        Ok(PoolStats {
            farmers: count("farmers")?,
            accepted_partials: count("partials")?,
            reward_claims: count("claims")?,
            signed_payouts: count("signed")?,
            confirmed_payouts: count("confirmed")?,
        })
    }

    pub async fn open(path: &Path, genesis: Bytes32, target: Bytes32) -> Result<Self, Error> {
        let mut file_options = std::fs::OpenOptions::new();
        file_options
            .read(true)
            .write(true)
            .create(true)
            .truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            file_options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        if std::fs::symlink_metadata(path)
            .is_ok_and(|metadata| !metadata.is_file() || metadata.file_type().is_symlink())
        {
            return Err(Error::other("pool database must be a regular file"));
        }
        let database = file_options.open(path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            database.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        drop(database);
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(false)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Full)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(5))
            .pragma("trusted_schema", "OFF");
        let mut connection = SqliteConnection::connect_with(&options)
            .await
            .map_err(Error::other)?;
        let mut transaction = connection.begin().await.map_err(Error::other)?;
        for statement in [
            "CREATE TABLE IF NOT EXISTS configuration (id INTEGER PRIMARY KEY CHECK(id=1), value TEXT NOT NULL)",
            "CREATE TABLE IF NOT EXISTS identity (id INTEGER PRIMARY KEY CHECK(id=1), genesis TEXT NOT NULL, target TEXT NOT NULL, schema_version INTEGER NOT NULL)",
            "CREATE TABLE IF NOT EXISTS farmers (launcher TEXT PRIMARY KEY, data TEXT NOT NULL)",
            "CREATE TABLE IF NOT EXISTS partials (id TEXT PRIMARY KEY, launcher TEXT NOT NULL REFERENCES farmers(launcher), points TEXT NOT NULL, received INTEGER NOT NULL, batch TEXT)",
            "CREATE INDEX IF NOT EXISTS unpaid_partials ON partials(batch, received)",
            "CREATE TABLE IF NOT EXISTS batches (reward_coin TEXT PRIMARY KEY, snapshot TEXT NOT NULL, distribution TEXT NOT NULL, bundle TEXT, confirmed_height INTEGER)",
            "CREATE TABLE IF NOT EXISTS batch_requests (reward_coin TEXT PRIMARY KEY REFERENCES batches(reward_coin), parameters TEXT NOT NULL)",
            "CREATE TABLE IF NOT EXISTS reward_claims (id TEXT PRIMARY KEY, data TEXT NOT NULL)",
            "CREATE TABLE IF NOT EXISTS tokens (hash TEXT PRIMARY KEY, launcher TEXT NOT NULL REFERENCES farmers(launcher), key TEXT NOT NULL, expires INTEGER NOT NULL)",
        ] {
            sqlx::query(statement)
                .execute(&mut *transaction)
                .await
                .map_err(Error::other)?;
        }
        sqlx::query("INSERT OR IGNORE INTO identity VALUES (1, ?, ?, 1)")
            .bind(genesis.to_string())
            .bind(target.to_string())
            .execute(&mut *transaction)
            .await
            .map_err(Error::other)?;
        let identity =
            sqlx::query("SELECT genesis, target, schema_version FROM identity WHERE id=1")
                .fetch_one(&mut *transaction)
                .await
                .map_err(Error::other)?;
        if identity
            .try_get::<String, _>("genesis")
            .map_err(Error::other)?
            != genesis.to_string()
            || identity
                .try_get::<String, _>("target")
                .map_err(Error::other)?
                != target.to_string()
            || identity
                .try_get::<i64, _>("schema_version")
                .map_err(Error::other)?
                != 1
        {
            return Err(Error::other(
                "pool database belongs to another chain, pool target, or schema version",
            ));
        }
        transaction.commit().await.map_err(Error::other)?;
        Ok(Self { connection })
    }

    pub async fn farmer(&mut self, launcher: Bytes32) -> Result<Option<Farmer>, Error> {
        sqlx::query("SELECT data FROM farmers WHERE launcher=?")
            .bind(launcher.to_string())
            .fetch_optional(&mut self.connection)
            .await
            .map_err(Error::other)?
            .map(|row| {
                serde_json::from_str(&row.try_get::<String, _>("data").map_err(Error::other)?)
                    .map_err(Error::other)
            })
            .transpose()
    }

    pub async fn bind_configuration(&mut self, value: &str) -> Result<(), Error> {
        sqlx::query("INSERT OR IGNORE INTO configuration VALUES (1, ?)")
            .bind(value)
            .execute(&mut self.connection)
            .await
            .map_err(Error::other)?;
        let stored: String = sqlx::query_scalar("SELECT value FROM configuration WHERE id=1")
            .fetch_one(&mut self.connection)
            .await
            .map_err(Error::other)?;
        if stored != value {
            return Err(Error::other(
                "pool identity, lock height, memoization or fees differ from the database",
            ));
        }
        Ok(())
    }

    pub async fn farmers(&mut self) -> Result<Vec<Farmer>, Error> {
        let rows = sqlx::query("SELECT data FROM farmers ORDER BY launcher LIMIT 1001")
            .fetch_all(&mut self.connection)
            .await
            .map_err(Error::other)?;
        if rows.len() > 1000 {
            return Err(Error::other("reference pool farmer capacity exceeded"));
        }
        rows.into_iter()
            .map(|row| {
                serde_json::from_str(&row.try_get::<String, _>("data").map_err(Error::other)?)
                    .map_err(Error::other)
            })
            .collect()
    }

    pub async fn claims(&mut self) -> Result<Vec<RewardClaim>, Error> {
        let rows = sqlx::query("SELECT data FROM reward_claims ORDER BY id LIMIT 1001")
            .fetch_all(&mut self.connection)
            .await
            .map_err(Error::other)?;
        if rows.len() > 1000 {
            return Err(Error::other(
                "reference pool reward journal capacity exceeded; archival support is required",
            ));
        }
        rows.into_iter()
            .map(|row| {
                serde_json::from_str(&row.try_get::<String, _>("data").map_err(Error::other)?)
                    .map_err(Error::other)
            })
            .collect()
    }

    pub async fn save_claim(&mut self, claim: &RewardClaim) -> Result<(), Error> {
        let data = serde_json::to_string(claim).map_err(Error::other)?;
        if data.len() > 1024 * 1024 {
            return Err(Error::other("reward claim exceeds size limit"));
        }
        sqlx::query("INSERT INTO reward_claims VALUES (?, ?)")
            .bind(claim.reward.name().to_string())
            .bind(data)
            .execute(&mut self.connection)
            .await
            .map_err(Error::other)?;
        Ok(())
    }

    pub async fn register(&mut self, farmer: &Farmer) -> Result<(), Error> {
        if farmer.difficulty == 0 {
            return Err(Error::other("difficulty must be positive"));
        }
        sqlx::query("INSERT INTO farmers(launcher, data) VALUES (?, ?)")
            .bind(farmer.launcher_id.to_string())
            .bind(serde_json::to_string(farmer).map_err(Error::other)?)
            .execute(&mut self.connection)
            .await
            .map_err(Error::other)?;
        Ok(())
    }

    pub async fn update(&mut self, expected: &Farmer, replacement: &Farmer) -> Result<(), Error> {
        if replacement.launcher_id != expected.launcher_id
            || replacement.version != expected.version
            || replacement.owner_public_key != expected.owner_public_key
            || replacement.contract_puzzle_hash != expected.contract_puzzle_hash
            || replacement.difficulty == 0
        {
            return Err(Error::other("invalid farmer update"));
        }
        let changed = sqlx::query("UPDATE farmers SET data=? WHERE launcher=? AND data=?")
            .bind(serde_json::to_string(replacement).map_err(Error::other)?)
            .bind(expected.launcher_id.to_string())
            .bind(serde_json::to_string(expected).map_err(Error::other)?)
            .execute(&mut self.connection)
            .await
            .map_err(Error::other)?
            .rows_affected();
        if changed != 1 {
            return Err(Error::other(
                "farmer settings changed; reload before updating",
            ));
        }
        Ok(())
    }

    pub async fn credit_partial(
        &mut self,
        id: Bytes32,
        farmer: &Farmer,
        received: u64,
    ) -> Result<(), Error> {
        let received = i64::try_from(received).map_err(Error::other)?;
        let changed = sqlx::query("INSERT INTO partials(id, launcher, points, received) SELECT ?, launcher, ?, ? FROM farmers WHERE launcher=? AND data=?")
            .bind(id.to_string()).bind(farmer.difficulty.to_string()).bind(received)
            .bind(farmer.launcher_id.to_string()).bind(serde_json::to_string(farmer).map_err(Error::other)?)
            .execute(&mut self.connection).await.map_err(Error::other)?.rows_affected();
        if changed != 1 {
            return Err(Error::other("farmer changed while verifying partial"));
        }
        Ok(())
    }

    pub async fn points(&mut self, launcher: Bytes32) -> Result<u64, Error> {
        let rows = sqlx::query("SELECT points FROM partials WHERE launcher=? AND batch IS NULL")
            .bind(launcher.to_string())
            .fetch_all(&mut self.connection)
            .await
            .map_err(Error::other)?;
        rows.into_iter().try_fold(0u64, |total, row| {
            let points = row
                .try_get::<String, _>("points")
                .map_err(Error::other)?
                .parse::<u64>()
                .map_err(Error::other)?;
            total
                .checked_add(points)
                .ok_or_else(|| Error::other("farmer points exceed protocol limit"))
        })
    }

    pub async fn has_unpaid_partials(&mut self, cutoff: u64) -> Result<bool, Error> {
        let exists: i64 = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM partials WHERE batch IS NULL AND received<=?)",
        )
        .bind(i64::try_from(cutoff).map_err(Error::other)?)
        .fetch_one(&mut self.connection)
        .await
        .map_err(Error::other)?;
        Ok(exists != 0)
    }

    pub async fn issue_token(
        &mut self,
        farmer: &Farmer,
        now: u64,
        lifetime: u64,
    ) -> Result<(String, u64), Error> {
        if farmer.version != PoolVersion::V2 || lifetime == 0 || lifetime > 3600 {
            return Err(Error::other("invalid token scope or lifetime"));
        }
        let expiration = now
            .checked_add(lifetime)
            .ok_or_else(|| Error::other("token expiry overflow"))?;
        let expiration_sql = i64::try_from(expiration).map_err(Error::other)?;
        let now_sql = i64::try_from(now).map_err(Error::other)?;
        let mut transaction = self.connection.begin().await.map_err(Error::other)?;
        sqlx::query("DELETE FROM tokens WHERE expires<=?")
            .bind(now_sql)
            .execute(&mut *transaction)
            .await
            .map_err(Error::other)?;
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM tokens WHERE launcher=?")
            .bind(farmer.launcher_id.to_string())
            .fetch_one(&mut *transaction)
            .await
            .map_err(Error::other)?;
        if count >= 64 {
            return Err(Error::other("too many active authentication tokens"));
        }
        let random: [u8; 32] = rand::rng().random();
        let token = hex::encode(random);
        sqlx::query("INSERT INTO tokens(hash, launcher, key, expires) VALUES (?, ?, ?, ?)")
            .bind(hex::encode(hash_256(token.as_bytes())))
            .bind(farmer.launcher_id.to_string())
            .bind(farmer.authentication_public_key.to_string())
            .bind(expiration_sql)
            .execute(&mut *transaction)
            .await
            .map_err(Error::other)?;
        transaction.commit().await.map_err(Error::other)?;
        Ok((token, expiration))
    }

    pub async fn verify_token(
        &mut self,
        farmer: &Farmer,
        token: &str,
        now: u64,
    ) -> Result<(), Error> {
        if farmer.version != PoolVersion::V2
            || token.len() != 64
            || !token.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(Error::other("invalid authentication token"));
        }
        let exists: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM tokens WHERE hash=? AND launcher=? AND key=? AND expires>?",
        )
        .bind(hex::encode(hash_256(token.as_bytes())))
        .bind(farmer.launcher_id.to_string())
        .bind(farmer.authentication_public_key.to_string())
        .bind(i64::try_from(now).map_err(Error::other)?)
        .fetch_one(&mut self.connection)
        .await
        .map_err(Error::other)?;
        if exists != 1 {
            return Err(Error::other("invalid or expired authentication token"));
        }
        Ok(())
    }

    pub async fn prepare_distribution(
        &mut self,
        reward_coin: Bytes32,
        amount: u64,
        fee_basis_points: u16,
        transaction_fee: u64,
        cutoff: u64,
    ) -> Result<Distribution, Error> {
        let parameters =
            serde_json::to_string(&(amount, fee_basis_points, transaction_fee, cutoff))
                .map_err(Error::other)?;
        let cutoff = i64::try_from(cutoff).map_err(Error::other)?;
        let mut transaction = self.connection.begin().await.map_err(Error::other)?;
        if let Some(row) = sqlx::query("SELECT distribution FROM batches WHERE reward_coin=?")
            .bind(reward_coin.to_string())
            .fetch_optional(&mut *transaction)
            .await
            .map_err(Error::other)?
        {
            let stored: Option<String> =
                sqlx::query_scalar("SELECT parameters FROM batch_requests WHERE reward_coin=?")
                    .bind(reward_coin.to_string())
                    .fetch_optional(&mut *transaction)
                    .await
                    .map_err(Error::other)?;
            if stored.as_deref() != Some(&parameters) {
                return Err(Error::other(
                    "payout retry parameters differ from the original batch",
                ));
            }
            return serde_json::from_str(
                &row.try_get::<String, _>("distribution")
                    .map_err(Error::other)?,
            )
            .map_err(Error::other);
        }
        let rows = sqlx::query("SELECT farmers.data, partials.points FROM partials JOIN farmers USING(launcher) WHERE batch IS NULL AND received<=? ORDER BY launcher, id")
            .bind(cutoff).fetch_all(&mut *transaction).await.map_err(Error::other)?;
        let mut shares = std::collections::BTreeMap::<[u8; 32], RewardShare>::new();
        for row in rows {
            let farmer: Farmer =
                serde_json::from_str(&row.try_get::<String, _>("data").map_err(Error::other)?)
                    .map_err(Error::other)?;
            let points = row
                .try_get::<String, _>("points")
                .map_err(Error::other)?
                .parse::<u64>()
                .map_err(Error::other)?;
            let share = shares
                .entry(farmer.launcher_id.bytes())
                .or_insert(RewardShare {
                    launcher_id: farmer.launcher_id,
                    payout_puzzle_hash: farmer.payout_puzzle_hash,
                    points: 0,
                });
            share.points = share
                .points
                .checked_add(points)
                .ok_or_else(|| Error::other("reward points overflow"))?;
        }
        let shares: Vec<_> = shares.into_values().collect();
        let distribution = distribute(amount, fee_basis_points, transaction_fee, &shares)?;
        sqlx::query("INSERT INTO batches(reward_coin, snapshot, distribution) VALUES (?, ?, ?)")
            .bind(reward_coin.to_string())
            .bind(serde_json::to_string(&shares).map_err(Error::other)?)
            .bind(serde_json::to_string(&distribution).map_err(Error::other)?)
            .execute(&mut *transaction)
            .await
            .map_err(Error::other)?;
        sqlx::query("INSERT INTO batch_requests VALUES (?, ?)")
            .bind(reward_coin.to_string())
            .bind(parameters)
            .execute(&mut *transaction)
            .await
            .map_err(Error::other)?;
        sqlx::query("UPDATE partials SET batch=? WHERE batch IS NULL AND received<=?")
            .bind(reward_coin.to_string())
            .bind(cutoff)
            .execute(&mut *transaction)
            .await
            .map_err(Error::other)?;
        transaction.commit().await.map_err(Error::other)?;
        Ok(distribution)
    }

    pub async fn batch(&mut self, reward_coin: Bytes32) -> Result<Option<PayoutBatch>, Error> {
        sqlx::query(
            "SELECT distribution, bundle, confirmed_height FROM batches WHERE reward_coin=?",
        )
        .bind(reward_coin.to_string())
        .fetch_optional(&mut self.connection)
        .await
        .map_err(Error::other)?
        .map(|row| {
            Ok(PayoutBatch {
                reward_coin,
                distribution: serde_json::from_str(
                    &row.try_get::<String, _>("distribution")
                        .map_err(Error::other)?,
                )
                .map_err(Error::other)?,
                bundle: row
                    .try_get::<Option<String>, _>("bundle")
                    .map_err(Error::other)?
                    .map(|value| serde_json::from_str(&value).map_err(Error::other))
                    .transpose()?,
                confirmed_height: row
                    .try_get::<Option<i64>, _>("confirmed_height")
                    .map_err(Error::other)?
                    .map(|value| u32::try_from(value).map_err(Error::other))
                    .transpose()?,
            })
        })
        .transpose()
    }

    pub async fn save_signed_batch(
        &mut self,
        reward_coin: Bytes32,
        bundle: &SpendBundle,
    ) -> Result<(), Error> {
        let encoded = serde_json::to_string(bundle).map_err(Error::other)?;
        if encoded.len() > 1024 * 1024
            || bundle.coin_spends.len() != 1
            || bundle
                .coin_spends
                .first()
                .is_none_or(|spend| spend.coin.name() != reward_coin)
        {
            return Err(Error::other(
                "payout must spend exactly its reserved reward input",
            ));
        }
        let changed = sqlx::query("UPDATE batches SET bundle=? WHERE reward_coin=? AND (bundle IS NULL OR bundle=?) AND confirmed_height IS NULL")
            .bind(&encoded).bind(reward_coin.to_string()).bind(&encoded)
            .execute(&mut self.connection).await.map_err(Error::other)?.rows_affected();
        if changed != 1 {
            return Err(Error::other(
                "payout batch is missing, confirmed, or has a different signed transaction",
            ));
        }
        Ok(())
    }

    pub async fn confirm_batch(&mut self, reward_coin: Bytes32, height: u32) -> Result<(), Error> {
        let count = sqlx::query(
            "UPDATE batches SET confirmed_height=? WHERE reward_coin=? AND bundle IS NOT NULL",
        )
        .bind(i64::from(height))
        .bind(reward_coin.to_string())
        .execute(&mut self.connection)
        .await
        .map_err(Error::other)?
        .rows_affected();
        if count != 1 {
            return Err(Error::other("cannot confirm an unsigned or missing payout"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn farmer(index: u8) -> Farmer {
        Farmer {
            version: PoolVersion::V2,
            launcher_id: [index; 32].into(),
            owner_public_key: [index; 48].into(),
            authentication_public_key: [index; 48].into(),
            contract_puzzle_hash: [index; 32].into(),
            payout_puzzle_hash: [index; 32].into(),
            difficulty: 10,
        }
    }

    #[tokio::test]
    async fn restart_deduplication_and_transactional_distribution() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("pool.sqlite");
        let genesis = [1; 32].into();
        let target = [2; 32].into();
        let mut store = PoolStore::open(&path, genesis, target).await.unwrap();
        for index in 1..=3 {
            let farmer = farmer(index);
            store.register(&farmer).await.unwrap();
            store
                .credit_partial([index; 32].into(), &farmer, 100)
                .await
                .unwrap();
            assert!(
                store
                    .credit_partial([index; 32].into(), &farmer, 101)
                    .await
                    .is_err()
            );
            assert_eq!(store.points(farmer.launcher_id).await.unwrap(), 10);
        }
        assert!(
            store
                .prepare_distribution([9; 32].into(), 100, 0, 101, 100)
                .await
                .is_err()
        );
        assert_eq!(store.points(farmer(1).launcher_id).await.unwrap(), 10);
        let distribution = store
            .prepare_distribution([9; 32].into(), 100, 0, 1, 100)
            .await
            .unwrap();
        assert_eq!(distribution.payouts.len(), 3);
        assert_eq!(store.points(farmer(1).launcher_id).await.unwrap(), 0);
        drop(store);
        let mut store = PoolStore::open(&path, genesis, target).await.unwrap();
        assert_eq!(
            store
                .prepare_distribution([9; 32].into(), 100, 0, 1, 100)
                .await
                .unwrap(),
            distribution
        );
        assert!(
            store
                .prepare_distribution([9; 32].into(), 101, 0, 1, 100)
                .await
                .is_err()
        );
        store.bind_configuration("original").await.unwrap();
        store.bind_configuration("original").await.unwrap();
        assert!(store.bind_configuration("changed").await.is_err());
        assert!(
            store
                .prepare_distribution([10; 32].into(), 100, 0, 1, 100)
                .await
                .is_err()
        );
        assert!(
            PoolStore::open(&path, [3; 32].into(), target)
                .await
                .is_err()
        );
        assert!(
            PoolStore::open(&path, genesis, [3; 32].into())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn tokens_are_scoped_expire_and_follow_key_rotation() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = PoolStore::open(
            &directory.path().join("pool.sqlite"),
            [1; 32].into(),
            [2; 32].into(),
        )
        .await
        .unwrap();
        let farmer = farmer(1);
        store.register(&farmer).await.unwrap();
        let (token, expiration) = store.issue_token(&farmer, 100, 300).await.unwrap();
        store.verify_token(&farmer, &token, 399).await.unwrap();
        assert!(
            store
                .verify_token(&farmer, &token, expiration)
                .await
                .is_err()
        );
        let mut changed = farmer.clone();
        changed.authentication_public_key = [2; 48].into();
        store.update(&farmer, &changed).await.unwrap();
        assert!(store.verify_token(&changed, &token, 101).await.is_err());
        assert!(store.update(&farmer, &changed).await.is_err());
        assert!(
            store
                .credit_partial([8; 32].into(), &farmer, 101)
                .await
                .is_err()
        );
        let mut other = changed;
        other.launcher_id = [3; 32].into();
        assert!(store.verify_token(&other, &token, 101).await.is_err());
    }
}
