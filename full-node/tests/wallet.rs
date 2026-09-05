// Wallet coin-state subscription server. A subscribed puzzle hash receives the correct CoinStateUpdate
// when a matching coin is created AND when it is spent, across a peak advance. Also covers coin-id
// subscriptions and the registry bounds.

mod common;

use dg_full_node::trust::TrustPolicy;
use dg_full_node::{LimitedSemaphore, WalletNotifier, WalletUpdate};
use dg_xch_core::blockchain::coin::Coin;
use dg_xch_core::blockchain::coin_record::CoinRecord;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::traits::SizedBytes;
use dg_xch_stores::{BlockStore, CoinStore};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

fn h(tag: u8) -> Bytes32 {
    Bytes32::from([tag; 32])
}

fn coin(tag: u8, ph: Bytes32, amount: u64) -> Coin {
    Coin {
        parent_coin_info: h(tag),
        puzzle_hash: ph,
        amount,
    }
}

fn record(c: Coin, height: u32) -> CoinRecord {
    CoinRecord {
        coin: c,
        confirmed_block_index: height,
        spent_block_index: 0,
        coinbase: false,
        timestamp: 0,
        spent: false,
    }
}

// Positional shorthand for the WalletUpdate these tests push.
fn upd<'a>(
    peak_hash: Bytes32,
    height: u32,
    fork_height: u32,
    created: &'a [CoinRecord],
    spent_ids: &'a [Bytes32],
    hints: &'a [(Bytes32, Bytes32)],
) -> WalletUpdate<'a> {
    WalletUpdate {
        peak_hash,
        height,
        fork_height,
        created,
        spent_ids,
        hints,
    }
}

#[tokio::test]
async fn unsubscribed_peak_notifications_do_not_read_spent_coins() {
    let store = common::open_store().await;
    let notifier = WalletNotifier::new();
    let telemetry = store.telemetry().unwrap();
    let peer = h(0xaa);
    let spent_ids = [h(0x01), h(0x02)];
    notifier
        .on_new_peak(&store, upd(h(0xf1), 200, 199, &[], &spent_ids, &[]))
        .await
        .unwrap();
    assert_eq!(
        telemetry
            .coin_reads
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );
    let _receiver = notifier
        .register_for_ph_updates(peer, None, &[h(0x42)])
        .await
        .unwrap();
    notifier
        .on_new_peak(&store, upd(h(0xf2), 201, 200, &[], &spent_ids, &[]))
        .await
        .unwrap();
    assert_eq!(
        telemetry
            .coin_reads
            .load(std::sync::atomic::Ordering::Relaxed),
        2
    );
    notifier.unsubscribe(&peer).await;
    notifier
        .on_new_peak(&store, upd(h(0xf3), 202, 201, &[], &spent_ids, &[]))
        .await
        .unwrap();
    assert_eq!(
        telemetry
            .coin_reads
            .load(std::sync::atomic::Ordering::Relaxed),
        2
    );
}

#[tokio::test]
async fn subscribed_puzzle_hash_receives_create_then_spend() {
    let store = common::open_store().await;
    let notifier = WalletNotifier::new();
    let peer = h(0xaa);
    let ph = h(0x42);
    let c = coin(0x01, ph, 1_000);

    let mut rx = notifier
        .register_for_ph_updates(peer, None, &[ph])
        .await
        .expect("register")
        .0
        .expect("first registration yields a receiver");

    // ---- created at height 200 ----
    store
        .apply_block(200, 200, &[record(c, 200)], &[])
        .await
        .unwrap();
    notifier
        .on_new_peak(&store, upd(h(0xf1), 200, 199, &[record(c, 200)], &[], &[]))
        .await
        .unwrap();

    let update = rx.recv().await.expect("create update");
    assert_eq!(update.height, 200);
    assert_eq!(update.items.len(), 1);
    let cs = &update.items[0];
    assert_eq!(cs.coin.name(), c.name());
    assert_eq!(cs.created_height, Some(200));
    assert_eq!(cs.spent_height, None);

    // ---- spent at height 201 ----
    store.apply_block(201, 201, &[], &[c.name()]).await.unwrap();
    notifier
        .on_new_peak(&store, upd(h(0xf2), 201, 200, &[], &[c.name()], &[]))
        .await
        .unwrap();

    let update = rx.recv().await.expect("spend update");
    assert_eq!(update.height, 201);
    assert_eq!(update.items.len(), 1);
    let cs = &update.items[0];
    assert_eq!(cs.coin.name(), c.name());
    assert_eq!(cs.created_height, Some(200));
    assert_eq!(cs.spent_height, Some(201));
}

#[tokio::test]
async fn subscribed_coin_id_receives_update() {
    let store = common::open_store().await;
    let notifier = WalletNotifier::new();
    let peer = h(0xbb);
    let c = coin(0x02, h(0x77), 500);

    let mut rx = notifier
        .register_for_coin_updates(peer, None, &[c.name()])
        .await
        .expect("register")
        .0
        .expect("receiver");

    store
        .apply_block(300, 300, &[record(c, 300)], &[])
        .await
        .unwrap();
    notifier
        .on_new_peak(&store, upd(h(0xf3), 300, 299, &[record(c, 300)], &[], &[]))
        .await
        .unwrap();

    let update = rx.recv().await.expect("update");
    assert_eq!(update.items.len(), 1);
    assert_eq!(update.items[0].coin.name(), c.name());
    assert_eq!(update.items[0].created_height, Some(300));
}

// A coin whose HINT equals a subscribed puzzle hash matches on the live push — the
// `update_wallets` joins `peers_for_puzzle_hash(hint)` alongside the coin's own puzzle hash and
// id. This is how a wallet subscribed to a CAT/DID/NFT outer puzzle
// hash sees the inner-puzzle coin land WITHOUT polling. The pairs arrive as the engine's
// `BlockDelta::hints` (hint, created_coin_id).
#[tokio::test]
async fn hinted_puzzle_hash_subscription_receives_create_and_same_block_spend() {
    let store = common::open_store().await;
    let notifier = WalletNotifier::new();
    let peer = h(0xdd);
    let hint = h(0x66);
    // the coin's OWN puzzle hash differs from the subscribed hash — only the hint matches
    let created = coin(0x04, h(0x67), 123);
    let spent = coin(0x05, h(0x68), 456);

    let mut rx = notifier
        .register_for_ph_updates(peer, None, &[hint])
        .await
        .expect("register")
        .0
        .expect("receiver");

    // `spent` was created (hintless) earlier; at height 500 `created` lands hinted to `hint` and
    // `spent` — hinted to `hint` in THIS block — is consumed.
    store
        .apply_block(499, 499, &[record(spent, 499)], &[])
        .await
        .unwrap();
    store
        .apply_block(500, 500, &[record(created, 500)], &[spent.name()])
        .await
        .unwrap();
    notifier
        .on_new_peak(
            &store,
            upd(
                h(0xf5),
                500,
                499,
                &[record(created, 500)],
                &[spent.name()],
                &[(hint, created.name()), (hint, spent.name())],
            ),
        )
        .await
        .unwrap();

    let update = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("a hint-matched CoinStateUpdate must be pushed")
        .expect("delivery channel open");
    assert_eq!(update.height, 500);
    assert!(
        update
            .items
            .iter()
            .any(|cs| cs.coin.name() == created.name() && cs.created_height == Some(500)),
        "the hinted CREATED coin is pushed to the hint subscriber"
    );
    assert!(
        update
            .items
            .iter()
            .any(|cs| cs.coin.name() == spent.name() && cs.spent_height == Some(500)),
        "a coin hinted in this block and spent in it is pushed too"
    );

    // The hint map covers only THIS peak's create-coin hints: a later
    // spend of `created` (whose hint is not re-declared) does not hint-match.
    store
        .apply_block(501, 501, &[], &[created.name()])
        .await
        .unwrap();
    notifier
        .on_new_peak(&store, upd(h(0xf7), 501, 500, &[], &[created.name()], &[]))
        .await
        .unwrap();
    assert!(
        rx.try_recv().is_err(),
        "no hint re-declaration, no coin-id subscription => no push"
    );
}

#[tokio::test]
async fn unmatched_peak_delivers_nothing() {
    let store = common::open_store().await;
    let notifier = WalletNotifier::new();
    let peer = h(0xcc);
    let mut rx = notifier
        .register_for_ph_updates(peer, None, &[h(0x11)])
        .await
        .unwrap()
        .0
        .unwrap();

    // a coin with a different puzzle hash — no match, no update
    let other = coin(0x03, h(0x99), 10);
    store
        .apply_block(400, 400, &[record(other, 400)], &[])
        .await
        .unwrap();
    notifier
        .on_new_peak(
            &store,
            upd(h(0xf4), 400, 399, &[record(other, 400)], &[], &[]),
        )
        .await
        .unwrap();

    assert!(rx.try_recv().is_err(), "no update for an unmatched peak");
}

#[tokio::test]
async fn second_registration_returns_no_new_receiver() {
    let notifier = WalletNotifier::new();
    let peer = h(0xdd);
    let (first, _) = notifier
        .register_for_ph_updates(peer, None, &[h(0x01)])
        .await
        .unwrap();
    assert!(first.is_some());
    let (second, _) = notifier
        .register_for_coin_updates(peer, None, &[h(0x02)])
        .await
        .unwrap();
    assert!(second.is_none(), "one channel per peer");
    assert_eq!(notifier.subscriber_count().await, 1);

    notifier.unsubscribe(&peer).await;
    assert_eq!(notifier.subscriber_count().await, 0);
}

// The per-peer combined subscription cap for an UNTRUSTED peer is 200,000. The default notifier
// trusts no one, so every peer resolves untrusted.
#[tokio::test]
async fn subscription_cap_matches_chia_untrusted_max_subscribe_items() {
    assert_eq!(
        WalletNotifier::new().max_subscriptions(&h(0x01), None),
        200_000
    );
}

#[tokio::test]
async fn subscription_cap_is_trusted_for_configured_node_id() {
    let trusted = h(0xaa);
    let untrusted = h(0xbb);
    let notifier = WalletNotifier::with_trust(Arc::new(TrustPolicy::new(HashSet::from([trusted]))));
    assert_eq!(notifier.max_subscriptions(&trusted, None), 2_000_000);
    assert_eq!(notifier.max_subscriptions(&untrusted, None), 200_000);
}

// Behavioral, small scale: a trusted peer may register MORE puzzle-hash
// subscriptions than an untrusted one — the per-peer add truncation honors the trusted cap. Untrusted
// cap 2, trusted cap 4; register four hashes each and assert the added counts diverge by tier.
#[tokio::test]
async fn trusted_peer_registers_past_the_untrusted_cap() {
    let trusted = h(0xaa);
    let untrusted = h(0xbb);
    // untrusted sub cap 2, trusted sub cap 4 (response caps irrelevant here).
    let policy = TrustPolicy::with_caps(HashSet::from([trusted]), 2, 4, 2, 4);
    let notifier = WalletNotifier::with_trust_and_subscribers(64, Arc::new(policy));
    let hashes = [h(0x01), h(0x02), h(0x03), h(0x04)];

    let (_rx, added_untrusted) = notifier
        .register_for_ph_updates(untrusted, None, &hashes)
        .await
        .expect("register untrusted");
    assert_eq!(
        added_untrusted.len(),
        2,
        "untrusted truncates at the 200k-tier cap (2 here)"
    );

    let (_rx, added_trusted) = notifier
        .register_for_ph_updates(trusted, None, &hashes)
        .await
        .expect("register trusted");
    assert_eq!(
        added_trusted.len(),
        4,
        "trusted truncates at the 2M-tier cap (4 here)"
    );
}

// `add_puzzle_subscriptions` returns ONLY the newly-added subscriptions — in-request duplicates,
// already-subscribed hashes, and the over-cap overflow are all filtered from the returned set
//. The register handler feeds that set (not the raw request) to the
// initial-state query.
#[tokio::test]
async fn register_reports_only_newly_added_subscriptions() {
    let notifier = WalletNotifier::with_limits(8, 2);
    let peer = h(0xee);
    let (rx, added) = notifier
        .register_for_ph_updates(peer, None, &[h(0x01), h(0x01), h(0x02), h(0x03)])
        .await
        .expect("register");
    assert!(rx.is_some());
    assert_eq!(
        added,
        vec![h(0x01), h(0x02)],
        "the in-request duplicate is deduped and the over-cap hash is dropped"
    );

    let (rx, added) = notifier
        .register_for_ph_updates(peer, None, &[h(0x01), h(0x02)])
        .await
        .expect("register");
    assert!(rx.is_none(), "one channel per peer");
    assert!(
        added.is_empty(),
        "already-subscribed hashes are not reported as added"
    );
}

// `request_remove_puzzle_subscriptions` / request_remove_coin_subscriptions semantics on the
// registry (+ ): Some(list) removes the listed
// subset returning only what was actually subscribed (duplicates and never-subscribed items
// filtered), None clears ALL returning the prior set — and the reverse index is scrubbed, so a
// removed subscription delivers nothing on the next peak while the peer's channel stays alive
// for a re-subscribe.
#[tokio::test]
async fn remove_subscriptions_subset_and_all_scrub_delivery() {
    let store = common::open_store().await;
    let notifier = WalletNotifier::new();
    let peer = h(0xa0);
    let (rx, _) = notifier
        .register_for_ph_updates(peer, None, &[h(0x01), h(0x02), h(0x03)])
        .await
        .expect("register");
    let mut rx = rx.expect("receiver");
    notifier
        .register_for_coin_updates(peer, None, &[h(0x11), h(0x12)])
        .await
        .expect("register coins");
    assert_eq!(notifier.peer_subscription_count(&peer).await, 5);

    // Subset removal: the duplicate and the never-subscribed hash are filtered from the answer.
    let removed = notifier
        .remove_ph_subscriptions(&peer, Some(&[h(0x01), h(0x01), h(0x77)]))
        .await;
    assert_eq!(removed, vec![h(0x01)]);
    assert_eq!(notifier.peer_subscription_count(&peer).await, 4);

    // The removed hash no longer delivers; a still-subscribed one does.
    let gone = coin(0x21, h(0x01), 10);
    let live = coin(0x22, h(0x02), 20);
    store
        .apply_block(500, 500, &[record(gone, 500), record(live, 500)], &[])
        .await
        .unwrap();
    notifier
        .on_new_peak(
            &store,
            upd(
                h(0xf5),
                500,
                499,
                &[record(gone, 500), record(live, 500)],
                &[],
                &[],
            ),
        )
        .await
        .unwrap();
    let update = rx.recv().await.expect("update for the live subscription");
    assert_eq!(
        update.items.len(),
        1,
        "only the still-subscribed hash matches"
    );
    assert_eq!(update.items[0].coin.name(), live.name());

    // None = remove ALL, returning the prior set; both legs.
    let mut removed_all = notifier.remove_ph_subscriptions(&peer, None).await;
    removed_all.sort_by_key(|b| b.bytes());
    assert_eq!(removed_all, vec![h(0x02), h(0x03)]);
    let mut removed_coins = notifier.remove_coin_subscriptions(&peer, None).await;
    removed_coins.sort_by_key(|b| b.bytes());
    assert_eq!(removed_coins, vec![h(0x11), h(0x12)]);
    assert_eq!(notifier.peer_subscription_count(&peer).await, 0);

    // A never-registered peer removes nothing.
    assert!(
        notifier
            .remove_ph_subscriptions(&h(0xEE), None)
            .await
            .is_empty()
    );

    // Nothing subscribed → the next peak delivers nothing, but the channel is still open
    // (the connection is kept; a re-subscribe reuses it — no new receiver).
    notifier
        .on_new_peak(
            &store,
            upd(h(0xf6), 501, 500, &[record(live, 501)], &[], &[]),
        )
        .await
        .unwrap();
    assert!(rx.try_recv().is_err(), "no update after remove-all");
    let (again, added) = notifier
        .register_for_ph_updates(peer, None, &[h(0x02)])
        .await
        .expect("re-subscribe");
    assert!(again.is_none(), "one channel per peer, still alive");
    assert_eq!(added, vec![h(0x02)]);
}

// LimitedSemaphore: `active_limit` concurrent holders + `waiting_limit`
// queued waiters; one more acquire fails IMMEDIATELY (LimitedSemaphoreFullError) instead of queueing.
#[tokio::test]
async fn limited_semaphore_rejects_beyond_active_plus_waiting() {
    let sem = Arc::new(LimitedSemaphore::new(1, 1));

    // Active slot taken.
    let p1 = sem
        .acquire()
        .await
        .expect("first acquire holds the active slot");

    // Waiting slot taken (parked until p1 drops).
    let sem2 = sem.clone();
    let waiter = tokio::spawn(async move {
        let _p = sem2
            .acquire()
            .await
            .expect("the waiter eventually acquires");
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    // active + waiting exhausted → immediate rejection, no unbounded queueing.
    assert!(
        sem.acquire().await.is_err(),
        "an acquire beyond active + waiting must fail immediately"
    );

    // Releasing the active permit lets the parked waiter through.
    drop(p1);
    tokio::time::timeout(Duration::from_secs(5), waiter)
        .await
        .expect("the waiter must be released")
        .expect("waiter task");

    // Slots restored: a fresh acquire succeeds again.
    assert!(sem.acquire().await.is_ok());
}

// Cross-crate pin: the store-blind copy of `MAX_PUZZLE_HASH_BATCH_SIZE` in the p2p handler layer
// (the decode-time list cap on `request_puzzle_state.puzzle_hashes`) must equal the store layer's
// authoritative constant. The two crates cannot depend on each other, so this test is the tie.
#[test]
fn p2p_puzzle_hash_batch_cap_matches_the_store_constant() {
    assert_eq!(
        dg_xch_p2p::handlers::MAX_PUZZLE_HASH_BATCH_SIZE as usize,
        dg_xch_stores::traits::MAX_PUZZLE_HASH_BATCH_SIZE,
        "p2p decode cap and store batch bound must stay in lockstep"
    );
}
