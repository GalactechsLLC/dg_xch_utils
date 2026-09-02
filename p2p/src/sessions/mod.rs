use crate::address_manager::AddressBook;
use crate::config::P2pSettings;
use crate::peer::{
    HandlerFactory, OutboundPeer, PeerRegistry, dial, dial_handlers, empty_handlers,
};
use dg_xch_clients::websocket::oneshot;
use dg_xch_core::blockchain::peer_info::TimestampedPeerInfo;
use dg_xch_core::protocols::full_node::{RequestPeers, RespondPeers};
use dg_xch_core::protocols::introducer::{RequestPeersIntroducer, RespondPeersIntroducer};
use dg_xch_core::protocols::{ChiaMessage, ProtocolMessageTypes};
use dg_xch_serialize::ChiaProtocolVersion;
use std::io::Error;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::task::JoinSet;
use tokio::time::sleep;

const WATCH: std::time::Duration = std::time::Duration::from_millis(50);

/// Fired once per successful outbound/manual dial, after the handshake completes and the peer is
/// registered. The hook is awaited inline before the hold loop starts, so implementations must
/// be short (a few sends, no long waits).
pub type OnConnectHook = Arc<
    dyn Fn(Arc<OutboundPeer>) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
        + Send
        + Sync,
>;

async fn keepalive_ok(peer: &OutboundPeer, settings: &P2pSettings, id: u16) -> bool {
    let version = ChiaProtocolVersion::default();
    let Ok(msg) = ChiaMessage::new(
        ProtocolMessageTypes::RequestPeers,
        version,
        &RequestPeers {},
        Some(id),
    ) else {
        return false;
    };
    oneshot::<RespondPeers>(
        peer.client.connection.clone(),
        msg,
        Some(ProtocolMessageTypes::RespondPeers),
        version,
        Some(id),
        Some(settings.pong_deadline.as_millis() as u64),
    )
    .await
    .is_ok()
}

// Hold a live channel until it closes or a stop is requested. is_closed is checked every
// WATCH tick (never trust the socket) and a keepalive probe fires every heartbeat; a
// missed pong within the deadline tears the half-open peer down.
async fn hold(peer: &OutboundPeer, run: &Arc<AtomicBool>, settings: &P2pSettings) {
    let mut hb = tokio::time::interval(settings.heartbeat);
    hb.tick().await; // consume the immediate first tick
    let mut watch = tokio::time::interval(WATCH);
    let mut probe_id: u16 = 0;
    while run.load(Ordering::Relaxed) && peer.run.load(Ordering::Relaxed) && !peer.is_closed() {
        tokio::select! {
            _ = hb.tick() => {
                probe_id = probe_id.wrapping_add(1);
                if !keepalive_ok(peer, settings, probe_id).await {
                    peer.stop();
                    break;
                }
            }
            _ = watch.tick() => {}
        }
    }
}

// A session that survives at least this long counts as a real connection: the slot's backoff
// resets and its address reclaims without a cooldown. Anything shorter is a churner — a peer
// at capacity accepting-then-closing, or one that cut us off — and sits out SHORT_SESSION_COOLDOWN
// before it can be dialed again.
const SHORT_SESSION_HOLD: Duration = Duration::from_secs(30);
const SHORT_SESSION_COOLDOWN: Duration = Duration::from_secs(300);

// session_outbound slot: take a random address, dial, hold, and on ANY stop reclaim the
// address and re-dial from the top — the reconnection loop lives OUTSIDE the dead channel.
async fn outbound_slot(
    book: Arc<Mutex<AddressBook>>,
    registry: Arc<PeerRegistry>,
    settings: P2pSettings,
    run: Arc<AtomicBool>,
    handlers: Option<HandlerFactory>,
    on_connect: Option<OnConnectHook>,
) {
    let mut attempt = 0u32;
    while run.load(Ordering::Relaxed) {
        let Some(addr) = book.lock().await.take() else {
            sleep(settings.retry_timeout).await;
            continue;
        };
        let endpoint = (addr.host.clone(), addr.port);
        if registry.reserve_outbound(&endpoint).await.is_err() {
            book.lock().await.reclaim(&addr, true);
            continue;
        }
        let peer_run = Arc::new(AtomicBool::new(true));
        match dial(
            &addr.host,
            addr.port,
            dial_handlers(handlers.as_ref()),
            peer_run.clone(),
            &settings,
        )
        .await
        {
            Ok(client) => {
                let peer = Arc::new(OutboundPeer {
                    endpoint: endpoint.clone(),
                    client,
                    run: peer_run,
                });
                registry.register_outbound(peer.clone()).await;
                // On-connect greetings: after the handshake, before the hold loop.
                if let Some(hook) = &on_connect {
                    hook(peer.clone()).await;
                }
                let held = std::time::Instant::now();
                hold(&peer, &run, &settings).await;
                peer.stop();
                registry.release_outbound(&endpoint).await;
                if held.elapsed() >= SHORT_SESSION_HOLD {
                    book.lock().await.reclaim(&addr, false);
                } else {
                    // The ADDRESS was the problem (a peer at capacity, or one that cut us
                    // off), not the slot: cool the address and move straight on to the next
                    // candidate. Growing the slot backoff here would slow the bootstrap
                    // burn-through that finds the healthy peers in the first place.
                    book.lock()
                        .await
                        .reclaim_after(&addr, SHORT_SESSION_COOLDOWN);
                }
                attempt = 0;
            }
            Err(_) => {
                registry.release_outbound(&endpoint).await;
                book.lock().await.reclaim(&addr, false);
                attempt = attempt.saturating_add(1);
            }
        }
        // Full-jitter defer before every re-dial (clean drop OR failure) so a mass
        // server-restart does not thundering-herd.
        if run.load(Ordering::Relaxed) {
            sleep(settings.jittered_backoff(attempt)).await;
        }
    }
}

// session_manual: a configured peer that reconnects forever on every drop, never aged out.
async fn manual_slot(
    host: String,
    port: u16,
    registry: Arc<PeerRegistry>,
    settings: P2pSettings,
    run: Arc<AtomicBool>,
    handlers: Option<HandlerFactory>,
    on_connect: Option<OnConnectHook>,
) {
    let endpoint = (host.clone(), port);
    let mut attempt = 0u32;
    while run.load(Ordering::Relaxed) {
        let peer_run = Arc::new(AtomicBool::new(true));
        if let Ok(client) = dial(
            &host,
            port,
            dial_handlers(handlers.as_ref()),
            peer_run.clone(),
            &settings,
        )
        .await
        {
            attempt = 0;
            let peer = Arc::new(OutboundPeer {
                endpoint: endpoint.clone(),
                client,
                run: peer_run,
            });
            registry.register_outbound(peer.clone()).await;
            // On-connect greetings, exactly as the outbound slot (a manual peer is a full node).
            if let Some(hook) = &on_connect {
                hook(peer.clone()).await;
            }
            hold(&peer, &run, &settings).await;
            peer.stop();
            registry.release_outbound(&endpoint).await;
        } else {
            attempt += 1;
        }
        if run.load(Ordering::Relaxed) {
            sleep(settings.jittered_backoff(attempt)).await;
        }
    }
}

// session_seed: one-shot bootstrap. Dial the introducer/seed, pull a peer list into the
// address book, then complete and exit — it holds no long-lived channel.
///
/// # Errors
/// Returns [`Error`] if the dial, handshake, or peer request fails.
pub async fn seed_once(
    host: &str,
    port: u16,
    book: Arc<Mutex<AddressBook>>,
    settings: &P2pSettings,
) -> Result<usize, Error> {
    let peer_run = Arc::new(AtomicBool::new(true));
    let mut client = dial(host, port, empty_handlers(), peer_run.clone(), settings).await?;
    // The introducer is NOT a full node — it speaks the introducer protocol
    // (RequestPeersIntroducer -> RespondPeersIntroducer), not full_node RequestPeers.
    // Sending RequestPeers makes the introducer protocol-close the connection.
    let version = ChiaProtocolVersion::default();
    let resp: RespondPeersIntroducer = oneshot(
        client.connection.clone(),
        ChiaMessage::new(
            ProtocolMessageTypes::RequestPeersIntroducer,
            version,
            &RequestPeersIntroducer {},
            Some(1),
        )?,
        Some(ProtocolMessageTypes::RespondPeersIntroducer),
        version,
        Some(1),
        Some(settings.handshake_timeout.as_millis() as u64),
    )
    .await?;
    let accepted = book.lock().await.insert_many(&resp.peer_list);
    peer_run.store(false, Ordering::Relaxed);
    let _ = client.shutdown().await;
    Ok(accepted)
}

// Introducer-retry backoff ceiling: doubles from `settings.retry_timeout` up to a 5-minute cap.
const INTRODUCER_BACKOFF_CAP: std::time::Duration = std::time::Duration::from_secs(300);

// Shutdown-prompt sleep: nap in WATCH slices so a capped introducer backoff (up to 300s) never
// stalls `Supervisor::stop` — the JoinSet drain waits for every session task to notice the run
// flag, and a monolithic `sleep(300s)` would hold the drain hostage.
async fn nap(total: std::time::Duration, run: &Arc<AtomicBool>) {
    let deadline = tokio::time::Instant::now() + total;
    while run.load(Ordering::Relaxed) {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return;
        }
        sleep(WATCH.min(deadline - now)).await;
    }
}

// session_introducer: retry the introducer seed while the node is peer-starved — below the
// outbound target with an address book that cannot supply a dial candidate. Doubling backoff,
// reset once the pool supplies addresses again.
async fn introducer_slot(
    host: String,
    port: u16,
    book: Arc<Mutex<AddressBook>>,
    registry: Arc<PeerRegistry>,
    settings: P2pSettings,
    run: Arc<AtomicBool>,
) {
    let mut backoff = settings.retry_timeout;
    while run.load(Ordering::Relaxed) {
        // Starved = below the outbound target AND no pooled candidate to dial. A book holding
        // addresses (even not-yet-proven ones) supplies the outbound slots first; a node at its
        // outbound target never contacts the introducer.
        let starving = registry.outbound_count().await < settings.target_outbound
            && book.lock().await.is_empty();
        if !starving {
            backoff = settings.retry_timeout;
            nap(settings.retry_timeout, &run).await;
            continue;
        }
        match seed_once(&host, port, book.clone(), &settings).await {
            Ok(n) => log::info!("seeded {n} addresses from introducer {host}:{port}"),
            Err(e) => log::warn!(
                "introducer seed failed ({host}:{port}), retrying in {}s: {e}",
                backoff.as_secs_f32()
            ),
        }
        // Backoff after EVERY attempt (success included): a reachable introducer with an empty
        // peer list must not be hammered. Doubles to the 5-minute cap, resets when starvation
        // clears (the `!starving` arm above).
        nap(backoff, &run).await;
        backoff = (backoff * 2).min(INTRODUCER_BACKOFF_CAP);
    }
}

// The connection supervisor: owns the run flag and all session tasks in one JoinSet so a
// stop drains every task. Also the outbound-slot maintainer (target_outbound slots).
pub struct Supervisor {
    pub run: Arc<AtomicBool>,
    pub registry: Arc<PeerRegistry>,
    pub book: Arc<Mutex<AddressBook>>,
    settings: P2pSettings,
    tasks: JoinSet<()>,
    // Per-connection handler map factory applied to every outbound/manual dial (the daemon's
    // full_node_handlers_client). None → bare connections (empty handler map).
    handlers: Option<HandlerFactory>,
    // Fired after each successful outbound/manual dial registers (the daemon's on-connect
    // greetings). None → no greeting.
    on_connect: Option<OnConnectHook>,
}

impl Supervisor {
    #[must_use]
    pub fn new(settings: P2pSettings) -> Self {
        Self {
            run: Arc::new(AtomicBool::new(true)),
            registry: Arc::new(PeerRegistry::new(settings)),
            book: Arc::new(Mutex::new(AddressBook::new(&settings))),
            settings,
            tasks: JoinSet::new(),
            handlers: None,
            on_connect: None,
        }
    }

    // Install the handler-map factory used for every outbound/manual dial. Call before start_outbound.
    pub fn set_handlers(&mut self, handlers: HandlerFactory) {
        self.handlers = Some(handlers);
    }

    // Install the on-connect hook fired after each successful outbound/manual dial registers.
    // Call before start_outbound/start_manual.
    pub fn set_on_connect(&mut self, hook: OnConnectHook) {
        self.on_connect = Some(hook);
    }

    pub async fn seed_addresses(&self, peers: &[TimestampedPeerInfo]) -> usize {
        self.book.lock().await.insert_many(peers)
    }

    // Hold target_outbound self-healing slots (the sync-concurrency width).
    pub fn start_outbound(&mut self) {
        for _ in 0..self.settings.target_outbound {
            self.tasks.spawn(outbound_slot(
                self.book.clone(),
                self.registry.clone(),
                self.settings,
                self.run.clone(),
                self.handlers.clone(),
                self.on_connect.clone(),
            ));
        }
    }

    // session_introducer: re-queries the introducer whenever the node still needs peers.
    pub fn start_introducer(&mut self, host: &str, port: u16) {
        self.tasks.spawn(introducer_slot(
            host.to_string(),
            port,
            self.book.clone(),
            self.registry.clone(),
            self.settings,
            self.run.clone(),
        ));
    }

    pub fn start_manual(&mut self, host: &str, port: u16) {
        self.tasks.spawn(manual_slot(
            host.to_string(),
            port,
            self.registry.clone(),
            self.settings,
            self.run.clone(),
            self.handlers.clone(),
            self.on_connect.clone(),
        ));
    }

    // Flip the run flag and drain every session task — no leaked tasks on shutdown.
    pub async fn stop(&mut self) {
        self.run.store(false, Ordering::Relaxed);
        for peer in self.registry.outbound_peers().await {
            peer.stop();
        }
        while self.tasks.join_next().await.is_some() {}
    }
}
