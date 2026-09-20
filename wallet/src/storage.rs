use crate::accounts::WalletSnapshot;
use dg_xch_core::blockchain::coin_record::CoinRecord;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::blockchain::spend_bundle::SpendBundle;
use dg_xch_core::traits::SizedBytes;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqliteSynchronous};
use sqlx::{Connection, Row, SqliteConnection};
use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::{Error, ErrorKind};
use std::path::Path;
use std::time::Duration;

const SCHEMA_VERSION: i64 = 1;
const MAX_RECORD_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BroadcastStatus {
    #[default]
    Prepared,
    Accepted,
    Rejected,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct StoredTransaction {
    pub bundle: SpendBundle,
    pub created_at: u64,
    pub broadcast: BroadcastStatus,
    pub inputs_spent: bool,
}

#[derive(Clone, Default)]
pub struct StoredWallet {
    pub derivations: u32,
    pub address_index: u32,
    pub peak: Option<Bytes32>,
    pub snapshot: WalletSnapshot,
    pub transactions: Vec<StoredTransaction>,
}

impl StoredWallet {
    pub fn reserved_coins(&self) -> HashSet<Bytes32> {
        self.transactions
            .iter()
            .flat_map(|transaction| transaction.bundle.removals())
            .map(|coin| coin.name())
            .collect()
    }

    pub fn refresh_balances(&mut self) -> Result<(), Error> {
        let reserved = self.reserved_coins();
        self.snapshot.confirmed = 0;
        self.snapshot.spendable = 0;
        for coin in self.snapshot.coins.iter().filter(|coin| !coin.spent) {
            self.snapshot.confirmed = self
                .snapshot
                .confirmed
                .checked_add(u128::from(coin.coin.amount))
                .ok_or_else(|| Error::other("wallet balance overflow"))?;
            if !reserved.contains(&coin.coin.name()) {
                self.snapshot.spendable = self
                    .snapshot
                    .spendable
                    .checked_add(u128::from(coin.coin.amount))
                    .ok_or_else(|| Error::other("spendable balance overflow"))?;
            }
        }
        self.snapshot.pending = self
            .transactions
            .iter()
            .filter(|transaction| !transaction.inputs_spent)
            .map(|transaction| transaction.bundle.name())
            .collect::<Result<_, _>>()?;
        Ok(())
    }
}

pub struct WalletDatabase {
    connection: SqliteConnection,
    _lock: File,
}

impl WalletDatabase {
    pub async fn open(path: &Path, account_id: &str, genesis: Bytes32) -> Result<Self, Error> {
        if account_id.len() != 64 || !account_id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "invalid wallet identity",
            ));
        }
        let directory = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "database needs a directory"))?;
        std::fs::create_dir_all(directory)?;
        if std::fs::symlink_metadata(directory)?
            .file_type()
            .is_symlink()
        {
            return Err(Error::new(
                ErrorKind::PermissionDenied,
                "wallet directory is a symlink",
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))?;
        }
        let lock = private_file(&path.with_extension("sqlite.lock"))?;
        lock.try_lock()
            .map_err(|error| Error::other(format!("wallet database is already in use: {error}")))?;
        let database_file = private_file(path)?;
        drop(database_file);
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(false)
            .foreign_keys(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Full)
            .busy_timeout(Duration::from_secs(30))
            .pragma("trusted_schema", "OFF")
            .pragma("temp_store", "MEMORY")
            .pragma("secure_delete", "ON");
        let connection = SqliteConnection::connect_with(&options)
            .await
            .map_err(Error::other)?;
        let mut database = Self {
            connection,
            _lock: lock,
        };
        database.initialize(account_id, genesis).await?;
        Ok(database)
    }

    async fn initialize(&mut self, account_id: &str, genesis: Bytes32) -> Result<(), Error> {
        let mut transaction = self.connection.begin().await.map_err(Error::other)?;
        let version: i64 = sqlx::query_scalar("PRAGMA user_version")
            .fetch_one(&mut *transaction)
            .await
            .map_err(Error::other)?;
        if version > SCHEMA_VERSION {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "wallet database was created by a newer version",
            ));
        }
        if version == 0 {
            let tables: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'")
                .fetch_one(&mut *transaction).await.map_err(Error::other)?;
            if tables != 0 {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "unrecognized wallet database schema",
                ));
            }
            for statement in [
                "CREATE TABLE identity (singleton INTEGER PRIMARY KEY CHECK (singleton = 1), account_id TEXT NOT NULL, genesis BLOB NOT NULL CHECK (length(genesis) = 32))",
                "CREATE TABLE state (singleton INTEGER PRIMARY KEY CHECK (singleton = 1), derivations INTEGER NOT NULL CHECK (derivations >= 0), address_index INTEGER NOT NULL CHECK (address_index >= 0), peak BLOB, snapshot BLOB NOT NULL)",
                "CREATE TABLE coins (coin_id BLOB PRIMARY KEY CHECK (length(coin_id) = 32), record BLOB NOT NULL) WITHOUT ROWID",
                "CREATE TABLE transactions (transaction_id BLOB PRIMARY KEY CHECK (length(transaction_id) = 32), record BLOB NOT NULL) WITHOUT ROWID",
                "CREATE TABLE reservations (coin_id BLOB PRIMARY KEY CHECK (length(coin_id) = 32), transaction_id BLOB NOT NULL REFERENCES transactions(transaction_id)) WITHOUT ROWID",
                "PRAGMA user_version = 1",
            ] {
                sqlx::query(statement)
                    .execute(&mut *transaction)
                    .await
                    .map_err(Error::other)?;
            }
            sqlx::query("INSERT INTO identity VALUES (1, ?, ?)")
                .bind(account_id)
                .bind(genesis.bytes().to_vec())
                .execute(&mut *transaction)
                .await
                .map_err(Error::other)?;
        }
        let identity = sqlx::query("SELECT account_id, genesis FROM identity WHERE singleton = 1")
            .fetch_one(&mut *transaction)
            .await
            .map_err(Error::other)?;
        let stored_account: String = identity.try_get("account_id").map_err(Error::other)?;
        let stored_genesis: Vec<u8> = identity.try_get("genesis").map_err(Error::other)?;
        if stored_account != account_id || stored_genesis.as_slice() != genesis.bytes() {
            return Err(Error::new(
                ErrorKind::PermissionDenied,
                "wallet database account or genesis does not match",
            ));
        }
        transaction.commit().await.map_err(Error::other)
    }

    pub async fn load(&mut self) -> Result<Option<StoredWallet>, Error> {
        let Some(row) = sqlx::query(
            "SELECT derivations, address_index, peak, snapshot FROM state WHERE singleton = 1",
        )
        .fetch_optional(&mut self.connection)
        .await
        .map_err(Error::other)?
        else {
            return Ok(None);
        };
        let derivations: i64 = row.try_get("derivations").map_err(Error::other)?;
        let address_index: i64 = row.try_get("address_index").map_err(Error::other)?;
        let peak: Option<Vec<u8>> = row.try_get("peak").map_err(Error::other)?;
        let snapshot: Vec<u8> = row.try_get("snapshot").map_err(Error::other)?;
        let mut stored = StoredWallet {
            derivations: u32::try_from(derivations).map_err(Error::other)?,
            address_index: u32::try_from(address_index).map_err(Error::other)?,
            peak: peak.map(|bytes| bytes32(&bytes)).transpose()?,
            snapshot: decode(&snapshot)?,
            transactions: Vec::new(),
        };
        stored.snapshot.coins.clear();
        for row in sqlx::query("SELECT coin_id, record FROM coins ORDER BY coin_id")
            .fetch_all(&mut self.connection)
            .await
            .map_err(Error::other)?
        {
            let name: Vec<u8> = row.try_get("coin_id").map_err(Error::other)?;
            let record: Vec<u8> = row.try_get("record").map_err(Error::other)?;
            let coin: CoinRecord = decode(&record)?;
            if coin.coin.name() != bytes32(&name)? {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "stored coin identity mismatch",
                ));
            }
            stored.snapshot.coins.push(coin);
        }
        stored
            .snapshot
            .coins
            .sort_by_key(|coin| (coin.confirmed_block_index, coin.coin.name().bytes()));
        for row in
            sqlx::query("SELECT transaction_id, record FROM transactions ORDER BY transaction_id")
                .fetch_all(&mut self.connection)
                .await
                .map_err(Error::other)?
        {
            let name: Vec<u8> = row.try_get("transaction_id").map_err(Error::other)?;
            let record: Vec<u8> = row.try_get("record").map_err(Error::other)?;
            let pending: StoredTransaction = decode(&record)?;
            if pending.bundle.name()? != bytes32(&name)? {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "stored transaction identity mismatch",
                ));
            }
            stored.transactions.push(pending);
        }
        stored
            .transactions
            .sort_by_key(|transaction| transaction.created_at);
        let rows = sqlx::query("SELECT coin_id, transaction_id FROM reservations")
            .fetch_all(&mut self.connection)
            .await
            .map_err(Error::other)?;
        let mut reservations = HashSet::new();
        for row in rows {
            let coin_id: Vec<u8> = row.try_get("coin_id").map_err(Error::other)?;
            let transaction_id: Vec<u8> = row.try_get("transaction_id").map_err(Error::other)?;
            reservations.insert((bytes32(&coin_id)?, bytes32(&transaction_id)?));
        }
        let mut expected = HashSet::new();
        for transaction in &stored.transactions {
            let name = transaction.bundle.name()?;
            for removal in transaction.bundle.removals() {
                expected.insert((removal.name(), name));
            }
        }
        if reservations != expected {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "stored coin reservations do not match transaction history",
            ));
        }
        stored.refresh_balances()?;
        Ok(Some(stored))
    }

    pub async fn save(&mut self, state: &StoredWallet) -> Result<(), Error> {
        let mut summary = state.snapshot.clone();
        summary.coins.clear();
        summary.pending.clear();
        let summary = encode(&summary)?;
        let mut transaction = self.connection.begin().await.map_err(Error::other)?;
        sqlx::query("INSERT INTO state VALUES (1, ?, ?, ?, ?) ON CONFLICT(singleton) DO UPDATE SET derivations = excluded.derivations, address_index = excluded.address_index, peak = excluded.peak, snapshot = excluded.snapshot")
            .bind(i64::from(state.derivations))
            .bind(i64::from(state.address_index))
            .bind(state.peak.map(|peak| peak.bytes().to_vec()))
            .bind(summary)
            .execute(&mut *transaction).await.map_err(Error::other)?;
        for statement in [
            "DELETE FROM coins",
            "DELETE FROM reservations",
            "DELETE FROM transactions",
        ] {
            sqlx::query(statement)
                .execute(&mut *transaction)
                .await
                .map_err(Error::other)?;
        }
        for coin in &state.snapshot.coins {
            sqlx::query("INSERT INTO coins VALUES (?, ?)")
                .bind(coin.coin.name().bytes().to_vec())
                .bind(encode(coin)?)
                .execute(&mut *transaction)
                .await
                .map_err(Error::other)?;
        }
        for pending in &state.transactions {
            let name = pending.bundle.name()?;
            sqlx::query("INSERT INTO transactions VALUES (?, ?)")
                .bind(name.bytes().to_vec())
                .bind(encode(pending)?)
                .execute(&mut *transaction)
                .await
                .map_err(Error::other)?;
            for removal in pending.bundle.removals() {
                sqlx::query("INSERT INTO reservations VALUES (?, ?)")
                    .bind(removal.name().bytes().to_vec())
                    .bind(name.bytes().to_vec())
                    .execute(&mut *transaction)
                    .await
                    .map_err(Error::other)?;
            }
        }
        transaction.commit().await.map_err(Error::other)
    }

    pub async fn checkpoint(&mut self) -> Result<(), Error> {
        let result = sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
            .fetch_one(&mut self.connection)
            .await
            .map_err(Error::other)?;
        let busy: i64 = result.try_get(0).map_err(Error::other)?;
        if busy != 0 {
            return Err(Error::other("wallet checkpoint is busy"));
        }
        Ok(())
    }

    pub async fn close(mut self) -> Result<(), Error> {
        self.checkpoint().await?;
        self.connection.close().await.map_err(Error::other)
    }
}

fn private_file(path: &Path) -> Result<File, Error> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => {
            return Err(Error::new(
                ErrorKind::PermissionDenied,
                "wallet path is not a regular file",
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(file)
}

fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, Error> {
    let bytes = serde_json::to_vec(value).map_err(Error::other)?;
    if bytes.len() > MAX_RECORD_BYTES {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "wallet record exceeds size limit",
        ));
    }
    Ok(bytes)
}

fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, Error> {
    if bytes.len() > MAX_RECORD_BYTES {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "wallet record exceeds size limit",
        ));
    }
    serde_json::from_slice(bytes).map_err(Error::other)
}

fn bytes32(bytes: &[u8]) -> Result<Bytes32, Error> {
    let array: [u8; 32] = bytes
        .try_into()
        .map_err(|_| Error::new(ErrorKind::InvalidData, "invalid stored hash length"))?;
    Ok(array.into())
}
