use crate::trust::TrustPolicy;
use dg_xch_core::blockchain::coin_record::CoinRecord;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::protocols::wallet::{CoinState, CoinStateUpdate};
use dg_xch_stores::{CoinStore, StoreError};
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt;
use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use tokio::sync::RwLock;
use tokio::sync::mpsc::{Receiver, Sender, channel};
use tokio::sync::{Semaphore, SemaphorePermit};

// Every registry dimension is a hard bound. A public wallet peer cannot grow the node's
// memory by registering unboundedly, and a slow wallet cannot stall the peak path (its channel is bounded and
// updates are dropped, not blocked on).
//
// MAX_SUBSCRIBERS caps the registry itself. The per-peer combined puzzle-hash + coin-id cap
// comes from the shared [`TrustPolicy`] keyed on the peer's cert-hash node id: the untrusted
// `max_subscribe_items` (200,000) by default, the trusted `trusted_max_subscribe_items`
// (2,000,000) for a configured trusted peer. An empty `trusted_peers` config leaves every remote
// peer untrusted.
const MAX_SUBSCRIBERS: usize = 4096;
const CHANNEL_CAPACITY: usize = 256;

#[derive(Debug)]
pub enum WalletError {
    TooManySubscribers,
    TooManyItems,
    Store(StoreError),
}

impl fmt::Display for WalletError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WalletError::TooManySubscribers => write!(f, "subscriber registry at capacity"),
            WalletError::TooManyItems => write!(f, "subscriber interest set at capacity"),
            WalletError::Store(e) => write!(f, "store error: {e}"),
        }
    }
}

impl Error for WalletError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            WalletError::Store(e) => Some(e),
            _ => None,
        }
    }
}

impl From<StoreError> for WalletError {
    fn from(e: StoreError) -> Self {
        WalletError::Store(e)
    }
}

struct Subscriber {
    puzzle_hashes: HashSet<Bytes32>,
    coin_ids: HashSet<Bytes32>,
    tx: Sender<CoinStateUpdate>,
}

impl Subscriber {
    fn items(&self) -> usize {
        self.puzzle_hashes.len() + self.coin_ids.len()
    }
}

#[derive(Default)]
struct Registry {
    subs: HashMap<Bytes32, Subscriber>,
    // reverse indexes so a new peak matches its coin deltas to interested peers in O(delta), not O(subs).
    by_ph: HashMap<Bytes32, HashSet<Bytes32>>,
    by_coin: HashMap<Bytes32, HashSet<Bytes32>>,
}

// Remove a peer's subscriber entry and scrub it from both reverse indexes (empty index buckets are
// removed so the maps stay bounded). Dropping the `Subscriber` drops its channel `Sender`.
fn drop_peer(reg: &mut Registry, peer: &Bytes32) {
    let Some(sub) = reg.subs.remove(peer) else {
        return;
    };
    for ph in sub.puzzle_hashes {
        if let Some(set) = reg.by_ph.get_mut(&ph) {
            set.remove(peer);
            if set.is_empty() {
                reg.by_ph.remove(&ph);
            }
        }
    }
    for id in sub.coin_ids {
        if let Some(set) = reg.by_coin.get_mut(&id) {
            set.remove(peer);
            if set.is_empty() {
                reg.by_coin.remove(&id);
            }
        }
    }
}

impl Registry {
    // The set of peers interested in a coin: by its puzzle hash, by its id, or by its HINT
    // treated as a puzzle-hash subscription — which is how a wallet subscribed to a hint (the
    // outer puzzle hash of a CAT/DID/NFT) sees the inner-puzzle coin land.
    fn match_peers(
        &self,
        puzzle_hash: Bytes32,
        coin_id: Bytes32,
        hint: Option<&Bytes32>,
    ) -> HashSet<Bytes32> {
        let mut peers = HashSet::new();
        if let Some(p) = self.by_ph.get(&puzzle_hash) {
            peers.extend(p.iter().copied());
        }
        if let Some(p) = self.by_coin.get(&coin_id) {
            peers.extend(p.iter().copied());
        }
        if let Some(h) = hint
            && let Some(p) = self.by_ph.get(h)
        {
            peers.extend(p.iter().copied());
        }
        peers
    }
}

// The wallet coin-state subscription server. A wallet peer registers interest in puzzle hashes /
// coin ids; on every new peak the node emits a `CoinStateUpdate` carrying the matching coins that were
// created or spent. Keyed on the peer id (the cert-hash identity in production). Delivery is a bounded
// per-peer channel the server's wire task forwards to the socket - a slow wallet drops updates, never
// backs pressure into the peak path.
pub struct WalletNotifier {
    inner: RwLock<Registry>,
    max_subscribers: usize,
    // The shared trusted-peer policy: resolves the per-peer subscription cap (untrusted vs trusted)
    // from the peer's cert-hash node id. Shared as an `Arc` with the full-node api and tx queue so a
    // node has ONE source of truth for who is trusted.
    trust: Arc<TrustPolicy>,
    channel_capacity: usize,
}

impl Default for WalletNotifier {
    fn default() -> Self {
        Self::new()
    }
}

impl WalletNotifier {
    /// A registry at the stock caps with an empty trusted set — every peer untrusted. Production
    /// injects a config-derived policy via [`WalletNotifier::with_trust`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(Registry::default()),
            max_subscribers: MAX_SUBSCRIBERS,
            trust: Arc::new(TrustPolicy::default()),
            channel_capacity: CHANNEL_CAPACITY,
        }
    }

    /// A registry wired to a shared [`TrustPolicy`] — the production constructor. The policy decides
    /// each peer's subscription cap; the same `Arc` backs the api's response-item cap and the tx
    /// queue's priority tier. Registry size stays under the [`MAX_SUBSCRIBERS`] bound.
    #[must_use]
    pub fn with_trust(trust: Arc<TrustPolicy>) -> Self {
        Self {
            inner: RwLock::new(Registry::default()),
            max_subscribers: MAX_SUBSCRIBERS,
            trust,
            channel_capacity: CHANNEL_CAPACITY,
        }
    }

    /// A registry with a custom subscriber bound AND an explicit trust policy — the test seam for
    /// driving the trusted/untrusted subscription-cap split at small scale.
    #[must_use]
    pub fn with_trust_and_subscribers(max_subscribers: usize, trust: Arc<TrustPolicy>) -> Self {
        Self {
            inner: RwLock::new(Registry::default()),
            max_subscribers,
            trust,
            channel_capacity: CHANNEL_CAPACITY,
        }
    }

    /// A registry with test-scale bounds (production uses [`WalletNotifier::with_trust`]). Both the
    /// untrusted and trusted per-peer caps are set to `max_items_per_subscriber` with an empty trusted
    /// set, so every peer resolves to that single cap.
    #[must_use]
    pub fn with_limits(max_subscribers: usize, max_items_per_subscriber: usize) -> Self {
        let trust = TrustPolicy::with_caps(
            HashSet::new(),
            max_items_per_subscriber,
            max_items_per_subscriber,
            max_items_per_subscriber,
            max_items_per_subscriber,
        );
        Self::with_trust_and_subscribers(max_subscribers, Arc::new(trust))
    }

    /// The per-peer combined subscription cap for `peer`: trusted peers get
    /// `trusted_max_subscribe_items`, everyone else `max_subscribe_items`. The register handlers
    /// slice / filter against it. `host` is the peer's remote IP (localhost / trusted-CIDR peers
    /// resolve trusted; `None` = node-id trust only).
    #[must_use]
    pub fn max_subscriptions(&self, peer: &Bytes32, host: Option<IpAddr>) -> usize {
        self.trust.max_subscriptions(peer, host)
    }

    pub async fn subscriber_count(&self) -> usize {
        self.inner.read().await.subs.len()
    }

    /// A peer's combined puzzle-hash + coin-id subscription count. The
    /// RequestPuzzleState/RequestCoinState handlers check `request items + this` against
    /// [`WalletNotifier::max_subscriptions`] before subscribing.
    pub async fn peer_subscription_count(&self, peer: &Bytes32) -> usize {
        self.inner
            .read()
            .await
            .subs
            .get(peer)
            .map_or(0, Subscriber::items)
    }

    /// Drop a peer's puzzle-hash subscriptions: `None` clears ALL (returning the prior set),
    /// `Some` removes the listed subset (returning only what was actually subscribed —
    /// in-request duplicates and never-subscribed hashes are filtered out). The peer's delivery
    /// channel STAYS: the connection is kept, and a later re-subscribe reuses it (one channel
    /// per peer).
    pub async fn remove_ph_subscriptions(
        &self,
        peer: &Bytes32,
        puzzle_hashes: Option<&[Bytes32]>,
    ) -> Vec<Bytes32> {
        let mut reg = self.inner.write().await;
        let removed: Vec<Bytes32> = {
            let Some(sub) = reg.subs.get_mut(peer) else {
                return Vec::new();
            };
            match puzzle_hashes {
                None => sub.puzzle_hashes.drain().collect(),
                Some(hashes) => hashes
                    .iter()
                    .filter(|ph| sub.puzzle_hashes.remove(*ph))
                    .copied()
                    .collect(),
            }
        };
        // Scrub the reverse index (empty buckets removed so the map stays bounded — the same
        // hygiene as drop_peer).
        for ph in &removed {
            if let Some(set) = reg.by_ph.get_mut(ph) {
                set.remove(peer);
                if set.is_empty() {
                    reg.by_ph.remove(ph);
                }
            }
        }
        removed
    }

    /// Drop a peer's coin-id subscriptions; see [`WalletNotifier::remove_ph_subscriptions`].
    pub async fn remove_coin_subscriptions(
        &self,
        peer: &Bytes32,
        coin_ids: Option<&[Bytes32]>,
    ) -> Vec<Bytes32> {
        let mut reg = self.inner.write().await;
        let removed: Vec<Bytes32> = {
            let Some(sub) = reg.subs.get_mut(peer) else {
                return Vec::new();
            };
            match coin_ids {
                None => sub.coin_ids.drain().collect(),
                Some(ids) => ids
                    .iter()
                    .filter(|id| sub.coin_ids.remove(*id))
                    .copied()
                    .collect(),
            }
        };
        for id in &removed {
            if let Some(set) = reg.by_coin.get_mut(id) {
                set.remove(peer);
                if set.is_empty() {
                    reg.by_coin.remove(id);
                }
            }
        }
        removed
    }

    // Create the bounded delivery channel for a peer on its first registration, returning the receiver the
    // server forwards to the socket. A no-op (None) if the peer already has a channel.
    async fn ensure_channel(
        &self,
        peer: Bytes32,
    ) -> Result<Option<Receiver<CoinStateUpdate>>, WalletError> {
        let mut reg = self.inner.write().await;
        if reg.subs.contains_key(&peer) {
            return Ok(None);
        }
        if reg.subs.len() >= self.max_subscribers {
            return Err(WalletError::TooManySubscribers);
        }
        let (tx, rx) = channel(self.channel_capacity);
        reg.subs.insert(
            peer,
            Subscriber {
                puzzle_hashes: HashSet::new(),
                coin_ids: HashSet::new(),
                tx,
            },
        );
        Ok(Some(rx))
    }

    /// Register a peer's interest in `puzzle_hashes` (`RegisterForPhUpdates`). Returns the
    /// delivery receiver on the peer's first registration (the server forwards it to the socket,
    /// `None` thereafter) AND the puzzle hashes actually subscribed by THIS call — the
    /// newly-added set with duplicates (in-request and already-subscribed) and the cap overflow
    /// filtered out; only that set feeds the initial-state query.
    ///
    /// # Errors
    /// Returns [`WalletError::TooManySubscribers`] / [`WalletError::TooManyItems`] if a bound is exceeded.
    pub async fn register_for_ph_updates(
        &self,
        peer: Bytes32,
        host: Option<IpAddr>,
        puzzle_hashes: &[Bytes32],
    ) -> Result<(Option<Receiver<CoinStateUpdate>>, Vec<Bytes32>), WalletError> {
        let rx = self.ensure_channel(peer).await?;
        let mut reg = self.inner.write().await;
        // The per-peer cap resolves from trust: a trusted peer (node id, localhost, or trusted CIDR)
        // gets `trusted_max_subscribe_items`.
        let cap = self.trust.max_subscriptions(&peer, host);
        // Truncate at the per-peer cap and dedup: add until the limit is reached, drop the
        // overflow, never error.
        let mut added = Vec::new();
        {
            let sub = reg.subs.get_mut(&peer).expect("just ensured");
            for ph in puzzle_hashes {
                if sub.items() >= cap {
                    break;
                }
                if sub.puzzle_hashes.insert(*ph) {
                    added.push(*ph);
                }
            }
        }
        for ph in &added {
            reg.by_ph.entry(*ph).or_default().insert(peer);
        }
        Ok((rx, added))
    }

    /// Register a peer's interest in `coin_ids` (`RegisterForCoinUpdates`). Returns the delivery
    /// receiver on the peer's first registration (`None` thereafter) and the coin ids newly
    /// subscribed by this call.
    ///
    /// # Errors
    /// Returns [`WalletError::TooManySubscribers`] / [`WalletError::TooManyItems`] if a bound is exceeded.
    pub async fn register_for_coin_updates(
        &self,
        peer: Bytes32,
        host: Option<IpAddr>,
        coin_ids: &[Bytes32],
    ) -> Result<(Option<Receiver<CoinStateUpdate>>, Vec<Bytes32>), WalletError> {
        let rx = self.ensure_channel(peer).await?;
        let mut reg = self.inner.write().await;
        // The per-peer cap resolves from trust: a trusted peer (node id, localhost, or trusted CIDR)
        // gets `trusted_max_subscribe_items`.
        let cap = self.trust.max_subscriptions(&peer, host);
        // Truncate at the per-peer cap and dedup.
        let mut added = Vec::new();
        {
            let sub = reg.subs.get_mut(&peer).expect("just ensured");
            for id in coin_ids {
                if sub.items() >= cap {
                    break;
                }
                if sub.coin_ids.insert(*id) {
                    added.push(*id);
                }
            }
        }
        for id in &added {
            reg.by_coin.entry(*id).or_default().insert(peer);
        }
        Ok((rx, added))
    }

    /// Drop a peer's subscriptions on disconnect (bounded-registry hygiene).
    pub async fn unsubscribe(&self, peer: &Bytes32) {
        let mut reg = self.inner.write().await;
        drop_peer(&mut reg, peer);
    }

    /// Reconcile the registry against the live inbound peer set — drop every subscriber whose
    /// connection is gone. The server runs this periodically against its inbound `PeerMap`; it is the
    /// disconnect hook (the `WebsocketServer` has no per-peer teardown callback). Dropping a subscriber
    /// drops its channel `Sender`, so the per-peer delivery task's `recv()` returns `None` and the
    /// forwarder task exits — no leaked task, no unbounded registry growth on a public listener.
    pub async fn retain_live(&self, live: &HashSet<Bytes32>) {
        let mut reg = self.inner.write().await;
        let gone: Vec<Bytes32> = reg
            .subs
            .keys()
            .filter(|p| !live.contains(*p))
            .copied()
            .collect();
        for peer in &gone {
            drop_peer(&mut reg, peer);
        }
    }

    /// Push already-resolved coin states to every matching subscriber — the reorg ROLLBACK push.
    /// The abandoned span's post-rollback records reach subscribers so a coin created above the
    /// fork reads "not on chain" (`created_height` None) and a coin spent above it reads unspent
    /// again (`spent_height` None) — a 0 index maps to None. Matching is by coin id + puzzle
    /// hash; the hint join on this push uses only the NEW peak's hint map, which cannot name a
    /// rolled-back coin, so ph + id is the same coverage. Delivery is the same bounded
    /// non-blocking channel as [`WalletNotifier::on_new_peak`].
    pub async fn notify_coin_states(
        &self,
        peak_hash: Bytes32,
        height: u32,
        fork_height: u32,
        records: &[CoinRecord],
    ) {
        let reg = self.inner.read().await;
        let mut per_peer: HashMap<Bytes32, Vec<CoinState>> = HashMap::new();
        for cr in records {
            let state = CoinState {
                coin: cr.coin,
                created_height: (cr.confirmed_block_index != 0).then_some(cr.confirmed_block_index),
                spent_height: (cr.spent_block_index != 0).then_some(cr.spent_block_index),
            };
            for peer in reg.match_peers(cr.coin.puzzle_hash, cr.coin.name(), None) {
                per_peer.entry(peer).or_default().push(state.clone());
            }
        }
        for (peer, items) in per_peer {
            if let Some(sub) = reg.subs.get(&peer) {
                let update = CoinStateUpdate {
                    height,
                    fork_height,
                    peak_hash,
                    items,
                };
                // Bounded, non-blocking: a slow or gone subscriber drops the update.
                let _ = sub.tx.try_send(update);
            }
        }
    }

    /// Emit a `CoinStateUpdate` to every subscriber whose interest matches a coin the new peak
    /// created or spent — see [`WalletUpdate`] for the delta shape. A coin whose HINT equals a
    /// subscribed puzzle hash matches too, with the map built from THIS peak's hints — the hint
    /// join covers coins hinted in this block, not spends of older hinted coins. Delivery is
    /// non-blocking: a full/closed channel drops the update (a slow wallet must not stall the
    /// peak path).
    ///
    /// # Errors
    /// Returns [`WalletError::Store`] if resolving a spent coin fails.
    pub async fn on_new_peak<S: CoinStore + Sync>(
        &self,
        store: &S,
        update: WalletUpdate<'_>,
    ) -> Result<(), WalletError> {
        // Resolve spent records first (await, no lock held) so no async lock spans the store call.
        let spent_records = if update.spent_ids.is_empty() {
            Vec::new()
        } else {
            store.get_coin_records(update.spent_ids).await?
        };

        // coin_id -> hint. BlockDelta::hints pairs are (hint, created_coin_id); the engine
        // already filters to exactly-32-byte hints.
        let hint_by_coin: HashMap<Bytes32, Bytes32> = update
            .hints
            .iter()
            .map(|(hint, coin_id)| (*coin_id, *hint))
            .collect();

        let reg = self.inner.read().await;
        let mut per_peer: HashMap<Bytes32, Vec<CoinState>> = HashMap::new();
        for cr in update.created {
            let state = created_state(cr);
            let coin_id = cr.coin.name();
            for peer in reg.match_peers(cr.coin.puzzle_hash, coin_id, hint_by_coin.get(&coin_id)) {
                per_peer.entry(peer).or_default().push(state.clone());
            }
        }
        for cr in &spent_records {
            let state = spent_state(cr);
            let coin_id = cr.coin.name();
            for peer in reg.match_peers(cr.coin.puzzle_hash, coin_id, hint_by_coin.get(&coin_id)) {
                per_peer.entry(peer).or_default().push(state.clone());
            }
        }
        for (peer, items) in per_peer {
            if let Some(sub) = reg.subs.get(&peer) {
                let update = CoinStateUpdate {
                    height: update.height,
                    fork_height: update.fork_height,
                    peak_hash: update.peak_hash,
                    items,
                };
                // Bounded, non-blocking: a slow or gone subscriber drops the update.
                let _ = sub.tx.try_send(update);
            }
        }
        Ok(())
    }
}

/// One confirmed peak's wallet-visible delta, borrowed from the engine's `BlockDelta`.
/// `created` are the block's addition records; `spent_ids` the removed coin ids (resolved to their
/// now-spent records from the store inside [`WalletNotifier::on_new_peak`]); `hints` the block's
/// create-coin `(hint, coin_id)` pairs (`BlockDelta::hints`, already filtered to 32-byte hints).
pub struct WalletUpdate<'a> {
    pub peak_hash: Bytes32,
    pub height: u32,
    pub fork_height: u32,
    pub created: &'a [CoinRecord],
    pub spent_ids: &'a [Bytes32],
    pub hints: &'a [(Bytes32, Bytes32)],
}

// A newly created coin: created at its confirmed index, unspent unless it was also spent this peak.
fn created_state(cr: &CoinRecord) -> CoinState {
    CoinState {
        coin: cr.coin,
        created_height: Some(cr.confirmed_block_index),
        spent_height: (cr.spent_block_index != 0).then_some(cr.spent_block_index),
    }
}

// A spent coin resolved from the store: both created and spent heights are known.
fn spent_state(cr: &CoinRecord) -> CoinState {
    CoinState {
        coin: cr.coin,
        created_height: Some(cr.confirmed_block_index),
        spent_height: Some(cr.spent_block_index),
    }
}

/// The acquire failed because active + waiting slots were all taken. The caller REJECTS the
/// request (`RejectAdditionsRequest` / `RejectRemovalsRequest`) — it never queues beyond the
/// waiting bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LimitedSemaphoreFull;

impl fmt::Display for LimitedSemaphoreFull {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "no waiting slot available")
    }
}

impl Error for LimitedSemaphoreFull {}

/// A bounded semaphore: at most `active_limit` holders run concurrently, at most `waiting_limit`
/// more may queue, and an acquire beyond active + waiting fails IMMEDIATELY with
/// [`LimitedSemaphoreFull`] instead of queueing without bound. The full node guards its heavy
/// wallet-serve DB work with one of these (active_limit=2, waiting_limit=20 on
/// `request_additions` / `request_removals`) so a public wallet peer cannot pile up unbounded
/// concurrent block-delta scans behind the rate limiter's per-message budget.
pub struct LimitedSemaphore {
    // The active-holder bound.
    active: Semaphore,
    // Remaining active + waiting slots. Checked-and-decremented on acquire, restored when the
    // permit drops; an acquire seeing no slot restores and fails without waiting.
    available: AtomicI64,
}

/// An acquired [`LimitedSemaphore`] slot. Dropping it releases the active permit and frees the
/// combined active/waiting slot.
pub struct LimitedPermit<'a> {
    _permit: SemaphorePermit<'a>,
    available: &'a AtomicI64,
}

impl Drop for LimitedPermit<'_> {
    fn drop(&mut self) {
        self.available.fetch_add(1, Ordering::AcqRel);
    }
}

impl LimitedSemaphore {
    #[must_use]
    pub fn new(active_limit: usize, waiting_limit: usize) -> Self {
        Self {
            active: Semaphore::new(active_limit),
            available: AtomicI64::new(
                i64::try_from(active_limit + waiting_limit).unwrap_or(i64::MAX),
            ),
        }
    }

    /// Take a slot: waits (bounded by `waiting_limit`) for an active permit, or fails immediately
    /// when active + waiting are exhausted.
    ///
    /// # Errors
    /// Returns [`LimitedSemaphoreFull`] when no active-or-waiting slot is free.
    pub async fn acquire(&self) -> Result<LimitedPermit<'_>, LimitedSemaphoreFull> {
        // check-then-decrement, restored on failure: transiently negative under a concurrent burst,
        // but every failed acquire restores its decrement, so the bound holds.
        if self.available.fetch_sub(1, Ordering::AcqRel) < 1 {
            self.available.fetch_add(1, Ordering::AcqRel);
            return Err(LimitedSemaphoreFull);
        }
        let permit = self
            .active
            .acquire()
            .await
            .expect("the active semaphore is never closed");
        Ok(LimitedPermit {
            _permit: permit,
            available: &self.available,
        })
    }
}

impl dg_xch_core::errors::ErrorCode for WalletError {
    fn band(&self) -> dg_xch_core::errors::ErrorBand {
        match self {
            WalletError::Store(inner) => inner.band(),
            _ => dg_xch_core::errors::ErrorBand::Wallet,
        }
    }
    fn variant(&self) -> u16 {
        match self {
            WalletError::TooManySubscribers => 1,
            WalletError::TooManyItems => 2,
            WalletError::Store(inner) => inner.variant(),
        }
    }
}
