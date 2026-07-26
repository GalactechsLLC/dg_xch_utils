//! Fetch a REAL weight proof from a live Chia full node over the P2P protocol and export it as a
//! ChiaSerialize fixture (hex) for the `weight_proof_validate` accept-vector test (feature 016).
//!
//! It resolves peers from the DNS seeder, connects with the Chia mTLS identity model (a per-run cert
//! signed by the embedded mainnet CA, `NoCertificateVerification` on the server side — the network's
//! model, not a bug), captures the peer's `NewPeak`, then `RequestProofOfWeight{tip, height}` and awaits
//! `RespondProofOfWeight`. The `WeightProof` is written as hex (`wp.to_bytes(version)`), which
//! `WeightProof::from_bytes` reads straight back.
//!
//! Run (from the dg_xch_utils workspace root):
//!   FRIA_WP_NETWORK=mainnet FRIA_WP_OUT=weight_proof_mainnet.hex \
//!     cargo run -p dg_xch_clients --example fetch_weight_proof
//! Optional: FRIA_WP_SEED=dns-introducer.chia.net  FRIA_WP_PORT=8444  FRIA_WP_PEER=<ip>  FRIA_WP_TIMEOUT_MS=180000

use dg_xch_clients::websocket::{oneshot, WsClient, WsClientConfig};
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::blockchain::weight_proof::WeightProof;
use dg_xch_core::constants::{CHIA_CA_CRT, CHIA_CA_KEY};
use dg_xch_core::protocols::full_node::{NewPeak, RequestProofOfWeight, RespondProofOfWeight};
use dg_xch_core::protocols::{
    ChiaMessage, ChiaMessageFilter, ChiaMessageHandler, MessageHandler, NodeType, PeerMap,
    ProtocolMessageTypes,
};
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use rustls::crypto::ring::default_provider;
use std::collections::HashMap;
use std::io::{Cursor, Error};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use uuid::Uuid;

/// Captures the first `NewPeak` the peer pushes after the handshake into a shared slot.
struct PeakCatcher {
    slot: Arc<RwLock<Option<NewPeak>>>,
    version: ChiaProtocolVersion,
}
#[async_trait::async_trait]
impl MessageHandler for PeakCatcher {
    async fn handle(
        &self,
        msg: Arc<ChiaMessage>,
        _peer_id: Arc<Bytes32>,
        _peers: PeerMap,
    ) -> Result<(), Error> {
        let mut cur = Cursor::new(msg.data.bytes.as_slice());
        if let Ok(peak) = NewPeak::from_bytes(&mut cur, self.version) {
            let mut w = self.slot.write().await;
            if w.is_none() {
                *w = Some(peak);
            }
        }
        Ok(())
    }
}

/// Ignore everything else the node streams at us (mempool churn, etc.) so it does not log-spam.
struct Ignore;
#[async_trait::async_trait]
impl MessageHandler for Ignore {
    async fn handle(&self, _m: Arc<ChiaMessage>, _p: Arc<Bytes32>, _s: PeerMap) -> Result<(), Error> {
        Ok(())
    }
}

async fn try_peer(
    host: &str,
    port: u16,
    network_id: &str,
    version: ChiaProtocolVersion,
    timeout_ms: u64,
) -> Result<(WeightProof, NewPeak), Error> {
    let peak_slot: Arc<RwLock<Option<NewPeak>>> = Arc::new(RwLock::new(None));
    let handles = Arc::new(RwLock::new(HashMap::from([
        (
            Uuid::new_v4(),
            Arc::new(ChiaMessageHandler {
                filter: Arc::new(ChiaMessageFilter {
                    msg_type: Some(ProtocolMessageTypes::NewPeak),
                    id: None,
                    custom_fn: None,
                }),
                handle: Arc::new(PeakCatcher {
                    slot: peak_slot.clone(),
                    version,
                }),
            }),
        ),
        (
            Uuid::new_v4(),
            Arc::new(ChiaMessageHandler {
                filter: Arc::new(ChiaMessageFilter {
                    msg_type: None,
                    id: None,
                    custom_fn: Some(Box::new(|_m| true)),
                }),
                handle: Arc::new(Ignore),
            }),
        ),
    ])));

    let config = Arc::new(WsClientConfig {
        host: host.to_string(),
        port,
        network_id: network_id.to_string(),
        ssl_info: None, // None → with_ca generates a per-run cert from the embedded CA (Chia identity model)
        software_version: None,
        protocol_version: version,
        additional_headers: None,
    });
    let run = Arc::new(AtomicBool::new(true));
    let client = WsClient::with_ca(
        config,
        NodeType::FullNode,
        handles,
        run,
        CHIA_CA_CRT.as_bytes(),
        CHIA_CA_KEY.as_bytes(),
        30_000,
    )
    .await?;

    // Wait for the peer to announce its peak (it pushes NewPeak right after the handshake).
    let mut waited = 0u64;
    let peak = loop {
        if let Some(p) = peak_slot.read().await.clone() {
            break p;
        }
        if waited >= 15_000 {
            let _ = client.connection.write().await.shutdown().await;
            return Err(Error::other("no NewPeak within 15s"));
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
        waited += 250;
    };
    println!(
        "  peak: height={} weight={} tip={}",
        peak.height, peak.weight, peak.header_hash
    );

    let req = RequestProofOfWeight {
        total_number_of_blocks: peak.height,
        tip: peak.header_hash,
    };
    let msg = ChiaMessage::new(ProtocolMessageTypes::RequestProofOfWeight, version, &req, None)?;
    let resp: RespondProofOfWeight = oneshot(
        client.connection.clone(),
        msg,
        Some(ProtocolMessageTypes::RespondProofOfWeight),
        version,
        None,
        Some(timeout_ms),
    )
    .await?;
    let _ = client.connection.write().await.shutdown().await;
    Ok((resp.wp, peak))
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let _ = default_provider().install_default();
    let network = std::env::var("FRIA_WP_NETWORK").unwrap_or_else(|_| "mainnet".to_string());
    let seed = std::env::var("FRIA_WP_SEED")
        .unwrap_or_else(|_| "dns-introducer.chia.net".to_string());
    let port: u16 = std::env::var("FRIA_WP_PORT").ok().and_then(|v| v.parse().ok()).unwrap_or(8444);
    let out = std::env::var("FRIA_WP_OUT")
        .unwrap_or_else(|_| format!("weight_proof_{network}.hex"));
    let timeout_ms: u64 = std::env::var("FRIA_WP_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(180_000);
    let version = ChiaProtocolVersion::default();

    // Peer list: an explicit FRIA_WP_PEER, else resolve the DNS seeder (its A records ARE full nodes).
    let mut peers: Vec<String> = Vec::new();
    if let Ok(p) = std::env::var("FRIA_WP_PEER") {
        peers.push(p);
    } else {
        let addrs = tokio::net::lookup_host((seed.as_str(), port)).await?;
        for a in addrs {
            peers.push(a.ip().to_string());
        }
    }
    println!(
        "network={network} version={version} seed={seed} candidates={} timeout={}ms",
        peers.len(),
        timeout_ms
    );

    for peer in peers.iter().take(12) {
        println!("connecting to {peer}:{port} ...");
        match try_peer(peer, port, &network, version, timeout_ms).await {
            Ok((wp, peak)) => {
                let bytes = wp.to_bytes(version)?;
                let hex_str = bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
                std::fs::write(&out, &hex_str)?;
                // Also drop the raw binary (half the size — better for a committed `include_bytes!` fixture).
                let bin_out = out.strip_suffix(".hex").map(|s| format!("{s}.bin")).unwrap_or_else(|| format!("{out}.bin"));
                std::fs::write(&bin_out, &bytes)?;
                // Prove it round-trips before we call it a fixture.
                let mut cur = Cursor::new(bytes.as_slice());
                let back = WeightProof::from_bytes(&mut cur, version)
                    .map_err(|e| Error::other(format!("fixture failed to round-trip: {e:?}")))?;
                let reser = back.to_bytes(version)?;
                assert_eq!(reser, bytes, "round-trip mismatch");
                println!("\nSUCCESS");
                println!("  network         : {network}");
                println!("  peer            : {peer}:{port}");
                println!("  tip             : {}", peak.header_hash);
                println!("  height          : {}", peak.height);
                println!("  weight          : {}", peak.weight);
                println!("  sub_epochs      : {}", wp.sub_epochs.len());
                println!("  sub_epoch_segs  : {}", wp.sub_epoch_segments.len());
                println!("  recent_chain    : {}", wp.recent_chain_data.len());
                println!("  wp bytes        : {}", bytes.len());
                println!("  fixture (bin)   : {bin_out}  ({} bytes)", bytes.len());
                println!("  fixture (hex)   : {out}  ({} chars)", hex_str.len());
                println!("  round-trips     : yes (to_bytes → from_bytes → to_bytes identical)");
                return Ok(());
            }
            Err(e) => {
                println!("  peer failed: {e}");
                continue;
            }
        }
    }
    Err(Error::other("no peer produced a weight proof"))
}
