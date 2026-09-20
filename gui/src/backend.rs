use crate::config::{AppPaths, GpuBackend, Settings};
use dg_xch_clients::rpc::full_node::FullnodeAPI;
use dg_xch_core::blockchain::blockchain_state::BlockchainState;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_farmer::FarmerService;
use dg_xch_farmer::farmer::config::Config;
use dg_xch_plotter::PlotRequest;
use dg_xch_plotter::backend::{
    GpuBackend as SelectedGpuBackend, GpuDevice, GpuPreference, GpuSelection, parse_cuda_probe,
    select_gpu,
};
use dg_xch_pos2::plotting::PlotLimits;
use dg_xch_wallet::accounts::{Account, WalletSession, WalletSnapshot};
use std::collections::HashMap;
use std::io::Error;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::runtime::Runtime;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use zeroize::Zeroizing;

#[derive(Clone)]
pub struct AccountView {
    pub account: Account,
    pub unlocked: bool,
    pub snapshot: Option<WalletSnapshot>,
    pub error: Option<String>,
    pub updated: Option<Instant>,
}

#[derive(Clone, Default)]
pub struct State {
    pub settings: Option<Settings>,
    pub accounts: Vec<AccountView>,
    pub node: Option<BlockchainState>,
    pub node_error: Option<String>,
    pub node_updated: Option<Instant>,
    pub node_metrics: String,
    pub node_details: String,
    pub farmer_running: bool,
    pub farmer_stats: String,
    pub plot_job: Option<String>,
    pub inventory: Vec<(PathBuf, String)>,
    pub notice: String,
}

pub enum Command {
    Import {
        name: String,
        mnemonic: Zeroizing<String>,
        password: Zeroizing<String>,
    },
    Unlock {
        id: String,
        password: Zeroizing<String>,
    },
    Lock(String),
    Send {
        id: String,
        address: String,
        amount: u64,
        fee: u64,
    },
    Settings(Settings),
    StartFarmer,
    StartAccountFarmer {
        id: String,
        password: Zeroizing<String>,
    },
    StopFarmer,
    ScanPlots,
    Plot {
        request: PlotRequest,
        output: PathBuf,
        limits: PlotLimits,
        gpu: bool,
    },
    ProvePlot {
        path: PathBuf,
        challenge: Bytes32,
        testnet: bool,
        gpu: bool,
    },
    CancelPlot,
}

enum WalletCommand {
    Send {
        destination: Bytes32,
        amount: u64,
        fee: u64,
    },
}

pub struct Backend {
    _instance_lock: std::fs::File,
    runtime: Option<Runtime>,
    pub state: Arc<Mutex<State>>,
    commands: mpsc::Sender<Command>,
    cancelled: Arc<AtomicBool>,
}

impl Backend {
    pub fn new(paths: AppPaths, settings: Settings) -> Result<Self, Error> {
        Self::new_inner(paths, settings, true)
    }

    pub fn for_smoke_test(paths: AppPaths, settings: Settings) -> Result<Self, Error> {
        Self::new_inner(paths, settings, false)
    }

    fn new_inner(
        paths: AppPaths,
        settings: Settings,
        network_enabled: bool,
    ) -> Result<Self, Error> {
        std::fs::create_dir_all(&paths.data)?;
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let instance_lock = options.open(paths.data.join("desktop.lock"))?;
        instance_lock.try_lock().map_err(|error| {
            Error::other(format!(
                "another desktop instance may be using these wallets: {error}"
            ))
        })?;
        let runtime = Runtime::new()?;
        let state = Arc::new(Mutex::new(State::default()));
        let (commands, receiver) = mpsc::channel(32);
        let (configuration, updates) = watch::channel(settings);
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut accounts = Vec::new();
        match std::fs::read_dir(paths.accounts()) {
            Ok(entries) => {
                for entry in entries {
                    let entry = entry?;
                    if entry
                        .path()
                        .extension()
                        .and_then(|extension| extension.to_str())
                        != Some("json")
                    {
                        continue;
                    }
                    let id = entry
                        .path()
                        .file_stem()
                        .and_then(|name| name.to_str())
                        .unwrap_or_default()
                        .to_string();
                    let account = Account::load(&paths.accounts(), &id)?;
                    accounts.push(AccountView {
                        account,
                        unlocked: false,
                        snapshot: None,
                        error: None,
                        updated: None,
                    });
                    if accounts.len() > 64 {
                        return Err(Error::other("account limit exceeded"));
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        state
            .lock()
            .map_err(|_| Error::other("desktop state poisoned"))?
            .accounts = accounts;
        state
            .lock()
            .map_err(|_| Error::other("desktop state poisoned"))?
            .settings = Some(configuration.borrow().clone());
        if network_enabled {
            runtime.spawn(node_worker(updates, state.clone()));
        }
        runtime.spawn(command_worker(
            paths,
            configuration,
            receiver,
            state.clone(),
            cancelled.clone(),
        ));
        Ok(Self {
            _instance_lock: instance_lock,
            runtime: Some(runtime),
            state,
            commands,
            cancelled,
        })
    }

    pub fn command(&self, command: Command) {
        if let Err(error) = self.commands.try_send(command) {
            self.state
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .notice = format!("Command queue unavailable: {error}");
        }
    }

    pub fn snapshot(&self) -> State {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

fn update(state: &Arc<Mutex<State>>, action: impl FnOnce(&mut State)) {
    action(&mut state.lock().unwrap_or_else(|error| error.into_inner()));
}

async fn node_worker(mut configuration: watch::Receiver<Settings>, state: Arc<Mutex<State>>) {
    loop {
        let settings = configuration.borrow_and_update().clone();
        let result = async {
            let client = settings.client()?;
            let blockchain = client.get_blockchain_state().await?;
            let metrics = match client.get_block_count_metrics().await {
                Ok(metrics) => serde_json::to_string_pretty(&metrics).map_err(Error::other)?,
                Err(error) => format!("Block counters unavailable: {error:?}"),
            };
            let details = match client.get_node_details().await {
                Ok(details) => serde_json::to_string_pretty(&details).map_err(Error::other)?,
                Err(error) => format!("Node internals unavailable: {error}"),
            };
            Ok::<_, Error>((blockchain, metrics, details))
        }
        .await;
        if configuration.has_changed().unwrap_or(false) {
            continue;
        }
        update(&state, |state| match result {
            Ok((node, metrics, details)) => {
                state.node = Some(node);
                state.node_metrics = metrics;
                state.node_details = details;
                state.node_updated = Some(Instant::now());
                state.node_error = None;
            }
            Err(error) => state.node_error = Some(error.to_string()),
        });
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(settings.poll_seconds)) => {},
            result = configuration.changed() => if result.is_err() { break; },
        }
    }
}

async fn wallet_worker(
    id: String,
    mut session: WalletSession,
    mut commands: mpsc::Receiver<WalletCommand>,
    state: Arc<Mutex<State>>,
    seconds: u64,
) {
    let mut interval = tokio::time::interval(Duration::from_secs(seconds));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        let result = tokio::select! {
            _ = interval.tick() => session.sync().await.map(|_| ()),
            command = commands.recv() => match command {
                Some(WalletCommand::Send { destination, amount, fee }) => {
                    let result = session.send(destination, amount, fee).await;
                    if let Ok(transaction) = &result { update(&state, |state| state.notice = format!("Transaction submitted: {transaction}")); }
                    result.map(|_| ())
                },
                None => break,
            }
        };
        update(&state, |state| {
            if let Some(account) = state
                .accounts
                .iter_mut()
                .find(|account| account.account.id == id)
            {
                account.snapshot = Some(session.snapshot());
                account.error = result.err().map(|error| error.to_string());
                account.updated = Some(Instant::now());
            }
        });
    }
}

async fn command_worker(
    paths: AppPaths,
    configuration: watch::Sender<Settings>,
    mut commands: mpsc::Receiver<Command>,
    state: Arc<Mutex<State>>,
    cancelled: Arc<AtomicBool>,
) {
    let mut wallets: HashMap<String, (mpsc::Sender<WalletCommand>, JoinHandle<()>)> =
        HashMap::new();
    let mut farmer: Option<FarmerService> = None;
    let mut plot: Option<JoinHandle<()>> = None;
    let mut inventory: Option<JoinHandle<()>> = None;
    let mut tick = tokio::time::interval(Duration::from_secs(2));
    loop {
        let command = tokio::select! {
            command = commands.recv() => match command { Some(command) => command, None => break },
            _ = tick.tick() => {
                if let Some(error) = farmer.as_mut().and_then(FarmerService::failure) {
                    if let Some(service) = farmer.take() { service.stop().await; }
                    update(&state, |state| { state.farmer_running = false; state.notice = error; });
                }
                if let Some(service) = &farmer {
                    let recent = service.state.most_recent_sp.read().await;
                    let counts = dg_xch_core::protocols::farmer::SerialPlotCounts::from(service.state.plot_counts.as_ref());
                    let counts = serde_json::to_string_pretty(&counts).unwrap_or_default();
                    update(&state, |state| state.farmer_stats = format!("Signage point: {}\nChallenge: {}\nLast point: {} seconds ago\nPlots: {counts}", recent.index, recent.hash, recent.timestamp.elapsed().as_secs()));
                }
                continue;
            }
        };
        let settings = configuration.borrow().clone();
        let result: Result<(), Error> = async {
            match command {
                Command::Import { name, mnemonic, password } => {
                    if state.lock().unwrap_or_else(|error| error.into_inner()).accounts.len() >= 64 { return Err(Error::other("account limit reached")); }
                    let account = tokio::task::spawn_blocking(move || Account::import(name, settings.network, &mnemonic, &password)).await.map_err(Error::other)??;
                    account.save_new(&paths.accounts())?;
                    update(&state, |state| { state.accounts.push(AccountView { account, unlocked: false, snapshot: None, error: None, updated: None }); state.notice = "Encrypted account imported. Back up your mnemonic independently.".into(); });
                },
                Command::Unlock { id, password } => {
                    if wallets.contains_key(&id) { return Err(Error::other("wallet is already unlocked")); }
                    let account = Account::load(&paths.accounts(), &id)?;
                    if account.network != settings.network { return Err(Error::other("account belongs to another network")); }
                    let genesis = Bytes32::from_str(&settings.genesis_header_hash).map_err(|_| Error::other("configure the trusted genesis block HEADER hash before unlocking"))?;
                    let secret = tokio::task::spawn_blocking(move || account.unlock(&password)).await.map_err(Error::other)??;
                    let session = WalletSession::new(secret, settings.client()?, Arc::new(settings.constants()?), genesis, paths.data.join("wallets").join(&id).join(format!("{}.sqlite", hex::encode(genesis)))).await?;
                    let (sender, receiver) = mpsc::channel(4);
                    update(&state, |state| if let Some(account) = state.accounts.iter_mut().find(|account| account.account.id == id) { account.unlocked = true; account.snapshot = Some(session.snapshot()); account.error = None; });
                    let worker = tokio::spawn(wallet_worker(id.clone(), session, receiver, state.clone(), settings.poll_seconds));
                    wallets.insert(id, (sender, worker));
                },
                Command::Lock(id) => {
                    if let Some((_, handle)) = wallets.remove(&id) { handle.abort(); let _ = handle.await; }
                    update(&state, |state| if let Some(account) = state.accounts.iter_mut().find(|account| account.account.id == id) { account.unlocked = false; account.snapshot = None; account.error = None; });
                },
                Command::Send { id, address, amount, fee } => {
                    let destination = dg_xch_keys::decode_puzzle_hash(&address)?;
                    if dg_xch_keys::encode_puzzle_hash(&destination, settings.constants()?.bech32_prefix)? != address.to_lowercase() { return Err(Error::other("destination belongs to another network")); }
                    wallets.get(&id).ok_or_else(|| Error::other("wallet is locked"))?.0.try_send(WalletCommand::Send { destination, amount, fee }).map_err(Error::other)?;
                    update(&state, |state| state.notice = "Transaction queued for revalidation and signing.".into());
                },
                Command::Settings(settings) => {
                    if !wallets.is_empty() || farmer.is_some() { return Err(Error::other("lock wallets and stop farming before changing settings")); }
                    paths.save(&settings)?;
                    configuration.send(settings.clone()).map_err(Error::other)?;
                    update(&state, |state| state.settings = Some(settings));
                    update(&state, |state| { state.node = None; state.node_updated = None; state.notice = "Settings saved.".into(); });
                },
                Command::StartFarmer => {
                    if farmer.is_some() { return Err(Error::other("farmer is already running")); }
                    let config: Config<()> = Config::try_from(PathBuf::from(&settings.farmer_config).as_path())?;
                    if config.selected_network != settings.network || config.constants()? != settings.constants()? { return Err(Error::other("farmer and desktop consensus settings differ")); }
                    farmer = Some(FarmerService::start(config).await?);
                    update(&state, |state| { state.farmer_running = true; state.notice = "Farmer started; waiting for full-node signage points.".into(); });
                },
                Command::StartAccountFarmer { id, password } => {
                    if farmer.is_some() { return Err(Error::other("farmer is already running")); }
                    let account = Account::load(&paths.accounts(), &id)?;
                    if account.network != settings.network { return Err(Error::other("farming account belongs to another network")); }
                    let secret = tokio::task::spawn_blocking(move || account.unlock(&password)).await.map_err(Error::other)??;
                    let constants = settings.constants()?;
                    let payout = dg_xch_keys::decode_puzzle_hash(&settings.farmer_payout_address)?;
                    if dg_xch_keys::encode_puzzle_hash(&payout, constants.bech32_prefix)? != settings.farmer_payout_address.to_lowercase() { return Err(Error::other("farmer payout address belongs to another network")); }
                    let mut config: Config<()> = Config { selected_network: settings.network, ..Default::default() };
                    if !settings.chain_definition_path.is_empty() {
                        config.chain_definition = Some(serde_json::from_reader(std::fs::File::open(settings.chain_definition_path)?).map_err(Error::other)?);
                    }
                    config.fullnode_ws_host = settings.farmer_ws_host;
                    config.fullnode_ws_port = settings.farmer_ws_port;
                    config.fullnode_rpc_host = settings.node_host;
                    config.fullnode_rpc_port = settings.node_port;
                    config.ssl_root_path = if settings.farmer_ssl_root.is_empty() { None } else { Some(settings.farmer_ssl_root) };
                    config.payout_address = settings.farmer_payout_address;
                    config.farmer_info.push(dg_xch_farmer::farmer::config::FarmingInfo {
                        farmer_secret_key: dg_xch_keys::master_sk_to_farmer_sk(&secret)?.to_bytes().into(),
                        pool_secret_key: Some(dg_xch_keys::master_sk_to_pool_sk(&secret)?.to_bytes().into()),
                        ..Default::default()
                    });
                    config.harvester_configs.druid_garden = Some(dg_xch_farmer::farmer::config::DruidGardenHarvesterConfig {
                        plot_directories: settings.plot_directories.iter().map(|path| path.to_string_lossy().into_owned()).collect(),
                    });
                    farmer = Some(FarmerService::start(config).await?);
                    update(&state, |state| { state.farmer_running = true; state.notice = "Account farmer started. Derived farming keys are not written to disk.".into(); });
                },
                Command::StopFarmer => {
                    if let Some(service) = farmer.take() { service.stop().await; }
                    update(&state, |state| { state.farmer_running = false; state.farmer_stats.clear(); });
                },
                Command::Plot { request, output, limits, gpu } => {
                    if plot.as_ref().is_some_and(|job| !job.is_finished()) { return Err(Error::other("a plot job is already running")); }
                    cancelled.store(false, Ordering::Release);
                    let cancelled = cancelled.clone();
                    let state = state.clone();
                    update(&state, |state| state.plot_job = Some("Selecting plotting backend".into()));
                    let (selection, job_label) = resolve_execution(&settings, gpu, &cancelled).await?;
                    if selection.as_ref().is_some_and(|selection| selection.backend == SelectedGpuBackend::Cuda) {
                        use std::ffi::OsString;
                        let mut args: Vec<OsString> = vec!["--output".into(), output.into_os_string(), "--farmer-key".into(), hex::encode(request.farmer_public_key).into(),
                            "--k".into(), request.k.to_string().into(), "--strength".into(), request.strength.to_string().into(),
                            "--index".into(), request.index.to_string().into(), "--meta-group".into(), request.meta_group.to_string().into(),
                            "--memory-mib".into(), (limits.memory_bytes / 1024 / 1024).to_string().into(), "--max-entries".into(), limits.max_entries.to_string().into(), "--max-work".into(), limits.max_work.to_string().into()];
                        match request.pool {
                            dg_xch_plotter::PoolBinding::Contract(hash) => args.extend(["--contract".into(), hex::encode(hash).into()]),
                            dg_xch_plotter::PoolBinding::PublicKey(key) => args.extend(["--pool-key".into(), hex::encode(key).into()]),
                        }
                        if request.testnet { args.push("--testnet".into()); }
                        update(&state, |state| state.plot_job = Some(format!("Running {job_label}")));
                        plot = Some(tokio::spawn(async move {
                            let result = run_cuda(&settings, args, &cancelled, Duration::from_secs(3600)).await;
                            update(&state, |state| state.plot_job = Some(format!("{job_label}: {}", result.unwrap_or_else(|error| format!("GPU job stopped: {error}")))));
                        }));
                        return Ok(());
                    }
                    update(&state, |state| state.plot_job = Some(format!("Running {job_label}")));
                    plot = Some(tokio::task::spawn_blocking(move || {
                        let result = if let Some(selection) = selection {
                            dg_xch_plotter::create_with_engine(&request, &output, limits, &cancelled, |params, limits, cancelled| dg_xch_pos2::vulkan::build(params, limits, cancelled, selection.device.ordinal))
                        } else {
                            dg_xch_plotter::create(&request, &output, limits, &cancelled)
                        };
                        update(&state, |state| state.plot_job = Some(match result {
                            Ok(info) => format!("{job_label}: complete: {} ({} bytes)", output.display(), info.file_bytes),
                            Err(error) => format!("{job_label}: stopped: {error}"),
                        }));
                    }));
                },
                Command::CancelPlot => { cancelled.store(true, Ordering::Release); },
                Command::ProvePlot { path, challenge, testnet, gpu } => {
                    if plot.as_ref().is_some_and(|job| !job.is_finished()) { return Err(Error::other("a plot job is already running")); }
                    cancelled.store(false, Ordering::Release);
                    let cancelled = cancelled.clone();
                    let state = state.clone();
                    update(&state, |state| state.plot_job = Some("Selecting development proof-check backend".into()));
                    let (selection, job_label) = resolve_execution(&settings, gpu, &cancelled).await?;
                    update(&state, |state| state.plot_job = Some(format!("{job_label}: development proof check reconstructs the entire plot under resource limits")));
                    if selection.as_ref().is_some_and(|selection| selection.backend == SelectedGpuBackend::Cuda) {
                        let mut args = vec!["--prove-plot".into(), path.into_os_string(), "--challenge".into(), hex::encode(challenge).into()];
                        if testnet { args.push("--testnet".into()); }
                        plot = Some(tokio::spawn(async move {
                            let result = run_cuda(&settings, args, &cancelled, Duration::from_secs(3600)).await;
                            update(&state, |state| state.plot_job = Some(format!("{job_label}: {}", result.unwrap_or_else(|error| format!("GPU proof check stopped: {error}")))));
                        }));
                    } else {
                        plot = Some(tokio::task::spawn_blocking(move || {
                            let result = (|| {
                                let harvester = if let Some(selection) = selection {
                                    dg_xch_farmer::harvesters::pos2::DevelopmentHarvester::open_with_engine(&path, testnet, PlotLimits::default(), &cancelled, |params, limits, cancelled| dg_xch_pos2::vulkan::build(params, limits, cancelled, selection.device.ordinal))?
                                } else {
                                    dg_xch_farmer::harvesters::pos2::DevelopmentHarvester::open(&path, testnet, PlotLimits::default(), &cancelled)?
                                };
                                let proofs = harvester.challenge(challenge, dg_xch_pos2::chainer::SearchLimits { max_hashes: 100_000_000, max_results: 1024 }, &cancelled)?;
                                Ok::<_, Error>(format!("Canonical plot verified. {} independently validated candidate proofs; not submitted to the network.", proofs.len()))
                            })();
                            update(&state, |state| state.plot_job = Some(format!("{job_label}: {}", result.unwrap_or_else(|error| error.to_string()))));
                        }));
                    }
                },
                Command::ScanPlots => {
                    if inventory.as_ref().is_some_and(|job| !job.is_finished()) { return Err(Error::other("plot scan is already running")); }
                    let state = state.clone();
                    inventory = Some(tokio::task::spawn_blocking(move || {
                        let mut plots = Vec::new();
                        for directory in settings.plot_directories {
                            match std::fs::read_dir(&directory) {
                                Ok(entries) => for entry in entries.flatten().take(100_000) {
                                    let path = entry.path();
                                    if path.extension().and_then(|value| value.to_str()) != Some("plot") { continue; }
                                    let description = match dg_xch_plotter::inspect(&path) {
                                        Ok(info) => format!("PoS2 · k{} · strength {} · {} bytes", info.k, info.strength, info.file_bytes),
                                        Err(_) => "Legacy plot or invalid PoS2 header; farmer validates legacy plots on load".into(),
                                    };
                                    plots.push((path, description));
                                },
                                Err(error) => plots.push((directory, format!("Directory error: {error}"))),
                            }
                        }
                        update(&state, |state| state.inventory = plots);
                    }));
                },
            }
            Ok(())
        }.await;
        if let Err(error) = result {
            update(&state, |state| state.notice = error.to_string());
        }
    }
    cancelled.store(true, Ordering::Release);
    for (_, (_, handle)) in wallets {
        handle.abort();
    }
    if let Some(service) = farmer {
        service.stop().await;
    }
}

async fn resolve_execution(
    settings: &Settings,
    gpu: bool,
    cancelled: &AtomicBool,
) -> Result<(Option<GpuSelection>, String), Error> {
    if !gpu {
        return Ok((None, "native Rust CPU".into()));
    }
    let preference = match settings.gpu_backend {
        GpuBackend::Auto => GpuPreference::Auto,
        GpuBackend::Cuda => GpuPreference::Cuda(settings.cuda_device),
        GpuBackend::Vulkan => GpuPreference::Vulkan(settings.vulkan_device),
    };
    let mut cuda_note = String::new();
    let cuda = if preference == GpuPreference::Auto || matches!(preference, GpuPreference::Cuda(_))
    {
        let probe = run_cuda(
            settings,
            vec!["--probe-device".into()],
            cancelled,
            Duration::from_secs(5),
        )
        .await
        .and_then(|output| parse_cuda_probe(&output, settings.cuda_device));
        match probe {
            Ok(device) => Some(device),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => return Err(error),
            Err(error) if preference == GpuPreference::Auto => {
                cuda_note = format!(
                    "; CUDA unavailable: {}",
                    error.to_string().chars().take(256).collect::<String>()
                );
                None
            }
            Err(error) => return Err(error),
        }
    } else {
        None
    };
    let vulkan = if cuda.is_some() {
        Vec::new()
    } else {
        tokio::task::spawn_blocking(|| {
            dg_xch_pos2::vulkan::adapters()
                .into_iter()
                .map(|device| GpuDevice {
                    ordinal: device.ordinal,
                    vendor: device.vendor,
                    name: device.name,
                })
                .collect::<Vec<_>>()
        })
        .await
        .map_err(Error::other)?
    };
    let selection = select_gpu(preference, cuda.as_ref(), &vulkan)
        .map_err(|error| Error::other(format!("{error}{cuda_note}")))?;
    let backend = match selection.backend {
        SelectedGpuBackend::Cuda => "native Rust CUDA",
        SelectedGpuBackend::Vulkan => "Vulkan",
    };
    let automatic = if preference == GpuPreference::Auto {
        "Auto (vendor preference, not a benchmark): "
    } else {
        ""
    };
    let label = format!(
        "{automatic}{backend} #{} {} (vendor {:#06x}){cuda_note}",
        selection.device.ordinal, selection.device.name, selection.device.vendor,
    );
    Ok((Some(selection), label))
}

async fn run_cuda(
    settings: &Settings,
    mut args: Vec<std::ffi::OsString>,
    cancelled: &AtomicBool,
    timeout: Duration,
) -> Result<String, Error> {
    use tokio::io::AsyncReadExt;
    let executable = PathBuf::from(&settings.cuda_executable);
    if !executable.is_absolute() || !executable.is_file() {
        return Err(Error::other(
            "set an absolute path to the trusted cuda-oxide executable in Preferences",
        ));
    }
    args.extend(["--device".into(), settings.cuda_device.to_string().into()]);
    let mut child = tokio::process::Command::new(executable)
        .args(args)
        .kill_on_drop(true)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| Error::other("missing GPU stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| Error::other("missing GPU stderr"))?;
    async fn bounded(reader: impl tokio::io::AsyncRead + Unpin) -> Result<Vec<u8>, Error> {
        let mut bytes = Vec::new();
        reader.take(65_537).read_to_end(&mut bytes).await?;
        if bytes.len() > 65_536 {
            return Err(Error::other("GPU process output limit exceeded"));
        }
        Ok(bytes)
    }
    let execution = async {
        let (status, stdout, stderr) =
            tokio::try_join!(child.wait(), bounded(stdout), bounded(stderr))?;
        if !status.success() {
            return Err(Error::other(format!(
                "GPU process {status}: {}",
                String::from_utf8_lossy(&stderr)
            )));
        }
        let output = String::from_utf8_lossy(&stdout).into_owned();
        Ok(if output.trim().is_empty() {
            "GPU job complete; no candidate proofs for this challenge.".into()
        } else {
            output
        })
    };
    tokio::select! {
        result = execution => result,
        _ = async { while !cancelled.load(Ordering::Acquire) { tokio::time::sleep(Duration::from_millis(100)).await; } } => Err(Error::new(std::io::ErrorKind::Interrupted, "GPU job cancelled")),
        _ = tokio::time::sleep(timeout) => Err(Error::new(std::io::ErrorKind::TimedOut, "GPU process timed out")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn explicit_cpu_does_not_probe_configured_helper() {
        let settings = Settings {
            cuda_executable: "untrusted-relative-helper".into(),
            ..Default::default()
        };
        let (selection, label) = resolve_execution(&settings, false, &AtomicBool::new(false))
            .await
            .unwrap();
        assert!(selection.is_none());
        assert_eq!(label, "native Rust CPU");
    }

    #[tokio::test]
    async fn cuda_probe_rejects_relative_executable_paths() {
        let settings = Settings {
            cuda_executable: "dg_xch_plotter_cuda".into(),
            gpu_backend: GpuBackend::Cuda,
            ..Default::default()
        };
        let error = resolve_execution(&settings, true, &AtomicBool::new(false))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("absolute path"));
    }
}
