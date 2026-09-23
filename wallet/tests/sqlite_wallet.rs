use blst::min_pk::SecretKey;
use dg_xch_clients::rpc::full_node::FullnodeClient;
use dg_xch_core::blockchain::coin::Coin;
use dg_xch_core::blockchain::coin_record::CoinRecord;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::consensus::constants::SIMULATOR;
use dg_xch_core::utils::hash_256;
use dg_xch_wallet::accounts::{WalletSession, WalletSnapshot};
use dg_xch_wallet::memory_wallet::MemoryWallet;
use dg_xch_wallet::storage::{BroadcastStatus, StoredTransaction, StoredWallet, WalletDatabase};
use dg_xch_wallet::{Wallet, WalletStore};
use sqlx::Connection;
use std::sync::Arc;

fn secret() -> SecretKey {
    SecretKey::key_gen_v3(&[33; 32], &[]).unwrap()
}

fn identity() -> String {
    hex::encode(hash_256(secret().sk_to_pk().to_bytes()))
}

fn genesis() -> Bytes32 {
    [44; 32].into()
}

fn coin(amount: u64) -> CoinRecord {
    CoinRecord {
        coin: Coin {
            parent_coin_info: [55; 32].into(),
            puzzle_hash: [66; 32].into(),
            amount,
        },
        confirmed_block_index: 3,
        spent_block_index: 0,
        coinbase: false,
        timestamp: 123,
        spent: false,
    }
}

fn state() -> StoredWallet {
    StoredWallet {
        derivations: 40,
        address_index: 7,
        peak: Some([88; 32].into()),
        snapshot: WalletSnapshot {
            synced: true,
            height: Some(42),
            receive_puzzle_hash: [77; 32].into(),
            coins: vec![coin(u64::MAX)],
            ..WalletSnapshot::default()
        },
        transactions: Vec::new(),
    }
}

#[tokio::test]
async fn sqlite_restores_exact_balances_and_replaces_reorged_coins() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("wallet.sqlite");
    let mut database = WalletDatabase::open(&path, &identity(), genesis())
        .await
        .unwrap();
    assert!(database.load().await.unwrap().is_none());
    database.save(&state()).await.unwrap();
    database.close().await.unwrap();

    let mut database = WalletDatabase::open(&path, &identity(), genesis())
        .await
        .unwrap();
    let restored = database.load().await.unwrap().unwrap();
    assert_eq!(restored.snapshot.confirmed, u128::from(u64::MAX));
    assert_eq!(restored.snapshot.spendable, u128::from(u64::MAX));
    assert_eq!(restored.derivations, 40);
    assert_eq!(restored.address_index, 7);
    assert_eq!(restored.peak, Some([88; 32].into()));

    let mut replacement = restored.clone();
    replacement.snapshot.height = Some(40);
    replacement.snapshot.coins.clear();
    replacement.peak = Some([99; 32].into());
    database.save(&replacement).await.unwrap();
    let restored = database.load().await.unwrap().unwrap();
    assert!(restored.snapshot.coins.is_empty());
    assert_eq!(restored.snapshot.confirmed, 0);
    assert_eq!(restored.snapshot.height, Some(40));
}

#[tokio::test]
async fn failed_commit_keeps_previous_complete_snapshot() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("wallet.sqlite");
    let mut database = WalletDatabase::open(&path, &identity(), genesis())
        .await
        .unwrap();
    database.save(&state()).await.unwrap();
    let mut invalid = state();
    invalid.derivations = 60;
    invalid.snapshot.height = Some(999);
    invalid.snapshot.coins.push(coin(u64::MAX));
    assert!(database.save(&invalid).await.is_err());
    let restored = database.load().await.unwrap().unwrap();
    assert_eq!(restored.snapshot.height, Some(42));
    assert_eq!(restored.derivations, 40);
    assert_eq!(restored.snapshot.coins.len(), 1);
}

#[tokio::test]
async fn database_rejects_another_account_chain_and_concurrent_writer() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("wallet.sqlite");
    let database = WalletDatabase::open(&path, &identity(), genesis())
        .await
        .unwrap();
    assert!(
        WalletDatabase::open(&path, &identity(), genesis())
            .await
            .is_err()
    );
    database.close().await.unwrap();
    assert!(
        WalletDatabase::open(&path, &"00".repeat(32), genesis())
            .await
            .is_err()
    );
    assert!(
        WalletDatabase::open(&path, &identity(), [45; 32].into())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn newer_schema_is_not_overwritten() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("wallet.sqlite");
    WalletDatabase::open(&path, &identity(), genesis())
        .await
        .unwrap()
        .close()
        .await
        .unwrap();
    let options = sqlx::sqlite::SqliteConnectOptions::new().filename(&path);
    let mut connection = sqlx::SqliteConnection::connect_with(&options)
        .await
        .unwrap();
    sqlx::query("PRAGMA user_version = 99")
        .execute(&mut connection)
        .await
        .unwrap();
    connection.close().await.unwrap();
    assert!(
        WalletDatabase::open(&path, &identity(), genesis())
            .await
            .is_err()
    );
    let mut connection = sqlx::SqliteConnection::connect_with(&options)
        .await
        .unwrap();
    let version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(version, 99);
}

#[tokio::test]
async fn wallet_session_restores_offline_and_migrates_legacy_journal_once() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("wallet.sqlite");
    let journal = path.with_extension("json");
    std::fs::write(&journal, br#"{"derivations":60,"pending":[]}"#).unwrap();
    let session = WalletSession::new(
        secret(),
        FullnodeClient::dummy(),
        Arc::new(SIMULATOR),
        genesis(),
        path.clone(),
    )
    .await
    .unwrap();
    assert!(!session.snapshot().synced);
    drop(session);
    std::fs::write(
        &journal,
        b"invalid older journal must not replace committed database",
    )
    .unwrap();
    let mut database = WalletDatabase::open(&path, &identity(), genesis())
        .await
        .unwrap();
    let migrated = database.load().await.unwrap().unwrap();
    assert_eq!(migrated.derivations, 60);
    database.save(&state()).await.unwrap();
    database.close().await.unwrap();
    let mut session = WalletSession::new(
        secret(),
        FullnodeClient::dummy(),
        Arc::new(SIMULATOR),
        genesis(),
        path,
    )
    .await
    .unwrap();
    let snapshot = session.snapshot();
    assert_eq!(snapshot.height, Some(42));
    assert_eq!(snapshot.confirmed, u128::from(u64::MAX));
    assert!(!snapshot.synced);
    assert!(journal.exists());
    session.checkpoint().await.unwrap();
}

#[tokio::test]
async fn prepared_transaction_reservations_survive_restart_and_reorg() {
    let wallet =
        MemoryWallet::new(secret(), &FullnodeClient::dummy(), Arc::new(SIMULATOR)).unwrap();
    let puzzle_hash = wallet.get_puzzle_hash(false).await.unwrap();
    let mut owned_coin = coin(5_000);
    owned_coin.coin.puzzle_hash = puzzle_hash;
    *wallet
        .wallet_store()
        .lock()
        .await
        .standard_coins()
        .lock()
        .await = vec![owned_coin];
    let bundle = wallet
        .generate_simple_signed_transaction(2_000, 10, [12; 32].into())
        .await
        .unwrap()
        .spend_bundle
        .unwrap();
    let name = bundle.name().unwrap();
    let mut saved = StoredWallet {
        snapshot: WalletSnapshot {
            coins: vec![owned_coin],
            ..WalletSnapshot::default()
        },
        transactions: vec![StoredTransaction {
            bundle,
            created_at: 1234,
            broadcast: BroadcastStatus::Prepared,
            inputs_spent: false,
            offer: None,
        }],
        ..StoredWallet::default()
    };
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("wallet.sqlite");
    let mut database = WalletDatabase::open(&path, &identity(), genesis())
        .await
        .unwrap();
    database.save(&saved).await.unwrap();
    database.close().await.unwrap();
    let mut database = WalletDatabase::open(&path, &identity(), genesis())
        .await
        .unwrap();
    let restored = database.load().await.unwrap().unwrap();
    assert_eq!(restored.snapshot.spendable, 0);
    assert_eq!(restored.snapshot.confirmed, 5_000);
    assert_eq!(restored.snapshot.pending, vec![name]);
    assert_eq!(
        restored.transactions[0].broadcast,
        BroadcastStatus::Prepared
    );
    let mut offered = saved.clone();
    offered.transactions[0].broadcast = BroadcastStatus::Offered;
    offered.transactions[0].offer = Some("persisted offer text".into());
    database.save(&offered).await.unwrap();
    database.close().await.unwrap();
    let mut database = WalletDatabase::open(&path, &identity(), genesis())
        .await
        .unwrap();
    let restored_offer = database.load().await.unwrap().unwrap();
    assert_eq!(restored_offer.snapshot.spendable, 0);
    assert!(restored_offer.snapshot.pending.is_empty());
    assert_eq!(
        restored_offer.transactions[0].offer.as_deref(),
        Some("persisted offer text")
    );
    assert!(
        restored_offer
            .reserved_coins()
            .contains(&owned_coin.coin.name())
    );
    saved.snapshot.coins[0].spent = true;
    saved.transactions[0].inputs_spent = true;
    database.save(&saved).await.unwrap();
    assert!(
        database
            .load()
            .await
            .unwrap()
            .unwrap()
            .snapshot
            .pending
            .is_empty()
    );
    saved.snapshot.coins[0].spent = false;
    saved.transactions[0].inputs_spent = false;
    database.save(&saved).await.unwrap();
    let restored = database.load().await.unwrap().unwrap();
    assert_eq!(restored.snapshot.spendable, 0);
    assert_eq!(restored.snapshot.pending, vec![name]);
}

#[cfg(unix)]
#[tokio::test]
async fn wallet_database_files_are_private_and_reject_symlinks() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("wallet.sqlite");
    let database = WalletDatabase::open(&path, &identity(), genesis())
        .await
        .unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::metadata(directory.path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    database.close().await.unwrap();
    let link = directory.path().join("link.sqlite");
    symlink(&path, &link).unwrap();
    assert!(
        WalletDatabase::open(&link, &identity(), genesis())
            .await
            .is_err()
    );
}
