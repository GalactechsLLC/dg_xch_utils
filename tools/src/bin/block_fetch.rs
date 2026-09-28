use dg_xch_node::sync::BlockRangeSource;
use dg_xch_node::sync::source::{CapturingSource, OutboundPeerSource};
use dg_xch_p2p::peer::empty_handlers;
use dg_xch_p2p::{OutboundPeer, P2pSettings, dial};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut host = None;
    let mut port = 8444u16;
    let mut start = None;
    let mut end = None;
    let mut out = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut take = || args.next().ok_or(format!("missing value for {a}"));
        match a.as_str() {
            "--host" => host = Some(take()?),
            "--port" => port = take()?.parse::<u16>()?,
            "--start" => start = Some(take()?.parse::<u32>()?),
            "--end" => end = Some(take()?.parse::<u32>()?),
            "--out" => out = Some(PathBuf::from(take()?)),
            other => return Err(format!("unknown arg {other}").into()),
        }
    }
    let host = host.ok_or("--host required")?;
    let (start, end) = (
        start.ok_or("--start required")?,
        end.ok_or("--end required")?,
    );
    let out = out.ok_or("--out required")?;
    std::fs::create_dir_all(&out)?;

    // rustls 0.23 needs a process-wide CryptoProvider before any TLS handshake; match the server.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(async move {
        let run = Arc::new(AtomicBool::new(true));
        let settings = P2pSettings::default();
        let client = dial(&host, port, empty_handlers(), run.clone(), &settings).await?;
        let peer = Arc::new(OutboundPeer {
            endpoint: (host.clone(), port),
            client,
            run,
        });
        let source = CapturingSource::new(
            Arc::new(OutboundPeerSource::new(peer, Duration::from_secs(60))),
            out.clone(),
        );
        let mut window_start = start;
        while window_start <= end {
            let window_end = window_start.saturating_add(31).min(end);
            let blocks = source.fetch_range(window_start, window_end).await?;
            let tx_blocks = blocks
                .iter()
                .filter(|b| b.transactions_generator.is_some())
                .count();
            println!(
                "captured {window_start}..={window_end}: {} blocks ({tx_blocks} with generators)",
                blocks.len()
            );
            window_start = window_end.saturating_add(1);
        }
        Ok::<(), Box<dyn std::error::Error>>(())
    })
}
