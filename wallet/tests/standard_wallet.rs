use blst::min_pk::SecretKey;
use dg_xch_clients::rpc::full_node::FullnodeClient;
use dg_xch_core::blockchain::coin::Coin;
use dg_xch_core::blockchain::coin_record::CoinRecord;
use dg_xch_core::blockchain::wallet_type::AmountWithPuzzleHash;
use dg_xch_core::consensus::constants::SIMULATOR;
use dg_xch_wallet::memory_wallet::MemoryWallet;
use dg_xch_wallet::{Wallet, WalletStore};
use std::sync::Arc;

async fn wallet() -> (MemoryWallet, Vec<CoinRecord>) {
    let secret = SecretKey::key_gen_v3(&[7; 32], &[]).unwrap();
    let wallet = MemoryWallet::new(secret, &FullnodeClient::dummy(), Arc::new(SIMULATOR)).unwrap();
    let puzzle_hash = wallet.get_puzzle_hash(false).await.unwrap();
    let coins: Vec<_> = [700, 800]
        .into_iter()
        .enumerate()
        .map(|(index, amount)| CoinRecord {
            coin: Coin {
                parent_coin_info: [index as u8; 32].into(),
                puzzle_hash,
                amount,
            },
            confirmed_block_index: 1,
            spent_block_index: 0,
            coinbase: false,
            timestamp: 0,
            spent: false,
        })
        .collect();
    let coin_store = wallet.wallet_store().lock().await.standard_coins();
    *coin_store.lock().await = coins.clone();
    (wallet, coins)
}

#[tokio::test]
async fn multi_input_payment_creates_outputs_once() {
    let (wallet, coins) = wallet().await;
    let payment = AmountWithPuzzleHash {
        puzzle_hash: [9; 32].into(),
        amount: 1_000,
        memos: Vec::new(),
    };
    let bundle = wallet
        .create_spend_bundle(
            vec![payment],
            &coins,
            Some(coins[0].coin.puzzle_hash),
            false,
            10,
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(bundle.coin_spends.len(), 2);
    let additions = bundle.additions().unwrap();
    assert_eq!(additions.len(), 2);
    assert_eq!(additions.iter().map(|coin| coin.amount).sum::<u64>(), 1_490);
    assert_eq!(
        additions
            .iter()
            .filter(|coin| coin.puzzle_hash == [9; 32].into())
            .count(),
        1
    );
}

#[tokio::test]
async fn signing_rejects_invalid_inputs_and_tracks_spent_balance() {
    let (wallet, coins) = wallet().await;
    assert!(
        wallet
            .create_spend_bundle(Vec::new(), &coins, None, true, -1, None, None)
            .await
            .is_err()
    );
    assert!(
        wallet
            .create_spend_bundle(Vec::new(), &[coins[0], coins[0]], None, true, 0, None, None)
            .await
            .is_err()
    );
    assert!(
        wallet
            .generate_simple_signed_transaction(u64::MAX, 1, [9; 32].into())
            .await
            .is_err()
    );
    let coin_store = wallet.wallet_store().lock().await.standard_coins();
    coin_store.lock().await[0].spent = true;
    assert_eq!(
        wallet
            .wallet_store()
            .lock()
            .await
            .get_confirmed_balance()
            .await,
        800
    );
    assert_eq!(
        wallet
            .wallet_store()
            .lock()
            .await
            .get_unconfirmed_balance()
            .await,
        800
    );
}

#[tokio::test]
async fn standard_send_conserves_inputs_and_fee() {
    let (wallet, _) = wallet().await;
    let transaction = wallet
        .generate_simple_signed_transaction(1_000, 10, [9; 32].into())
        .await
        .unwrap();
    assert_eq!(
        transaction
            .removals
            .iter()
            .map(|coin| coin.amount)
            .sum::<u64>(),
        1_500
    );
    assert_eq!(
        transaction
            .additions
            .iter()
            .map(|coin| coin.amount)
            .sum::<u64>(),
        1_490
    );
    assert!(transaction.spend_bundle.is_some());
}

#[tokio::test]
async fn coin_selection_uses_wide_balance_totals() {
    let (wallet, mut coins) = wallet().await;
    coins[0].coin.amount = u64::MAX;
    coins[1].coin.amount = u64::MAX - 1;
    let store = wallet.wallet_store();
    let coin_store = store.lock().await.standard_coins();
    *coin_store.lock().await = coins;
    let selected = store
        .lock()
        .await
        .select_coins(10, None, None, u64::MAX, None)
        .await
        .unwrap();
    assert_eq!(selected.len(), 1);
}

#[tokio::test]
async fn exhausted_derivation_indices_return_errors() {
    use std::sync::atomic::Ordering;
    let (wallet, _) = wallet().await;
    wallet
        .wallet_store()
        .lock()
        .await
        .current_index
        .store(u32::MAX, Ordering::Relaxed);
    assert!(wallet.get_puzzle_hash(true).await.is_err());
    assert!(wallet.puzzle_hashes(usize::MAX, 1, false).await.is_err());
}
