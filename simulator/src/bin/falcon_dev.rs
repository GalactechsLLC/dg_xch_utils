//! Isolated direct-Falcon development network. Production consensus is reused through
//! Engine and native primitives. The small plots/VDF parameters and simple TCP service
//! are intended for integration tests; there is no assume-valid or proof bypass.
use clap::{Parser, Subcommand};
use dg_xch_core::{
    blockchain::{coin::Coin, sized_bytes::Bytes32},
    consensus::{chain_definition::ChainDefinition, constants::ConsensusConstants},
    signatures::{
        SignatureProfile, SignatureSigner,
        falcon::{FalconSigner, SECRET_KEY_BYTES},
        falcon_block::{FalconBlock, MAX_ENVELOPE_BYTES},
        falcon_wallet,
    },
};
use dg_xch_node::{Engine, falcon::FalconPrimitives};
use dg_xch_simulator_lib::{
    chain::ChainBuilder,
    error::SimError,
    falcon::{FalconFarming, FalconPlotKeys},
    pos2::PlotSet,
};
use dg_xch_stores::{SqliteStore, traits::BlockStore};
use serde::{Deserialize, Serialize};
use std::{
    io::{Error, Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{Mutex, Semaphore},
};
use zeroize::Zeroizing;

#[derive(Parser)]
#[command(
    about = "Native consensus integration network with direct padded Falcon-512 authorization"
)]
struct Args {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Init {
        #[arg(long)]
        root: PathBuf,
    },
    Status {
        #[arg(long)]
        peer: String,
    },
    Prepare {
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        plots: PathBuf,
    },
    Node {
        #[arg(long)]
        chain: PathBuf,
        #[arg(long)]
        data: PathBuf,
        #[arg(long, default_value = "127.0.0.1:8544")]
        listen: String,
        #[arg(long)]
        peer: Option<String>,
    },
    Farm {
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        plots: PathBuf,
        #[arg(long, required = true)]
        peer: String,
        #[arg(long, default_value_t = 105)]
        blocks: u32,
        #[arg(long)]
        spend_test: bool,
    },
    Check {
        #[arg(long)]
        chain: PathBuf,
        #[arg(long, required = true, value_delimiter = ',')]
        peers: Vec<String>,
        #[arg(long, default_value_t = 104)]
        min_height: u32,
        #[arg(long, default_value_t = 1)]
        min_spends: u64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Status {
    profile: String,
    genesis: Bytes32,
    height: Option<u32>,
    header_hash: Option<Bytes32>,
    validated_blocks: usize,
    header_signatures: u64,
    transaction_signatures: u64,
}

fn error(value: impl std::fmt::Display) -> Error {
    Error::other(value.to_string())
}
fn definition(path: &Path) -> Result<ChainDefinition, Error> {
    let bytes = read_bounded(path, 64 * 1024)?;
    let definition: ChainDefinition = serde_json::from_slice(&bytes).map_err(|cause| error(format!("cannot reuse this development chain: {cause}; preserve old data and initialize fresh FN-DSA v2 development state")))?;
    if definition.signature_profile != SignatureProfile::FnDsa512V2 {
        return Err(error(
            "this service requires an explicit Falcon development chain",
        ));
    }
    definition.constants().map_err(error)?;
    Ok(definition)
}
fn read_bounded(path: &Path, maximum: usize) -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(maximum as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > maximum {
        return Err(error("file exceeds size limit"));
    }
    Ok(bytes)
}
fn write_new(path: &Path, bytes: &[u8], private: bool) -> Result<(), Error> {
    let parent = path
        .parent()
        .ok_or_else(|| error("missing parent directory"))?;
    std::fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temporary
            .as_file()
            .set_permissions(std::fs::Permissions::from_mode(if private {
                0o600
            } else {
                0o644
            }))?;
    }
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    temporary.persist_noclobber(path).map_err(|e| e.error)?;
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}
fn init(root: &Path) -> Result<(), Error> {
    let chain = root.join("common/chain.json");
    let keys = root.join("farmer-cpu/falcon-keys.bin");
    if chain.exists() || keys.exists() {
        let d = definition(&chain)?;
        load_keys(
            root,
            d.constants().map_err(error)?.genesis_challenge,
            &FalconFarming::default(),
        )?;
        println!("Existing Falcon development identity verified");
        return Ok(());
    }
    let d = ChainDefinition::falcon_development(format!(
        "dg_xch/falcon/dev/{}",
        Bytes32::const_new(dg_xch_core::utils::hash_256(
            FalconSigner::generate().map_err(error)?.public_key()
        ))
    ));
    let mut encoded = Zeroizing::new(b"DGXKEY02".to_vec());
    encoded.push(12);
    for _ in 0..25 {
        let signer = FalconSigner::generate().map_err(error)?;
        encoded.extend_from_slice(&signer.public_key());
        encoded.extend_from_slice(signer.secret_bytes());
    }
    write_new(&keys, &encoded, true)?;
    write_new(
        &chain,
        &serde_json::to_vec_pretty(&d).map_err(error)?,
        false,
    )?;
    println!("Initialized {}", d.handshake_network_id().map_err(error)?);
    Ok(())
}
fn load_keys(
    root: &Path,
    genesis: Bytes32,
    farming: &FalconFarming,
) -> Result<(Vec<FalconPlotKeys>, FalconSigner), Error> {
    let path = root.join("farmer-cpu/falcon-keys.bin");
    let metadata = std::fs::symlink_metadata(&path)?;
    if !metadata.is_file() {
        return Err(error("Falcon keys must be a regular file"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.mode() & 0o077 != 0 || metadata.nlink() != 1 {
            return Err(error(
                "Falcon key file requires private permissions and a single link",
            ));
        }
    }
    let encoded = Zeroizing::new(read_bounded(&path, 64 * 1024)?);
    if encoded.get(..8) != Some(b"DGXKEY02")
        || encoded.get(8) != Some(&12)
        || encoded.len() != 9 + 25 * (897 + SECRET_KEY_BYTES)
    {
        return Err(error(
            "invalid or legacy Falcon key file; preserve old data and regenerate development keys and plots for FN-DSA v2",
        ));
    }
    let mut signers = Vec::new();
    for bytes in encoded[9..].as_chunks::<{ 897 + SECRET_KEY_BYTES }>().0 {
        signers.push(
            FalconSigner::from_secret_bytes(bytes[..897].try_into().map_err(error)?, &bytes[897..])
                .map_err(error)?,
        );
    }
    let wallet = signers.pop().ok_or_else(|| error("missing wallet"))?;
    let mut signers = signers.into_iter();
    let mut plots = Vec::new();
    while let Some(plot) = signers.next() {
        plots.push(farming.load_keys(
            genesis,
            plot,
            signers.next().ok_or_else(|| error("missing pool key"))?,
        ));
    }
    Ok((plots, wallet))
}
fn prepare(root: &Path, plots: &Path) -> Result<(), Error> {
    let constants = definition(&root.join("common/chain.json"))?
        .constants()
        .map_err(error)?;
    let (keys, _) = load_keys(root, constants.genesis_challenge, &FalconFarming::default())?;
    let set = PlotSet::setup_keys(plots, keys, constants.plot_size_v2, 2, constants.is_testnet)?;
    println!("Falcon native plots ready: {}", set.plots.len());
    Ok(())
}

struct NodeState {
    constants: ConsensusConstants,
    store: Arc<SqliteStore>,
    frames: Vec<Vec<u8>>,
    data: PathBuf,
    header_signatures: u64,
    transaction_signatures: u64,
    failed: bool,
    _scratch: tempfile::TempDir,
}
impl NodeState {
    async fn open(chain: &Path, data: PathBuf) -> Result<Self, Error> {
        let constants = definition(chain)?.constants().map_err(error)?;
        std::fs::create_dir_all(&data)?;
        // The durable source is the complete signed envelope log. Rebuild the SQLite
        // index by full consensus replay on every start, including signature/VDF checks.
        let scratch = tempfile::tempdir_in(&data)?;
        let store = Arc::new(
            SqliteStore::open(&scratch.path().join("chain.sqlite"))
                .await
                .map_err(error)?,
        );
        let mut node = Self {
            constants,
            store,
            frames: vec![],
            data,
            header_signatures: 0,
            transaction_signatures: 0,
            failed: false,
            _scratch: scratch,
        };
        let files = std::fs::read_dir(&node.data)?
            .filter_map(Result::ok)
            .filter(|e| e.path().extension().is_some_and(|x| x == "falcon"))
            .count();
        if files > 4096 {
            return Err(error("development chain log exceeds 4096 blocks"));
        }
        for height in 0..files {
            let bytes = read_bounded(&node.path(height), MAX_ENVELOPE_BYTES)?;
            node.accept(bytes, false).await?;
        }
        Ok(node)
    }
    fn path(&self, height: usize) -> PathBuf {
        self.data.join(format!("block-{height:010}.falcon"))
    }
    async fn status(&self) -> Result<Status, Error> {
        if self.failed {
            return Err(error("durable log write failed; restart required"));
        }
        let peak = self.store.get_peak().await.map_err(error)?;
        Ok(Status {
            profile: "fn_dsa512_v2_direct".into(),
            genesis: self.constants.genesis_challenge,
            height: peak.map(|p| p.1),
            header_hash: peak.map(|p| p.0),
            validated_blocks: self.frames.len(),
            header_signatures: self.header_signatures,
            transaction_signatures: self.transaction_signatures,
        })
    }
    async fn accept(&mut self, bytes: Vec<u8>, persist: bool) -> Result<(), Error> {
        if self.failed || self.frames.len() >= 4096 {
            return Err(error("node unavailable or development block limit reached"));
        }
        let envelope = FalconBlock::from_bytes(&bytes, self.constants.genesis_challenge)?;
        let height = envelope.block.height() as usize;
        if height < self.frames.len() && self.frames[height] == bytes {
            return Ok(());
        }
        if height != self.frames.len() {
            return Err(error("expected the next contiguous block"));
        }
        let primitives = FalconPrimitives::new(envelope.clone(), &self.constants)?;
        let mut engine = Engine::new(self.store.clone(), primitives, self.constants);
        engine.add_block(&envelope.block).await.map_err(error)?;
        if persist && let Err(e) = write_new(&self.path(height), &bytes, false) {
            self.failed = true;
            return Err(e);
        }
        self.header_signatures += envelope.header.len() as u64;
        self.transaction_signatures += envelope.transactions.len() as u64;
        self.frames.push(bytes);
        println!(
            "{}",
            serde_json::to_string(&self.status().await?).map_err(error)?
        );
        Ok(())
    }
}

async fn read_frame(stream: &mut TcpStream) -> Result<Vec<u8>, Error> {
    let length = stream.read_u32().await? as usize;
    if length == 0 || length > MAX_ENVELOPE_BYTES + 1 {
        return Err(error("invalid frame length"));
    }
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes).await?;
    Ok(bytes)
}
async fn send_frame(stream: &mut TcpStream, bytes: &[u8]) -> Result<(), Error> {
    if bytes.len() > MAX_ENVELOPE_BYTES + 1 {
        return Err(error("outgoing frame too large"));
    }
    stream.write_u32(bytes.len() as u32).await?;
    stream.write_all(bytes).await?;
    stream.flush().await
}
async fn request(peer: &str, request: &[u8]) -> Result<Vec<u8>, Error> {
    tokio::time::timeout(Duration::from_secs(30), async {
        let mut stream = TcpStream::connect(peer).await?;
        send_frame(&mut stream, request).await?;
        let response = read_frame(&mut stream).await?;
        if response[0] != 0 {
            return Err(error(String::from_utf8_lossy(&response[1..])));
        }
        Ok(response[1..].to_vec())
    })
    .await
    .map_err(error)?
}
async fn status(peer: &str) -> Result<Status, Error> {
    serde_json::from_slice(&request(peer, &[0]).await?).map_err(error)
}
async fn handle(mut stream: TcpStream, state: Arc<Mutex<NodeState>>) -> Result<(), Error> {
    let frame = tokio::time::timeout(Duration::from_secs(10), read_frame(&mut stream))
        .await
        .map_err(error)??;
    let response = match frame[0] {
        0 if frame.len() == 1 => {
            serde_json::to_vec(&state.lock().await.status().await?).map_err(error)
        }
        1 => state
            .lock()
            .await
            .accept(frame[1..].to_vec(), true)
            .await
            .map(|_| Vec::new()),
        2 if frame.len() == 5 => {
            let height = u32::from_be_bytes(frame[1..].try_into().map_err(error)?) as usize;
            state
                .lock()
                .await
                .frames
                .get(height)
                .cloned()
                .ok_or_else(|| error("block unavailable"))
        }
        _ => Err(error("unknown development protocol request")),
    };
    let mut bytes = vec![u8::from(response.is_err())];
    match response {
        Ok(value) => bytes.extend(value),
        Err(e) => bytes.extend(e.to_string().bytes().take(1024)),
    }
    send_frame(&mut stream, &bytes).await
}
async fn node(
    chain: PathBuf,
    data: PathBuf,
    listen: String,
    peer: Option<String>,
) -> Result<(), Error> {
    let state = Arc::new(Mutex::new(NodeState::open(&chain, data).await?));
    let listener = TcpListener::bind(&listen).await?;
    println!("Falcon node listening at {listen}");
    if let Some(peer) = peer {
        let state = state.clone();
        tokio::spawn(async move {
            loop {
                let next = state.lock().await.frames.len() as u32;
                let mut query = vec![2];
                query.extend_from_slice(&next.to_be_bytes());
                if let Ok(bytes) = request(&peer, &query).await {
                    if let Err(e) = state.lock().await.accept(bytes, true).await {
                        eprintln!("peer block rejected: {e}");
                        tokio::time::sleep(Duration::from_millis(250)).await;
                    }
                } else {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
            }
        });
    }
    let slots = Arc::new(Semaphore::new(8));
    loop {
        let permit = slots.clone().acquire_owned().await.map_err(error)?;
        let (stream, _) = listener.accept().await?;
        let state = state.clone();
        tokio::spawn(async move {
            let _permit = permit;
            if let Err(e) = handle(stream, state).await {
                eprintln!("request rejected: {e}");
            }
        });
    }
}

async fn farm(
    root: PathBuf,
    plots: PathBuf,
    peer: String,
    blocks: u32,
    spend_test: bool,
) -> Result<(), Error> {
    if !(1..=4096).contains(&blocks) {
        return Err(error("blocks must be in 1..4096"));
    }
    let constants = definition(&root.join("common/chain.json"))?
        .constants()
        .map_err(error)?;
    let existing = status(&peer).await?;
    if existing.genesis != constants.genesis_challenge || existing.validated_blocks != 0 {
        return Err(error("farmer requires a matching empty development chain"));
    }
    let farming = FalconFarming::default();
    let (keys, wallet) = load_keys(&root, constants.genesis_challenge, &farming)?;
    let reward = falcon_wallet::puzzle(&wallet.public_key()).tree_hash();
    let plots = PlotSet::setup_keys(
        &plots,
        keys,
        constants.plot_size_v2,
        2,
        constants.is_testnet,
    )?;
    let scratch = tempfile::tempdir()?;
    let store = Arc::new(
        SqliteStore::open(&scratch.path().join("farmer.sqlite"))
            .await
            .map_err(error)?,
    );
    let mut chain = ChainBuilder::new_with_primitives(
        store,
        FalconPrimitives::default(),
        constants,
        plots,
        reward,
    );
    let authorizer = farming.clone();
    chain.set_authorization_factory(Arc::new(move |block| {
        authorizer.primitives(block, &constants)
    }));
    let mut spend_coin: Option<Coin> = None;
    let mut spent = false;
    for height in 0..blocks {
        let outcome = if height == 0 {
            chain.farm_genesis().await
        } else if spend_test && !spent && height >= 5 && spend_coin.is_some() {
            let coin = spend_coin.ok_or_else(|| error("missing spend coin"))?;
            let spend =
                falcon_wallet::spend(&wallet, coin, reward, coin.amount, &constants, height)?;
            farming.admit_signatures(&spend, &constants, height)?;
            match chain.farm_next_tx_with_bundle(spend.bundle).await {
                Ok(outcome) => {
                    spent = true;
                    Ok(outcome)
                }
                Err(SimError::SubSlotExhausted) => chain.farm_next_slot().await,
                other => other,
            }
        } else {
            match chain.farm_next_tx().await {
                Err(SimError::SubSlotExhausted) => chain.farm_next_slot().await,
                other => other,
            }
        };
        outcome.map_err(error)?;
        if let Some(delta) = chain.take_last_delta()
            && spend_coin.is_none()
        {
            spend_coin = delta
                .additions
                .iter()
                .find(|record| record.coin.puzzle_hash == reward && record.coin.amount > 0)
                .map(|r| r.coin);
        }
        let envelope = chain
            .engine()
            .primitives()
            .block()
            .ok_or_else(|| error("missing Falcon block"))?;
        let mut request_bytes = vec![1];
        request_bytes.extend(envelope.to_bytes()?);
        request(&peer, &request_bytes).await?;
        println!(
            "farmed height={height} hash={} Falcon_header_signatures={} Falcon_spend_signatures={}",
            envelope.block.header_hash()?,
            envelope.header.len(),
            envelope.transactions.len()
        );
    }
    if spend_test && !spent {
        return Err(error(
            "chain finished without confirming the requested Falcon spend",
        ));
    }
    Ok(())
}
async fn check(
    chain: PathBuf,
    peers: Vec<String>,
    min_height: u32,
    min_spends: u64,
) -> Result<(), Error> {
    let constants = definition(&chain)?.constants().map_err(error)?;
    let mut expected = None;
    for peer in peers {
        let status = status(&peer).await?;
        if status.profile != "fn_dsa512_v2_direct"
            || status.genesis != constants.genesis_challenge
            || status.height.is_none_or(|h| h < min_height)
            || status.transaction_signatures < min_spends
            || status.header_signatures < status.validated_blocks as u64 * 3
        {
            return Err(error(format!(
                "{peer} has not met Falcon acceptance thresholds: {status:?}"
            )));
        }
        let peak = (status.height, status.header_hash);
        if expected.is_some_and(|e| e != peak) {
            return Err(error("Falcon nodes disagree on their peak"));
        }
        expected = Some(peak);
        println!("{peer} {}", serde_json::to_string(&status).map_err(error)?);
    }
    Ok(())
}
#[tokio::main(worker_threads = 2)]
async fn main() -> Result<(), Error> {
    dg_xch_core::compute::configure(2).map_err(error)?;
    match Args::parse().command {
        Command::Init { root } => init(&root),
        Command::Status { peer } => {
            println!(
                "{}",
                serde_json::to_string(&status(&peer).await?).map_err(error)?
            );
            Ok(())
        }
        Command::Prepare { root, plots } => prepare(&root, &plots),
        Command::Node {
            chain,
            data,
            listen,
            peer,
        } => node(chain, data, listen, peer).await,
        Command::Farm {
            root,
            plots,
            peer,
            blocks,
            spend_test,
        } => farm(root, plots, peer, blocks, spend_test).await,
        Command::Check {
            chain,
            peers,
            min_height,
            min_spends,
        } => check(chain, peers, min_height, min_spends).await,
    }
}
