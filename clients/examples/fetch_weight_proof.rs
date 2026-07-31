// Fetches a live weight proof and writes it as a fixture.

use dg_xch_clients::websocket::{WsClient, WsClientConfig, oneshot};
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
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
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
    async fn handle(
        &self,
        _m: Arc<ChiaMessage>,
        _p: Arc<Bytes32>,
        _s: PeerMap,
    ) -> Result<(), Error> {
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
    let msg = ChiaMessage::new(
        ProtocolMessageTypes::RequestProofOfWeight,
        version,
        &req,
        None,
    )?;
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
    // mainnet is the only network this fixture targets; the id is only needed for the handshake.
    let network = "mainnet";
    // Standard dg_xch env vars (see cli/src/lib.rs); default to the druid.garden full node.
    let host = std::env::var("FULLNODE_HOST").unwrap_or_else(|_| "druid.garden".to_string());
    let port: u16 = std::env::var("FULLNODE_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8444);
    let timeout_ms: u64 = 180_000;
    let out = format!("weight_proof_{network}.hex");
    let version = ChiaProtocolVersion::default();

    println!("network={network} version={version} node={host}:{port} timeout={timeout_ms}ms");
    println!("connecting to {host}:{port} ...");
    match try_peer(&host, port, network, version, timeout_ms).await {
        Ok((wp, peak)) => {
            let bytes = wp.to_bytes(version)?;
            let hex_str = bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
            std::fs::write(&out, &hex_str)?;
            // Also drop the raw binary (half the size — better for a committed `include_bytes!` fixture).
            let bin_out = out
                .strip_suffix(".hex")
                .map(|s| format!("{s}.bin"))
                .unwrap_or_else(|| format!("{out}.bin"));
            std::fs::write(&bin_out, &bytes)?;
            // Prove it round-trips before we call it a fixture.
            let mut cur = Cursor::new(bytes.as_slice());
            let back = WeightProof::from_bytes(&mut cur, version)
                .map_err(|e| Error::other(format!("fixture failed to round-trip: {e:?}")))?;
            let reser = back.to_bytes(version)?;
            assert_eq!(reser, bytes, "round-trip mismatch");
            println!("\nSUCCESS");
            println!("  network         : {network}");
            println!("  node            : {host}:{port}");
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
            Ok(())
        }
        Err(e) => {
            println!("  node failed: {e}");
            Err(Error::other("node did not produce a weight proof"))
        }
    }
}
