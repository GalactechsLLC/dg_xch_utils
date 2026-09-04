use async_trait::async_trait;
use clap::Parser;
use dg_xch_clients::websocket::{WsClient, WsClientConfig};
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::constants::{CHIA_CA_CRT, CHIA_CA_KEY};
use dg_xch_core::protocols::farmer::NewSignagePoint;
use dg_xch_core::protocols::timelord::NewPeakTimelord;
use dg_xch_core::protocols::{
    ChiaMessage, ChiaMessageFilter, ChiaMessageHandler, MessageHandler, NodeType, PeerMap,
    ProtocolMessageTypes,
};
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use std::collections::HashMap;
use std::io::{Cursor, Error};
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{RwLock, mpsc};
use uuid::Uuid;

/// Producer counter families exposed by the full-node metrics service.
const PRODUCER_FAMILIES: [&str; 9] = [
    "fullnode_producer_declares_received_total",
    "fullnode_producer_candidates_built_total",
    "fullnode_producer_request_signed_values_total",
    "fullnode_producer_signed_values_received_total",
    "fullnode_producer_ub_assembled_total",
    "fullnode_producer_full_blocks_total",
    "fullnode_producer_declares_validated_total",
    "fullnode_producer_candidates_dropped_total",
    "fullnode_producer_ub_broadcast_total",
];

/// The signage-point metric family the node exposes today. The node metrics surface has NO dedicated
/// SP-interval histogram; the interval is proven live from captured frames instead (farmer cadence).
const SIGNAGE_FAMILIES: [&str; 2] = [
    "fullnode_signage_points_total",
    "fullnode_current_signage_point",
];

/// The candidate-drop reason that names the "unfinished block ready but no timelord attached" wall.
const NO_TIMELORD_SERIES: &str =
    "fullnode_producer_candidates_dropped_total{reason=\"no_timelord_peer\"}";

#[derive(Parser, Debug)]
#[command(
    about = "Synthetic-peer prerequisite validation against a running full node",
    long_about = "Connects to a running full node as a synthetic Timelord and Farmer, captures inbound \
                  frames, and asserts the node gossips NewPeakTimelord (timelord) and NewSignagePoint \
                  with sp_source_data (farmer). Prints captured fields as evidence."
)]
struct Args {
    /// Node p2p WebSocket host (service: dg-xch-node).
    #[arg(long, env = "DGXCH_VALIDATE_NODE_WS", default_value = "dg-xch-node")]
    ws_host: String,
    #[arg(long, default_value_t = 8444)]
    ws_port: u16,
    /// Node metrics host (service: dg-xch-node-rpc).
    #[arg(long, default_value = "dg-xch-node-rpc")]
    metrics_host: String,
    #[arg(long, default_value_t = 9100)]
    metrics_port: u16,
    #[arg(long, default_value = "mainnet")]
    network: String,
    #[arg(long, default_value_t = 60)]
    timelord_window_secs: u64,
    /// Window to capture NewSignagePoint frames. Index-0 SPs recur ~once per sub-slot (~10 min on
    /// mainnet); raise this to prove the index-0 sub_slot_data path. The tool exits early once it has
    /// captured an index-0 SP, a normal-index SP, and enough SPs to measure cadence.
    #[arg(long, default_value_t = 120)]
    farmer_window_secs: u64,
    /// Connect + handshake timeout (seconds).
    #[arg(long, default_value_t = 30)]
    connect_timeout_secs: u64,
    /// Hard-fail the farmer section if no index-0 (sub-slot-start) SP is captured within the window.
    /// Default false so a short run is not blocked by the ~10-minute index-0 cadence.
    #[arg(long, default_value_t = false)]
    require_index_zero_sp: bool,
}

/// A catch-all inbound-frame capture. The connection filter is match-all, so every inbound frame is
/// forwarded WHOLE (type + id + payload) to a bounded channel. The connection spawns this per message,
/// so awaiting the send only backpressures that spawned task, never the read loop; a closed receiver
/// (window elapsed) just ends the forward silently.
struct CaptureHandler {
    tx: mpsc::Sender<Arc<ChiaMessage>>,
}

#[async_trait]
impl MessageHandler for CaptureHandler {
    async fn handle(
        &self,
        msg: Arc<ChiaMessage>,
        _peer_id: Arc<Bytes32>,
        _peers: PeerMap,
    ) -> Result<(), Error> {
        let _ = self.tx.send(msg).await;
        Ok(())
    }
}

async fn connect_synthetic_peer(
    args: &Args,
    node_type: NodeType,
) -> Result<(WsClient, mpsc::Receiver<Arc<ChiaMessage>>), Error> {
    // Bounded: 512 frames is ~30x the busiest window (SPs every ~9s, peaks every
    // ~15s) — it will not fill; if it somehow did we would drop evidence, never stall the reader.
    let (tx, rx) = mpsc::channel::<Arc<ChiaMessage>>(512);
    let handlers: HashMap<Uuid, Arc<ChiaMessageHandler>> = HashMap::from([(
        Uuid::new_v4(),
        Arc::new(ChiaMessageHandler::new(
            Arc::new(ChiaMessageFilter {
                msg_type: None,
                id: None,
                custom_fn: None,
            }),
            Arc::new(CaptureHandler { tx }),
        )),
    )]);
    let config = Arc::new(WsClientConfig {
        host: args.ws_host.clone(),
        port: args.ws_port,
        network_id: args.network.clone(),
        ssl_info: None,
        software_version: None,
        // Request the newest protocol so sp_source_data (>= 0.0.36) is negotiated.
        protocol_version: ChiaProtocolVersion::Chia0_0_37,
        additional_headers: None,
        rate_limited: false,
    });
    let run = Arc::new(AtomicBool::new(true));
    let client = WsClient::with_ca(
        config,
        node_type,
        Arc::new(RwLock::new(handlers)),
        run,
        CHIA_CA_CRT.as_bytes(),
        CHIA_CA_KEY.as_bytes(),
        args.connect_timeout_secs,
    )
    .await?;
    Ok((client, rx))
}

/// Print the peer handshake and return the protocol version the node negotiated (used to decode the
/// payloads the node encodes for us). Returns None if the handshake is missing.
fn report_handshake(client: &WsClient, role: &str) -> Option<ChiaProtocolVersion> {
    match &client.handshake {
        Some(h) => {
            println!(
                "  handshake({role}): node_type={} network_id={} protocol_version={} software_version={}",
                h.node_type, h.network_id, h.protocol_version, h.software_version
            );
            Some(
                ChiaProtocolVersion::from_str(&h.protocol_version)
                    .unwrap_or(ChiaProtocolVersion::Chia0_0_37),
            )
        }
        None => {
            println!("  handshake({role}): MISSING");
            None
        }
    }
}

fn pass(label: &str) {
    println!("  PASS  {label}");
}
fn fail(label: &str) {
    println!("  FAIL  {label}");
}
fn check(label: &str, cond: bool, ok: &mut bool) {
    if cond {
        pass(label);
    } else {
        fail(label);
        *ok = false;
    }
}

fn type_name(code: u8) -> String {
    match code {
        1 => "Handshake".into(),
        8 => "NewSignagePoint".into(),
        13 => "NewPeakTimelord".into(),
        14 => "NewUnfinishedBlockTimelord".into(),
        20 => "NewPeak".into(),
        other => format!("type_{other}"),
    }
}

fn named_counts(counts: &HashMap<u8, u32>) -> Vec<(String, u32)> {
    let mut v: Vec<(String, u32)> = counts.iter().map(|(k, n)| (type_name(*k), *n)).collect();
    v.sort();
    v
}

/// Validate a captured NewPeakTimelord and print its key fields as evidence.
fn validate_new_peak_timelord(np: &NewPeakTimelord, ok: &mut bool) {
    println!("  --- NewPeakTimelord evidence ---");
    println!(
        "    reward_chain_block: height={} weight={} sp_index={} total_iters={}",
        np.reward_chain_block.height,
        np.reward_chain_block.weight,
        np.reward_chain_block.signage_point_index,
        np.reward_chain_block.total_iters
    );
    println!(
        "    difficulty={} sub_slot_iters={} deficit={}",
        np.difficulty, np.sub_slot_iters, np.deficit
    );
    println!(
        "    previous_reward_challenges: {} entries",
        np.previous_reward_challenges.len()
    );
    println!(
        "    last_challenge_sb_or_eos_total_iters={}",
        np.last_challenge_sb_or_eos_total_iters
    );
    println!(
        "    passes_ses_height_but_not_yet_included={}",
        np.passes_ses_height_but_not_yet_included
    );
    match &np.sub_epoch_summary {
        Some(ses) => println!(
            "    sub_epoch_summary: Some(new_difficulty={:?} new_sub_slot_iters={:?})",
            ses.new_difficulty, ses.new_sub_slot_iters
        ),
        None => println!("    sub_epoch_summary: None (normal off a sub-epoch boundary)"),
    }
    println!("  --- assertions ---");
    check(
        "reward_chain_block present (weight > 0)",
        np.reward_chain_block.weight > 0,
        ok,
    );
    check("difficulty > 0", np.difficulty > 0, ok);
    check("sub_slot_iters > 0", np.sub_slot_iters > 0, ok);
    check(
        "previous_reward_challenges non-empty",
        !np.previous_reward_challenges.is_empty(),
        ok,
    );
    let ses_consistent = match &np.sub_epoch_summary {
        None => true,
        Some(ses) => ses.new_difficulty.is_some() == ses.new_sub_slot_iters.is_some(),
    };
    check("sub_epoch_summary field consistent", ses_consistent, ok);
}

/// SYNTHETIC TIMELORD: assert the node gossips a well-formed NewPeakTimelord within the window.
async fn run_timelord_check(args: &Args) -> bool {
    println!("\n== SYNTHETIC TIMELORD PEER (NodeType::Timelord) ==");
    let (mut client, mut rx) = match connect_synthetic_peer(args, NodeType::Timelord).await {
        Ok(v) => v,
        Err(e) => {
            fail(&format!("connect/handshake as Timelord: {e}"));
            return false;
        }
    };
    let version = report_handshake(&client, "timelord").unwrap_or(ChiaProtocolVersion::Chia0_0_37);
    let deadline = Instant::now() + Duration::from_secs(args.timelord_window_secs);
    let mut ok = true;
    let mut got = false;
    let mut counts: HashMap<u8, u32> = HashMap::new();
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Some(msg)) => {
                *counts.entry(msg.msg_type as u8).or_default() += 1;
                match msg.msg_type {
                    ProtocolMessageTypes::NewPeakTimelord => {
                        let mut cur = Cursor::new(msg.data.bytes.as_slice());
                        match NewPeakTimelord::from_bytes(&mut cur, version) {
                            Ok(np) => {
                                validate_new_peak_timelord(&np, &mut ok);
                                got = true;
                                break;
                            }
                            Err(e) => {
                                fail(&format!("NewPeakTimelord decode: {e}"));
                                ok = false;
                            }
                        }
                    }
                    ProtocolMessageTypes::NewUnfinishedBlockTimelord => {
                        println!(
                            "  (observed NewUnfinishedBlockTimelord — logged; not required here)"
                        );
                    }
                    _ => {}
                }
            }
            Ok(None) => {
                fail("connection closed before NewPeakTimelord");
                break;
            }
            Err(_) => break,
        }
    }
    println!("  inbound frames seen: {:?}", named_counts(&counts));
    if !got {
        fail(&format!(
            "NewPeakTimelord NOT captured within {}s — timelord-gossip gap (or node not yet at tip)",
            args.timelord_window_secs
        ));
        ok = false;
    }
    let _ = client.shutdown().await;
    ok
}

#[derive(Default)]
struct FarmerObserved {
    total_sps: u32,
    with_source_data: u32,
    index0_seen: bool,
    index0_subslot_ok: bool,
    normal_vdf_ok: bool,
    max_gap: Duration,
}

/// SYNTHETIC FARMER: assert the node gossips NewSignagePoint with sp_source_data, that an index-0 SP
/// carries sub_slot_data (proof of the farmer #1 path) and a normal-index SP carries vdf_data, and that
/// consecutive-SP cadence stays under the 60s reconnect watchdog.
async fn run_farmer_check(args: &Args) -> bool {
    println!("\n== SYNTHETIC FARMER PEER (NodeType::Farmer) ==");
    let (mut client, mut rx) = match connect_synthetic_peer(args, NodeType::Farmer).await {
        Ok(v) => v,
        Err(e) => {
            fail(&format!("connect/handshake as Farmer: {e}"));
            return false;
        }
    };
    let mut ok = true;
    let version = report_handshake(&client, "farmer");
    match &client.handshake {
        Some(h) => {
            check(
                "handshake protocol_version present",
                !h.protocol_version.is_empty(),
                &mut ok,
            );
            check(
                "handshake software_version present",
                !h.software_version.is_empty(),
                &mut ok,
            );
        }
        None => {
            fail("handshake missing");
            ok = false;
        }
    }
    let version = version.unwrap_or(ChiaProtocolVersion::Chia0_0_37);
    let deadline = Instant::now() + Duration::from_secs(args.farmer_window_secs);
    let mut obs = FarmerObserved::default();
    let mut last_arrival: Option<Instant> = None;
    let mut counts: HashMap<u8, u32> = HashMap::new();
    let mut examples_printed = 0u32;
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Some(msg)) => {
                *counts.entry(msg.msg_type as u8).or_default() += 1;
                if msg.msg_type != ProtocolMessageTypes::NewSignagePoint {
                    continue;
                }
                let now = Instant::now();
                if let Some(prev) = last_arrival {
                    let gap = now.saturating_duration_since(prev);
                    if gap > obs.max_gap {
                        obs.max_gap = gap;
                    }
                }
                last_arrival = Some(now);
                let mut cur = Cursor::new(msg.data.bytes.as_slice());
                let sp = match NewSignagePoint::from_bytes(&mut cur, version) {
                    Ok(sp) => sp,
                    Err(e) => {
                        fail(&format!("NewSignagePoint decode: {e}"));
                        ok = false;
                        continue;
                    }
                };
                obs.total_sps += 1;
                let src = sp.sp_source_data.as_ref();
                if src.is_some() {
                    obs.with_source_data += 1;
                }
                let sub_slot = src.and_then(|s| s.sub_slot_data.as_ref());
                let vdf = src.and_then(|s| s.vdf_data.as_ref());
                if examples_printed < 4 {
                    println!(
                        "  SP idx={:>2} peak_height={} src={} sub_slot_data={} vdf_data={}",
                        sp.signage_point_index,
                        sp.peak_height,
                        src.is_some(),
                        sub_slot.is_some(),
                        vdf.is_some()
                    );
                    examples_printed += 1;
                }
                if sp.signage_point_index == 0 {
                    obs.index0_seen = true;
                    if sub_slot.is_some() && vdf.is_none() {
                        obs.index0_subslot_ok = true;
                        println!(
                            "  >> index-0 SP: cc_sub_slot + rc_sub_slot present, vdf_data None (farmer #1 proof)"
                        );
                    }
                } else if vdf.is_some() {
                    obs.normal_vdf_ok = true;
                }
                if obs.index0_subslot_ok && obs.normal_vdf_ok && obs.total_sps >= 2 {
                    break;
                }
            }
            Ok(None) => {
                fail("connection closed during farmer capture");
                break;
            }
            Err(_) => break,
        }
    }
    println!("  inbound frames seen: {:?}", named_counts(&counts));
    println!(
        "  captured {} NewSignagePoint frames ({} with sp_source_data); max consecutive gap {:.1}s",
        obs.total_sps,
        obs.with_source_data,
        obs.max_gap.as_secs_f64()
    );
    println!("  --- assertions ---");
    check("NewSignagePoint arrives", obs.total_sps > 0, &mut ok);
    check(
        "every SP carries sp_source_data",
        obs.total_sps > 0 && obs.with_source_data == obs.total_sps,
        &mut ok,
    );
    check(
        "normal-index SP carries vdf_data (cc_vdf/rc_vdf)",
        obs.normal_vdf_ok,
        &mut ok,
    );
    check(
        "consecutive-SP cadence < 60s (reconnect watchdog)",
        obs.total_sps >= 2 && obs.max_gap < Duration::from_secs(60),
        &mut ok,
    );
    if obs.index0_seen {
        check(
            "index-0 SP carries sub_slot_data NOT vdf_data (farmer #1)",
            obs.index0_subslot_ok,
            &mut ok,
        );
    } else if args.require_index_zero_sp {
        fail("index-0 SP not captured within window (required)");
        ok = false;
    } else {
        println!(
            "  SKIP  index-0 SP not observed in {}s window (index-0 recurs ~10 min on mainnet); \
             re-run with a longer --farmer-window-secs and --require-index-zero-sp for the #1 proof",
            args.farmer_window_secs
        );
    }
    let _ = client.shutdown().await;
    ok
}

async fn scrape_metrics(host: &str, port: u16, timeout_secs: u64) -> Result<String, Error> {
    let body = tokio::time::timeout(Duration::from_secs(timeout_secs), async {
        let mut stream = TcpStream::connect((host, port)).await?;
        let req = format!(
            "GET /metrics HTTP/1.1\r\nHost: {host}\r\nAccept: text/plain\r\nConnection: close\r\n\r\n"
        );
        stream.write_all(req.as_bytes()).await?;
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await?;
        Ok::<String, Error>(String::from_utf8_lossy(&buf).into_owned())
    })
    .await
    .map_err(|_| Error::other("timeout scraping /metrics"))??;
    Ok(body)
}

/// Extract the numeric value of an exact metric series line (name including any label set).
fn metric_value(body: &str, series: &str) -> Option<f64> {
    for line in body.lines() {
        if line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix(series) {
            // Guard against a longer metric name sharing this prefix: the next char must be space or {.
            if !(rest.starts_with(' ') || rest.starts_with('{')) {
                continue;
            }
            if let Some(tok) = rest.split_whitespace().last()
                && let Ok(n) = tok.parse::<f64>()
            {
                return Some(n);
            }
        }
    }
    None
}

/// Assert the producer + signage metric families are present. Returns (ok, no_timelord_peer value).
fn assert_metric_families(body: &str) -> (bool, f64) {
    println!("  --- metric family assertions ---");
    let mut ok = true;
    for fam in PRODUCER_FAMILIES {
        check(
            &format!("family present: {fam}"),
            body.contains(fam),
            &mut ok,
        );
    }
    for fam in SIGNAGE_FAMILIES {
        check(
            &format!("family present: {fam}"),
            body.contains(fam),
            &mut ok,
        );
    }
    // Honest note: the node metrics surface has no dedicated SP-interval histogram; the interval is
    // proven directly from captured SP frames (farmer cadence assertion), not from a _bucket series.
    if body.contains("signage") && body.contains("_bucket") {
        pass("signage-point interval histogram present");
    } else {
        println!(
            "  NOTE  no SP-interval histogram in the node metrics surface (only \
             fullnode_signage_points_total + fullnode_current_signage_point); interval is proven live \
             by the farmer cadence capture"
        );
    }
    let no_tl = metric_value(body, NO_TIMELORD_SERIES).unwrap_or(0.0);
    (ok, no_tl)
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    // rustls 0.23 needs a process-wide CryptoProvider before any TLS handshake; match the server.
    let _ = rustls::crypto::ring::default_provider().install_default();
    println!("node prerequisite validation harness");
    println!(
        "  target ws=wss://{}:{}/ws  metrics=http://{}:{}/metrics  network={}",
        args.ws_host, args.ws_port, args.metrics_host, args.metrics_port, args.network
    );

    // 1) Baseline metrics scrape: families must exist; record the no_timelord_peer wall counter.
    println!("\n== METRICS: baseline scrape ==");
    let (metrics_ok, baseline_no_tl, metrics_available) =
        match scrape_metrics(&args.metrics_host, args.metrics_port, 15).await {
            Ok(body) => {
                let (ok, no_tl) = assert_metric_families(&body);
                println!("  baseline {NO_TIMELORD_SERIES} = {no_tl}");
                (ok, no_tl, true)
            }
            Err(e) => {
                fail(&format!("scrape /metrics: {e}"));
                (false, 0.0, false)
            }
        };

    // 2) Timelord peer: NewPeakTimelord must arrive well-formed within the window.
    let timelord_ok = run_timelord_check(&args).await;

    // 3) no_timelord_peer wall must not have fired while a timelord was attached (the peer stayed
    //    connected across the whole timelord window). Only checkable when producer metrics exist.
    println!("\n== METRICS: no_timelord_peer wall (post-timelord-window) ==");
    let mut wall_ok = true;
    if metrics_available {
        match scrape_metrics(&args.metrics_host, args.metrics_port, 15).await {
            Ok(body) => match metric_value(&body, NO_TIMELORD_SERIES) {
                Some(after_val) => {
                    println!("  {NO_TIMELORD_SERIES}: baseline={baseline_no_tl} after={after_val}");
                    check(
                        "no_timelord_peer wall did NOT fire while synthetic timelord attached",
                        after_val <= baseline_no_tl,
                        &mut wall_ok,
                    );
                }
                None => {
                    println!(
                        "  N/A  no_timelord_peer series absent (producer metrics not on this image) \
                         — wall check not applicable"
                    );
                }
            },
            Err(e) => {
                fail(&format!("re-scrape /metrics: {e}"));
                wall_ok = false;
            }
        }
    } else {
        println!("  N/A  metrics endpoint unavailable — wall check skipped");
    }

    // 4) Farmer peer: NewSignagePoint prerequisites.
    let farmer_ok = run_farmer_check(&args).await;

    println!("\n== VERDICT ==");
    println!("  metrics families    : {}", verdict(metrics_ok));
    println!("  timelord gossip     : {}", verdict(timelord_ok));
    println!("  no_timelord wall    : {}", verdict(wall_ok));
    println!("  farmer prerequisites: {}", verdict(farmer_ok));
    let all = metrics_ok && timelord_ok && wall_ok && farmer_ok;
    println!("  OVERALL             : {}", verdict(all));
    if !all {
        println!(
            "  (a FAIL on the timelord/producer-metrics rows with farmer PASS is the NEGATIVE-CONTROL \
             signature of a pre-da60e8d image or a node not yet at tip — the harness is detecting absence)"
        );
    }
    std::process::exit(i32::from(!all));
}

fn verdict(ok: bool) -> &'static str {
    if ok { "PASS" } else { "FAIL" }
}

#[cfg(test)]
#[path = "../../tests/unit/bin/validate_node_ws.rs"]
mod tests;
