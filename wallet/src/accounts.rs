use crate::memory_wallet::MemoryWallet;
use crate::storage::{BroadcastStatus, StoredTransaction, StoredWallet, WalletDatabase};
use crate::{Wallet, WalletStore};
use argon2::{Algorithm, Argon2, Params, Version};
use blst::min_pk::SecretKey;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use dg_xch_clients::rpc::full_node::{FullnodeAPI, FullnodeClient};
use dg_xch_core::blockchain::coin_record::CoinRecord;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::blockchain::spend_bundle::SpendBundle;
use dg_xch_core::blockchain::tx_status::TXStatus;
use dg_xch_core::consensus::constants::ConsensusConstants;
use dg_xch_core::utils::hash_256;
use dg_xch_keys::key_from_mnemonic_str;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::{Error, ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{SystemTime, UNIX_EPOCH};
use zeroize::Zeroizing;

const ACCOUNT_VERSION: u32 = 1;
const GAP_LIMIT: u32 = 20;
const MAX_DERIVATIONS: u32 = 100_000;

#[derive(Clone, Serialize, Deserialize)]
pub struct Account {
    pub version: u32,
    pub id: String,
    pub name: String,
    pub network: String,
    pub salt: [u8; 16],
    pub nonce: [u8; 24],
    pub encrypted_key: Vec<u8>,
}

impl Account {
    pub fn import(
        name: String,
        network: String,
        mnemonic: &str,
        password: &str,
    ) -> Result<Self, Error> {
        if password.len() < 12 || name.trim().is_empty() || network.trim().is_empty() {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "name, network and a password of at least 12 bytes are required",
            ));
        }
        let secret = key_from_mnemonic_str(mnemonic)?;
        let id = hex::encode(hash_256(secret.sk_to_pk().to_bytes()));
        let mut account = Self {
            version: ACCOUNT_VERSION,
            id,
            name,
            network,
            salt: rand::random(),
            nonce: rand::random(),
            encrypted_key: Vec::new(),
        };
        let key = account.encryption_key(password)?;
        let plaintext = Zeroizing::new(secret.to_bytes());
        account.encrypted_key = XChaCha20Poly1305::new_from_slice(key.as_ref())
            .map_err(|_| Error::other("invalid encryption key"))?
            .encrypt(
                XNonce::from_slice(&account.nonce),
                Payload {
                    msg: plaintext.as_ref(),
                    aad: &account.associated_data()?,
                },
            )
            .map_err(|_| Error::other("account encryption failed"))?;
        Ok(account)
    }

    fn associated_data(&self) -> Result<Vec<u8>, Error> {
        serde_json::to_vec(&(self.version, &self.id, &self.name, &self.network))
            .map_err(Error::other)
    }

    fn encryption_key(&self, password: &str) -> Result<Zeroizing<[u8; 32]>, Error> {
        if self.version != ACCOUNT_VERSION {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "unsupported account version",
            ));
        }
        let params = Params::new(65_536, 3, 1, Some(32)).map_err(Error::other)?;
        let mut key = Zeroizing::new([0u8; 32]);
        Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
            .hash_password_into(password.as_bytes(), &self.salt, key.as_mut())
            .map_err(Error::other)?;
        Ok(key)
    }

    pub fn unlock(&self, password: &str) -> Result<SecretKey, Error> {
        let key = self.encryption_key(password)?;
        let plaintext = Zeroizing::new(
            XChaCha20Poly1305::new_from_slice(key.as_ref())
                .map_err(|_| Error::other("invalid encryption key"))?
                .decrypt(
                    XNonce::from_slice(&self.nonce),
                    Payload {
                        msg: &self.encrypted_key,
                        aad: &self.associated_data()?,
                    },
                )
                .map_err(|_| {
                    Error::new(
                        ErrorKind::PermissionDenied,
                        "incorrect password or damaged account",
                    )
                })?,
        );
        let secret = SecretKey::from_bytes(&plaintext)
            .map_err(|_| Error::other("invalid account secret"))?;
        if hex::encode(hash_256(secret.sk_to_pk().to_bytes())) != self.id {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "account identity mismatch",
            ));
        }
        Ok(secret)
    }

    pub fn save_new(&self, directory: &Path) -> Result<(), Error> {
        std::fs::create_dir_all(directory)?;
        let path = account_path(directory, &self.id)?;
        let mut output = tempfile::NamedTempFile::new_in(directory)?;
        output.write_all(&serde_json::to_vec(self).map_err(Error::other)?)?;
        output.as_file().sync_all()?;
        output
            .persist_noclobber(path)
            .map_err(|error| error.error)?;
        Ok(())
    }

    pub fn load(directory: &Path, id: &str) -> Result<Self, Error> {
        let account: Self = read_json(&account_path(directory, id)?, 16_384)?;
        if account.id != id {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "account filename does not match identity",
            ));
        }
        Ok(account)
    }
}

pub fn account_path(directory: &Path, id: &str) -> Result<PathBuf, Error> {
    if id.len() != 64 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "invalid account identifier",
        ));
    }
    Ok(directory.join(format!("{id}.json")))
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path, limit: u64) -> Result<T, Error> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "wallet file exceeds size limit",
        ));
    }
    serde_json::from_slice(&bytes).map_err(Error::other)
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct WalletSnapshot {
    #[serde(default)]
    pub synced: bool,
    pub confirmed: u128,
    pub spendable: u128,
    pub pending_change: u128,
    pub height: Option<u32>,
    pub receive_puzzle_hash: Bytes32,
    pub coins: Vec<CoinRecord>,
    pub pending: Vec<Bytes32>,
    #[serde(default)]
    pub assets: Vec<crate::assets::AssetCoin>,
    #[serde(default)]
    pub watched_cats: Vec<Bytes32>,
}

#[derive(Default, Serialize, Deserialize)]
struct WalletJournal {
    derivations: u32,
    pending: Vec<SpendBundle>,
}

pub struct WalletSession {
    wallet: MemoryWallet,
    client: FullnodeClient,
    database: WalletDatabase,
    stored: StoredWallet,
    owned_hashes: HashSet<Bytes32>,
    expected_genesis: Bytes32,
}

impl WalletSession {
    pub async fn new(
        secret: SecretKey,
        client: FullnodeClient,
        constants: Arc<ConsensusConstants>,
        expected_genesis: Bytes32,
        database_path: PathBuf,
    ) -> Result<Self, Error> {
        let database_path = database_path.with_extension("sqlite");
        let account_id = hex::encode(hash_256(secret.sk_to_pk().to_bytes()));
        let mut database =
            WalletDatabase::open(&database_path, &account_id, expected_genesis).await?;
        let mut stored = match database.load().await? {
            Some(stored) => stored,
            None => {
                let journal: WalletJournal =
                    match read_json(&database_path.with_extension("json"), 16 * 1024 * 1024) {
                        Ok(journal) => journal,
                        Err(error) if error.kind() == ErrorKind::NotFound => {
                            WalletJournal::default()
                        }
                        Err(error) => return Err(error),
                    };
                StoredWallet {
                    derivations: journal.derivations,
                    transactions: journal
                        .pending
                        .into_iter()
                        .map(|bundle| StoredTransaction {
                            bundle,
                            created_at: 0,
                            broadcast: BroadcastStatus::Prepared,
                            inputs_spent: false,
                            offer: None,
                        })
                        .collect(),
                    ..StoredWallet::default()
                }
            }
        };
        if stored.derivations > MAX_DERIVATIONS || stored.address_index >= MAX_DERIVATIONS {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "wallet derivation limit exceeded",
            ));
        }
        stored.snapshot.synced = false;
        let wallet = MemoryWallet::new(secret, &client, constants)?;
        wallet
            .wallet_store()
            .lock()
            .await
            .current_index
            .store(stored.address_index, Ordering::Relaxed);
        stored.snapshot.receive_puzzle_hash = wallet.get_puzzle_hash(false).await?;
        stored.refresh_balances()?;
        database.save(&stored).await?;
        let session = Self {
            wallet,
            client,
            database,
            stored,
            owned_hashes: HashSet::new(),
            expected_genesis,
        };
        session.restore_signer_coins().await;
        Ok(session)
    }

    pub fn snapshot(&self) -> WalletSnapshot {
        let mut snapshot = self.stored.snapshot.clone();
        let reserved = self.stored.reserved_coins();
        for asset in &mut snapshot.assets {
            asset.reserved = reserved.contains(&asset.record.coin.name());
        }
        snapshot
    }

    pub async fn watch_cat(&mut self, asset_id: Bytes32) -> Result<(), Error> {
        if self.stored.snapshot.watched_cats.contains(&asset_id) {
            return Ok(());
        }
        if self.stored.snapshot.watched_cats.len() >= 100 {
            return Err(Error::other("CAT watch list limit reached"));
        }
        let mut next = self.stored.clone();
        next.snapshot.watched_cats.push(asset_id);
        next.snapshot.synced = false;
        self.database.save(&next).await?;
        self.stored = next;
        Ok(())
    }

    pub fn transactions(&self) -> &[StoredTransaction] {
        &self.stored.transactions
    }

    async fn restore_signer_coins(&self) {
        let reserved = self.stored.reserved_coins();
        let coins = self
            .stored
            .snapshot
            .coins
            .iter()
            .filter(|coin| !coin.spent && !reserved.contains(&coin.coin.name()))
            .copied()
            .collect();
        let coin_store = self.wallet.wallet_store().lock().await.standard_coins();
        *coin_store.lock().await = coins;
    }

    pub async fn checkpoint(&mut self) -> Result<(), Error> {
        self.database.checkpoint().await
    }

    pub async fn sync(&mut self) -> Result<WalletSnapshot, Error> {
        self.stored.snapshot.synced = false;
        let genesis = self.client.get_block_record_by_height(0).await?;
        if genesis.header_hash != self.expected_genesis {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "node genesis does not match this wallet connection",
            ));
        }
        let before = self.client.get_blockchain_state().await?;
        if !before.sync.synced || before.sync.sync_mode {
            return Err(Error::other("node is not synchronized"));
        }
        let peak = before
            .peak
            .ok_or_else(|| Error::other("node has no peak"))?;
        let mut records = HashMap::new();
        let mut assets = HashMap::new();
        let mut parents = HashMap::new();
        let mut hashes = HashSet::new();
        let mut scanned = 0;
        let mut last_used = 0;
        loop {
            if scanned >= MAX_DERIVATIONS {
                return Err(Error::other(
                    "wallet discovery limit reached; refusing a partial balance",
                ));
            }
            let mut batch = Vec::with_capacity((GAP_LIMIT * 2) as usize);
            for hardened in [false, true] {
                batch.extend(
                    self.wallet
                        .puzzle_hashes(scanned as usize, GAP_LIMIT as usize, hardened)
                        .await?,
                );
            }
            let coins = self
                .client
                .get_coin_records_by_puzzle_hashes(&batch, Some(true), None, None)
                .await?;
            let mut used = !coins.is_empty();
            hashes.extend(batch.iter().copied());
            for hash in &batch {
                let mut hinted = self
                    .client
                    .get_coin_records_by_hint(hash, Some(true), None, None)
                    .await?;
                let wrapped: Vec<_> = self
                    .stored
                    .snapshot
                    .watched_cats
                    .iter()
                    .flat_map(|id| {
                        [
                            crate::assets::AssetKind::Cat1,
                            crate::assets::AssetKind::Cat2,
                        ]
                        .map(|kind| crate::assets::cat_puzzle_hash(kind, *id, *hash))
                    })
                    .collect();
                if !wrapped.is_empty() {
                    hinted.extend(
                        self.client
                            .get_coin_records_by_puzzle_hashes(&wrapped, Some(true), None, None)
                            .await?,
                    );
                }
                if hinted.len() > 1000 {
                    return Err(Error::other(
                        "asset discovery limit exceeded; refusing a partial asset balance",
                    ));
                }
                for record in hinted {
                    if hashes.contains(&record.coin.puzzle_hash) {
                        continue;
                    }
                    if let Some(previous) = assets.get(&record.coin.name()) {
                        let previous: &crate::assets::AssetCoin = previous;
                        if previous.record != record {
                            return Err(Error::other(
                                "node returned conflicting asset coin records",
                            ));
                        }
                        continue;
                    }
                    if record.confirmed_block_index > peak.height
                        || record.spent_block_index > peak.height
                        || record.spent != (record.spent_block_index != 0)
                        || (record.spent && record.spent_block_index < record.confirmed_block_index)
                    {
                        return Err(Error::other("inconsistent asset coin heights"));
                    }
                    let parent_id = record.coin.parent_coin_info;
                    if !parents.contains_key(&parent_id) {
                        if parents.len() >= 1000 {
                            return Err(Error::other("asset discovery parent limit exceeded"));
                        }
                        let parent = self
                            .client
                            .get_coin_record_by_name(&parent_id)
                            .await?
                            .ok_or_else(|| Error::other("asset parent is missing"))?;
                        if !parent.spent || parent.spent_block_index != record.confirmed_block_index
                        {
                            return Err(Error::other("asset parent spend height mismatch"));
                        }
                        let spend = self
                            .client
                            .get_puzzle_and_solution(&parent_id, parent.spent_block_index)
                            .await?;
                        if spend.coin != parent.coin {
                            return Err(Error::other("asset parent spend coin mismatch"));
                        }
                        parents.insert(parent_id, spend);
                    }
                    let parent = parents
                        .get(&parent_id)
                        .ok_or_else(|| Error::other("missing asset parent cache"))?;
                    if let Some(asset) =
                        crate::assets::discover_asset(record, parent.clone(), &hashes)?
                    {
                        used = true;
                        assets.insert(record.coin.name(), asset);
                    }
                }
            }
            for coin in coins {
                if !hashes.contains(&coin.coin.puzzle_hash) {
                    return Err(Error::other("node returned an unrelated wallet coin"));
                }
                if coin.confirmed_block_index > peak.height
                    || coin.spent_block_index > peak.height
                    || coin.spent != (coin.spent_block_index != 0)
                    || (coin.spent && coin.spent_block_index < coin.confirmed_block_index)
                {
                    return Err(Error::other(
                        "node returned inconsistent wallet coin heights",
                    ));
                }
                if let Some(previous) = records.insert(coin.coin.name(), coin)
                    && previous != coin
                {
                    return Err(Error::other(
                        "node returned conflicting versions of a wallet coin",
                    ));
                }
            }
            scanned += GAP_LIMIT;
            if used {
                last_used = scanned;
            }
            if !used
                && scanned
                    >= self
                        .stored
                        .derivations
                        .max(last_used + GAP_LIMIT)
                        .max(self.stored.address_index + GAP_LIMIT)
            {
                break;
            }
        }
        let after = self.client.get_blockchain_state().await?;
        if after.peak.as_ref().map(|record| record.header_hash) != Some(peak.header_hash)
            || !after.sync.synced
            || after.sync.sync_mode
        {
            return Err(Error::other(
                "chain changed during wallet scan; retrying next poll",
            ));
        }
        let mut next = self.stored.clone();
        next.derivations = next.derivations.max(scanned);
        next.peak = Some(peak.header_hash);
        let mut coins: Vec<_> = records.into_values().collect();
        coins.sort_by_key(|coin| coin.confirmed_block_index);
        let spent: HashSet<_> = coins
            .iter()
            .chain(assets.values().map(|asset| &asset.record))
            .filter(|coin| coin.spent)
            .map(|coin| coin.coin.name())
            .collect();
        for transaction in &mut next.transactions {
            let removals = transaction.bundle.removals();
            let input_ids: HashSet<_> = removals.iter().map(|coin| coin.name()).collect();
            transaction.inputs_spent = !removals.is_empty()
                && removals
                    .iter()
                    .filter(|coin| !input_ids.contains(&coin.parent_coin_info))
                    .all(|coin| spent.contains(&coin.name()));
        }
        next.snapshot = WalletSnapshot {
            synced: true,
            height: Some(peak.height),
            receive_puzzle_hash: self.wallet.get_puzzle_hash(false).await?,
            coins,
            assets: assets.into_values().collect(),
            watched_cats: self.stored.snapshot.watched_cats.clone(),
            ..WalletSnapshot::default()
        };
        next.refresh_balances()?;
        refresh_pending_change(&mut next, &hashes)?;
        self.database.save(&next).await?;
        self.stored = next;
        self.owned_hashes = hashes;
        self.restore_signer_coins().await;
        Ok(self.snapshot())
    }

    pub async fn send(
        &mut self,
        destination: Bytes32,
        amount: u64,
        fee: u64,
    ) -> Result<Bytes32, Error> {
        if amount == 0 || amount.checked_add(fee).is_none() {
            return Err(Error::new(ErrorKind::InvalidInput, "invalid amount or fee"));
        }
        self.sync().await?;
        let transaction = self
            .wallet
            .generate_simple_signed_transaction(amount, fee, destination)
            .await?;
        let bundle = transaction
            .spend_bundle
            .ok_or_else(|| Error::other("transaction has no spend bundle"))?;
        self.broadcast(bundle).await
    }

    pub async fn asset_transaction(
        &mut self,
        action: crate::assets::AssetAction,
        fee: u64,
    ) -> Result<Bytes32, Error> {
        self.sync().await?;
        let bundle = crate::assets::build_transaction(
            &self.wallet,
            &self.stored.snapshot.coins,
            &self.stored.snapshot.assets,
            &self.stored.reserved_coins(),
            &action,
            fee,
            self.stored.snapshot.receive_puzzle_hash,
        )
        .await?;
        self.broadcast(bundle).await
    }

    pub async fn create_offer(
        &mut self,
        give: crate::offers::OfferAmount,
        receive: crate::offers::OfferAmount,
        fee: u64,
    ) -> Result<String, Error> {
        self.sync().await?;
        let reserved = self.stored.reserved_coins();
        let prepared = crate::offers::OfferInputs {
            wallet: &self.wallet,
            coins: &self.stored.snapshot.coins,
            assets: &self.stored.snapshot.assets,
            reserved: &reserved,
            change: self.stored.snapshot.receive_puzzle_hash,
        }
        .make(give, receive, fee)
        .await?;
        let mut next = self.stored.clone();
        next.transactions.push(StoredTransaction {
            bundle: prepared.maker_bundle,
            created_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(Error::other)?
                .as_secs(),
            broadcast: BroadcastStatus::Offered,
            inputs_spent: false,
            offer: Some(prepared.text.clone()),
        });
        next.refresh_balances()?;
        self.database.save(&next).await?;
        self.stored = next;
        self.restore_signer_coins().await;
        Ok(prepared.text)
    }

    pub async fn take_offer(&mut self, text: &str, fee: u64) -> Result<Bytes32, Error> {
        self.sync().await?;
        let inputs = crate::offers::inputs(text)?;
        let names: Vec<_> = inputs.iter().map(|coin| coin.name()).collect();
        let records = self
            .client
            .get_coin_records_by_names(&names, Some(true), None, None)
            .await?;
        if records.len() != inputs.len()
            || inputs.iter().any(|coin| {
                !records
                    .iter()
                    .any(|record| record.coin == *coin && !record.spent)
            })
        {
            return Err(Error::other(
                "offer inputs are spent or unavailable on the selected chain",
            ));
        }
        let reserved = self.stored.reserved_coins();
        let bundle = crate::offers::OfferInputs {
            wallet: &self.wallet,
            coins: &self.stored.snapshot.coins,
            assets: &self.stored.snapshot.assets,
            reserved: &reserved,
            change: self.stored.snapshot.receive_puzzle_hash,
        }
        .take(text, fee)
        .await?;
        self.broadcast(bundle).await
    }

    async fn broadcast(&mut self, bundle: SpendBundle) -> Result<Bytes32, Error> {
        self.journal_and_broadcast(bundle, None).await
    }

    pub async fn cancel_offer(&mut self, offer_id: Bytes32, fee: u64) -> Result<Bytes32, Error> {
        self.sync().await?;
        let transaction = self
            .stored
            .transactions
            .iter()
            .find(|transaction| {
                transaction.broadcast == BroadcastStatus::Offered
                    && transaction.bundle.name().ok() == Some(offer_id)
            })
            .ok_or_else(|| Error::other("active offer not found"))?;
        let reserved = self.stored.reserved_coins();
        let bundle = crate::offers::OfferInputs {
            wallet: &self.wallet,
            coins: &self.stored.snapshot.coins,
            assets: &self.stored.snapshot.assets,
            reserved: &reserved,
            change: self.stored.snapshot.receive_puzzle_hash,
        }
        .cancel(&transaction.bundle, fee)
        .await?;
        self.journal_and_broadcast(bundle, Some(offer_id)).await
    }

    async fn journal_and_broadcast(
        &mut self,
        bundle: SpendBundle,
        replacement: Option<Bytes32>,
    ) -> Result<Bytes32, Error> {
        let name = bundle.name()?;
        let mut next = self.stored.clone();
        let offer = if let Some(replacement) = replacement {
            let index = next
                .transactions
                .iter()
                .position(|transaction| {
                    transaction.broadcast == BroadcastStatus::Offered
                        && transaction.bundle.name().ok() == Some(replacement)
                })
                .ok_or_else(|| Error::other("offer journal changed"))?;
            next.transactions.remove(index).offer
        } else {
            None
        };
        next.address_index = self.wallet.wallet_store().lock().await.current_index();
        if next.address_index >= MAX_DERIVATIONS {
            return Err(Error::other("wallet derivation limit exceeded"));
        }
        next.snapshot.receive_puzzle_hash = self.wallet.get_puzzle_hash(false).await?;
        self.owned_hashes.insert(next.snapshot.receive_puzzle_hash);
        next.transactions.push(StoredTransaction {
            bundle: bundle.clone(),
            created_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(Error::other)?
                .as_secs(),
            broadcast: BroadcastStatus::Prepared,
            inputs_spent: false,
            offer,
        });
        next.snapshot.synced = false;
        next.refresh_balances()?;
        refresh_pending_change(&mut next, &self.owned_hashes)?;
        self.database.save(&next).await?;
        self.stored = next;
        self.restore_signer_coins().await;
        let status = self.client.push_tx(&bundle).await?;
        let mut next = self.stored.clone();
        let transaction = next
            .transactions
            .last_mut()
            .ok_or_else(|| Error::other("missing stored transaction"))?;
        transaction.broadcast = match status {
            TXStatus::SUCCESS | TXStatus::PENDING => BroadcastStatus::Accepted,
            TXStatus::FAILED => BroadcastStatus::Rejected,
        };
        self.database.save(&next).await?;
        self.stored = next;
        if status == TXStatus::FAILED {
            return Err(Error::other(
                "node rejected transaction; inputs remain reserved pending reconciliation",
            ));
        }
        Ok(name)
    }
}

fn refresh_pending_change(
    stored: &mut StoredWallet,
    hashes: &HashSet<Bytes32>,
) -> Result<(), Error> {
    stored.snapshot.pending_change = 0;
    let unspent: HashSet<_> = stored
        .snapshot
        .coins
        .iter()
        .filter(|coin| !coin.spent)
        .map(|coin| coin.coin.name())
        .collect();
    for transaction in &stored.transactions {
        if transaction.broadcast == BroadcastStatus::Offered {
            continue;
        }
        if transaction
            .bundle
            .removals()
            .iter()
            .any(|coin| unspent.contains(&coin.name()))
        {
            for coin in transaction
                .bundle
                .additions()?
                .iter()
                .filter(|coin| hashes.contains(&coin.puzzle_hash))
            {
                stored.snapshot.pending_change = stored
                    .snapshot
                    .pending_change
                    .checked_add(u128::from(coin.amount))
                    .ok_or_else(|| Error::other("pending change overflow"))?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MNEMONIC: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

    #[test]
    fn encrypted_accounts_round_trip_and_reject_tampering() {
        let password = "correct horse battery staple";
        let account =
            Account::import("Primary".into(), "testnet11".into(), MNEMONIC, password).unwrap();
        let another =
            Account::import("Primary".into(), "testnet11".into(), MNEMONIC, password).unwrap();
        assert_ne!(account.nonce, another.nonce);
        assert_ne!(account.encrypted_key, another.encrypted_key);
        assert_eq!(
            account.unlock(password).unwrap().to_bytes(),
            key_from_mnemonic_str(MNEMONIC).unwrap().to_bytes()
        );
        assert!(account.unlock("wrong password").is_err());
        let mut changed = account.clone();
        changed.network = "mainnet".into();
        assert!(changed.unlock(password).is_err());
        let directory = tempfile::tempdir().unwrap();
        account.save_new(directory.path()).unwrap();
        assert!(account.save_new(directory.path()).is_err());
        assert_eq!(
            Account::load(directory.path(), &account.id)
                .unwrap()
                .unlock(password)
                .unwrap()
                .to_bytes(),
            account.unlock(password).unwrap().to_bytes()
        );
        assert!(account_path(directory.path(), "../../escape").is_err());
    }
}
