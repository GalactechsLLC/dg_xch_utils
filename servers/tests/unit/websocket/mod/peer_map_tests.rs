use super::deregister_peer;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

// A distinct peer id per index (the map is keyed by the cert-hash identity in production).
fn id(i: u32) -> Bytes32 {
    let mut b = [0u8; 32];
    b[..4].copy_from_slice(&i.to_le_bytes());
    Bytes32::from(b)
}

// The generic helper carries the exact map lifecycle `handle_connection` uses; testing it
// over `Arc<u32>` exercises the identity guard without standing up a TLS socket to build a
// real `SocketPeer`.
type Map = Arc<RwLock<HashMap<Bytes32, Arc<u32>>>>;

// Insert-only grows the inbound map without bound: one orphaned `SocketPeer` (and its dead
// `WebsocketConnection`) per closed connection.
#[tokio::test]
async fn insert_only_grows_unbounded() {
    let peers: Map = Arc::new(RwLock::new(HashMap::new()));
    for i in 0..1_000u32 {
        peers.write().await.insert(id(i), Arc::new(i));
    }
    assert_eq!(
        peers.read().await.len(),
        1_000,
        "without teardown every closed connection leaks its map entry"
    );
}

// With the teardown, N open/close cycles leave the map flat at baseline — bounded memory
// under churn.
#[tokio::test]
async fn churn_with_teardown_stays_flat() {
    let peers: Map = Arc::new(RwLock::new(HashMap::new()));
    for i in 0..10_000u32 {
        // ids intentionally collide (mod) so the replace path is exercised too.
        let key = id(i % 251);
        let ours = Arc::new(i);
        peers.write().await.insert(key, ours.clone());
        deregister_peer(&peers, &key, &ours).await;
    }
    assert_eq!(
        peers.read().await.len(),
        0,
        "inbound map must return to baseline after connection churn"
    );
}

// The identity guard: a stale connection's teardown must never evict the entry a peer
// installed when it reconnected.
#[tokio::test]
async fn reconnect_keeps_the_fresh_entry() {
    let peers: Map = Arc::new(RwLock::new(HashMap::new()));
    let key = id(7);
    let first = Arc::new(1u32);
    peers.write().await.insert(key, first.clone());
    // Peer reconnects: a fresh handle replaces the map value.
    let second = Arc::new(2u32);
    peers.write().await.insert(key, second.clone());
    // The FIRST (now-dead) connection's teardown must be a no-op — it is not current.
    assert!(!deregister_peer(&peers, &key, &first).await);
    assert_eq!(peers.read().await.len(), 1);
    assert!(Arc::ptr_eq(peers.read().await.get(&key).unwrap(), &second));
    // The live connection's own teardown clears it.
    assert!(deregister_peer(&peers, &key, &second).await);
    assert_eq!(peers.read().await.len(), 0);
}
