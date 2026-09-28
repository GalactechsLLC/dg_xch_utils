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
    pub pool_settings: Vec<dg_xch_farmer::pool_management::PoolSettings>,
    pub plot_job: Option<String>,
    pub inventory: Vec<(PathBuf, String)>,
    pub notice: String,
    pub notice_error: bool,
    pub notice_revision: u64,
    pub import_result: Option<Result<String, String>>,
    pub plotting_keys: Option<(String, String, String)>,
    pub offers: HashMap<String, Vec<(Bytes32, String, bool)>>,
}

#[derive(Clone)]
pub enum OfferAction {
    Create {
        give: dg_xch_wallet::offers::OfferAmount,
        receive: dg_xch_wallet::offers::OfferAmount,
    },
    Take(String),
    Cancel(Bytes32),
}

pub enum Command {
    Offer {
        id: String,
        genesis: Bytes32,
        action: OfferAction,
        fee: u64,
    },
    PlottingKeys {
        id: String,
        password: Zeroizing<String>,
    },
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
    Asset {
        id: String,
        genesis: Bytes32,
        action: dg_xch_wallet::assets::AssetAction,
        fee: u64,
    },
    WatchCat {
        id: String,
        asset_id: Bytes32,
    },
    Settings(Settings),
    StartFarmer,
    StartAccountFarmer {
        id: String,
        password: Zeroizing<String>,
    },
    StopFarmer,
    LoadPools,
    UpdatePool {
        expected: Box<dg_xch_farmer::pool_management::PoolSettings>,
        payout: String,
        difficulty: u64,
    },
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
        memory_bytes: u64,
        max_work: u64,
    },
    CancelPlot,
}

enum WalletCommand {
    Offer {
        action: OfferAction,
        fee: u64,
    },
    Asset {
        action: dg_xch_wallet::assets::AssetAction,
        fee: u64,
    },
    WatchCat(Bytes32),
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
        let importing = matches!(&command, Command::Import { .. });
        update(&self.state, |state| {
            state.notice_revision = state.notice_revision.wrapping_add(1);
            state.notice = "Working…".into();
            state.notice_error = false;
            if importing {
                state.import_result = None;
            }
        });
        if let Err(error) = self.commands.try_send(command) {
            update(&self.state, |state| {
                state.notice = format!("Command queue unavailable: {error}");
                state.notice_error = true;
                if importing {
                    state.import_result = Some(Err(state.notice.clone()));
                }
            });
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
                Some(WalletCommand::Offer { action, fee }) => {
                    let result = match action {
                        OfferAction::Create { give, receive } => session.create_offer(give, receive, fee).await.map(|_| "Offer signed and saved. Its inputs remain reserved until spent. Copy it from the saved offers below.".to_string()),
                        OfferAction::Take(text) => session.take_offer(&text, fee).await.map(|id| format!("Offer acceptance submitted: {id}")),
                        OfferAction::Cancel(id) => session.cancel_offer(id, fee).await.map(|id| format!("On-chain cancellation submitted: {id}. The offer is not cancelled until this transaction confirms.")),
                    };
                    update(&state, |state| {
                        state.notice = match &result { Ok(message) => message.clone(), Err(error) => format!("Offer operation failed: {error}") };
                        state.notice_error = result.is_err();
                    });
                    result.map(|_| ())
                },
                Some(WalletCommand::WatchCat(asset_id)) => {
                    let result = session.watch_cat(asset_id).await;
                    update(&state, |state| { state.notice = match &result { Ok(_) => "CAT asset ID saved. Both CAT1 (read-only) and CAT2 addresses will be scanned.".into(), Err(error) => format!("Could not watch CAT: {error}") }; state.notice_error = result.is_err(); });
                    result
                },
                Some(WalletCommand::Asset { action, fee }) => {
                    let result = session.asset_transaction(action, fee).await;
                    update(&state, |state| {
                        state.notice = match &result { Ok(transaction) => format!("Asset transaction submitted: {transaction}"), Err(error) => format!("Asset transaction failed: {error}") };
                        state.notice_error = result.is_err();
                    });
                    result.map(|_| ())
                },
                Some(WalletCommand::Send { destination, amount, fee }) => {
                    let result = session.send(destination, amount, fee).await;
                    update(&state, |state| match &result {
                        Ok(transaction) => { state.notice = format!("Transaction submitted: {transaction}"); state.notice_error = false; }
                        Err(error) => { state.notice = format!("Payment failed: {error}"); state.notice_error = true; }
                    });
                    result.map(|_| ())
                },
                None => break,
            }
        };
        update(&state, |state| {
            state.offers.insert(
                id.clone(),
                session
                    .transactions()
                    .iter()
                    .filter_map(|transaction| {
                        transaction.offer.as_ref().and_then(|text| {
                            transaction.bundle.name().ok().map(|name| {
                                (
                                    name,
                                    text.clone(),
                                    transaction.broadcast
                                        == dg_xch_wallet::storage::BroadcastStatus::Offered
                                        && !transaction.inputs_spent,
                                )
                            })
                        })
                    })
                    .collect(),
            );
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
    let mut farming_keys = HashMap::new();
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
                    update(&state, |state| { state.farmer_running = false; state.notice = error; state.notice_error = true; });
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
        let importing = matches!(&command, Command::Import { .. });
        update(&state, |state| {
            state.notice = "Working…".into();
            state.notice_error = false;
        });
        let result: Result<(), Error> = async {
            match command {
                Command::PlottingKeys { id, password } => {
                    update(&state, |state| state.plotting_keys = None);
                    let account = Account::load(&paths.accounts(), &id)?;
                    if account.network != settings.network { return Err(Error::other("account belongs to another network")); }
                    let keys = account_farming_keys(&account, password, &farming_keys).await?;
                    let farmer = hex::encode(keys.0.sk_to_pk().to_bytes());
                    let pool = hex::encode(keys.1.sk_to_pk().to_bytes());
                    update(&state, |state| { state.plotting_keys = Some((id, farmer, pool)); state.notice = "Public plotting keys loaded. No node connection or wallet sync is needed.".into(); });
                },
                Command::Import { name, mnemonic, password } => {
                    if state.lock().unwrap_or_else(|error| error.into_inner()).accounts.len() >= 64 { return Err(Error::other("account limit reached")); }
                    let account = tokio::task::spawn_blocking(move || Account::import(name, settings.network, &mnemonic, &password)).await.map_err(Error::other)??;
                    account.save_new(&paths.accounts())?;
                    update(&state, |state| { state.import_result = Some(Ok(account.id.clone())); state.accounts.push(AccountView { account, unlocked: false, snapshot: None, error: None, updated: None }); state.notice = "Wallet created and encrypted. Unlock it to track its balance.".into(); });
                },
                Command::Unlock { id, password } => {
                    if wallets.contains_key(&id) { return Err(Error::other("wallet is already unlocked")); }
                    let account = Account::load(&paths.accounts(), &id)?;
                    if account.network != settings.network { return Err(Error::other("account belongs to another network")); }
                    let genesis = settings.trusted_genesis()?;
                    let secret = tokio::task::spawn_blocking(move || account.unlock(&password)).await.map_err(Error::other)??;
                    let keys = (dg_xch_keys::master_sk_to_farmer_sk(&secret)?, dg_xch_keys::master_sk_to_pool_sk(&secret)?);
                    let session = WalletSession::new(secret, settings.client()?, Arc::new(settings.constants()?), genesis, paths.data.join("wallets").join(&id).join(format!("{}.sqlite", hex::encode(genesis)))).await?;
                    let (sender, receiver) = mpsc::channel(4);
                    update(&state, |state| if let Some(account) = state.accounts.iter_mut().find(|account| account.account.id == id) { account.unlocked = true; account.snapshot = Some(session.snapshot()); account.error = None; });
                    let worker = tokio::spawn(wallet_worker(id.clone(), session, receiver, state.clone(), settings.poll_seconds));
                    farming_keys.insert(id.clone(), keys);
                    wallets.insert(id, (sender, worker));
                    update(&state, |state| state.notice = "Wallet unlocked. Balance tracking is running in the background.".into());
                },
                Command::Lock(id) => {
                    farming_keys.remove(&id);
                    if let Some((_, handle)) = wallets.remove(&id) { handle.abort(); let _ = handle.await; }
                    update(&state, |state| if let Some(account) = state.accounts.iter_mut().find(|account| account.account.id == id) { account.unlocked = false; account.snapshot = None; account.error = None; });
                    update(&state, |state| state.notice = "Wallet locked. An already-running farmer keeps its separate farming keys until stopped.".into());
                },
                Command::Send { id, address, amount, fee } => {
                    let destination = dg_xch_keys::decode_puzzle_hash(&address)?;
                    if dg_xch_keys::encode_puzzle_hash(&destination, settings.constants()?.bech32_prefix)? != address.to_lowercase() { return Err(Error::other("destination belongs to another network")); }
                    wallets.get(&id).ok_or_else(|| Error::other("wallet is locked"))?.0.try_send(WalletCommand::Send { destination, amount, fee }).map_err(Error::other)?;
                    update(&state, |state| state.notice = "Transaction queued for revalidation and signing.".into());
                },
                Command::WatchCat { id, asset_id } => {
                    wallets.get(&id).ok_or_else(|| Error::other("wallet is locked"))?.0.try_send(WalletCommand::WatchCat(asset_id)).map_err(Error::other)?;
                },
                Command::Offer { id, genesis, action, fee } => {
                    if settings.trusted_genesis()? != genesis { return Err(Error::other("network changed; review the offer again")); }
                    wallets.get(&id).ok_or_else(|| Error::other("wallet is locked"))?.0.try_send(WalletCommand::Offer { action, fee }).map_err(Error::other)?;
                    update(&state, |state| state.notice = "Offer operation queued for validation and signing.".into());
                }
                Command::Asset { id, genesis, action, fee } => {
                    if genesis != settings.trusted_genesis()? { return Err(Error::other("network changed since you reviewed this asset transaction")); }
                    wallets.get(&id).ok_or_else(|| Error::other("wallet is locked"))?.0.try_send(WalletCommand::Asset { action, fee }).map_err(Error::other)?;
                    update(&state, |state| state.notice = "Asset transaction queued for fresh synchronization, validation and signing.".into());
                },
                Command::Settings(mut settings) => {
                    settings.normalize_network()?;
                    let mut appearance_only = configuration.borrow().clone();
                    appearance_only.theme = settings.theme;
                    if appearance_only == settings {
                        paths.save(&settings)?;
                        configuration.send(settings.clone()).map_err(Error::other)?;
                        update(&state, |state| { state.settings = Some(settings); state.notice = "Appearance saved.".into(); });
                        return Ok(());
                    }
                    if !wallets.is_empty() || farmer.is_some() { return Err(Error::other("lock wallets and stop farming before changing settings")); }
                    paths.save(&settings)?;
                    configuration.send(settings.clone()).map_err(Error::other)?;
                    update(&state, |state| state.settings = Some(settings));
                    update(&state, |state| { state.node = None; state.node_updated = None; state.plotting_keys = None; state.notice = "Settings saved.".into(); });
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
                    let keys = account_farming_keys(&account, password, &farming_keys).await?;
                    if settings.farmer_payout_address.trim().is_empty() { return Err(Error::other("Set a farmer payout address in Settings before starting the farmer.")); }
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
                        farmer_secret_key: keys.0.to_bytes().into(),
                        pool_secret_key: Some(keys.1.to_bytes().into()),
                        ..Default::default()
                    });
                    config.harvester_configs.plot_directories = settings.plot_directories.iter().map(|path| path.to_string_lossy().into_owned()).collect();
                    farmer = Some(FarmerService::start(config).await?);
                    update(&state, |state| { state.farmer_running = true; state.notice = "Account farmer started. Derived farming keys are not written to disk.".into(); });
                },
                Command::StopFarmer => {
                    if let Some(service) = farmer.take() { service.stop().await; }
                    update(&state, |state| { state.farmer_running = false; state.pool_settings.clear(); state.farmer_stats.clear(); state.notice = "Farmer stopped.".into(); });
                },
                Command::LoadPools => {
                    let service = farmer.as_ref().ok_or_else(|| Error::other("start your configured farmer first"))?;
                    let pools = service.pool_settings().await?;
                    update(&state, |state| {
                        state.notice = if pools.is_empty() { "No pool accounts configured. Select a farmer configuration containing your pool and owner/authentication keys.".into() } else { "Current settings loaded from the pools. No values were changed.".into() };
                        state.pool_settings = pools;
                    });
                },
                Command::UpdatePool { expected, payout, difficulty } => {
                    let service = farmer.as_ref().ok_or_else(|| Error::other("farmer is stopped"))?;
                    let current = service.update_pool_settings(&expected, &payout, difficulty).await?;
                    update(&state, |state| {
                        state.pool_settings.retain(|pool| pool.config.launcher_id != current.config.launcher_id);
                        state.pool_settings.push(current);
                        state.notice = "Pool changes accepted; current values reloaded from the pool.".into();
                    });
                },
                Command::Plot { request, output, limits, gpu } => {
                    if plot.as_ref().is_some_and(|job| !job.is_finished()) { return Err(Error::other("a plot job is already running")); }
                    if !output.is_dir() { return Err(Error::other("choose an existing output directory")); }
                    let started = time::OffsetDateTime::now_utc();
                    let staging = tempfile::Builder::new().prefix(".dgx-plot-").tempdir_in(&output)?;
                    let output = staging.path().join("plot.partial");
                    cancelled.store(false, Ordering::Release);
                    let cancelled = cancelled.clone();
                    let state = state.clone();
                    update(&state, |state| state.plot_job = Some("Selecting plotting backend".into()));
                    let (selection, job_label) = resolve_execution(&settings, gpu, &cancelled).await?;
                    if selection.as_ref().is_some_and(|selection| selection.backend == SelectedGpuBackend::Cuda) {
                        use std::ffi::OsString;
                        let mut args: Vec<OsString> = vec!["--output".into(), output.clone().into_os_string(), "--farmer-key".into(), hex::encode(request.farmer_public_key).into(),
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
                            let result = result.and_then(|_| {
                                let info = dg_xch_plotter::inspect(&output)?;
                                publish_plot(staging, &info, started).map(|path| format!("complete: {} ({} bytes)", path.display(), info.file_bytes))
                            });
                            update(&state, |state| state.plot_job = Some(format!("{job_label}: {}", result.unwrap_or_else(|error| format!("GPU job stopped: {error}")))));
                        }));
                        return Ok(());
                    }
                    update(&state, |state| state.plot_job = Some(format!("Running {job_label}")));
                    plot = Some(tokio::task::spawn_blocking(move || {
                        let result = if let Some(selection) = selection {
                            dg_xch_plotter::vulkan::create(&request, &output, limits, &cancelled, selection.device.ordinal)
                        } else {
                            dg_xch_plotter::create(&request, &output, limits, &cancelled)
                        };
                        update(&state, |state| state.plot_job = Some(match result {
                            Ok(info) => match publish_plot(staging, &info, started) {
                                Ok(path) => format!("{job_label}: complete: {} ({} bytes)", path.display(), info.file_bytes),
                                Err(error) => format!("{job_label}: stopped: {error}"),
                            },
                            Err(error) => format!("{job_label}: stopped: {error}"),
                        }));
                    }));
                },
                Command::CancelPlot => { cancelled.store(true, Ordering::Release); update(&state, |state| state.notice = "Cancellation requested. The job will stop at its next cancellation checkpoint.".into()); },
                Command::ProvePlot { path, challenge, testnet, gpu, memory_bytes, max_work } => {
                    if plot.as_ref().is_some_and(|job| !job.is_finished()) { return Err(Error::other("a plot job is already running")); }
                    let info = dg_xch_plotter::inspect(&path)?;
                    let proof_limits = PlotLimits { memory_bytes, max_work, max_entries: 256usize << (info.k / 2) };
                    cancelled.store(false, Ordering::Release);
                    let cancelled = cancelled.clone();
                    let state = state.clone();
                    update(&state, |state| state.plot_job = Some("Selecting development proof-check backend".into()));
                    let (selection, job_label) = resolve_execution(&settings, gpu, &cancelled).await?;
                    update(&state, |state| state.plot_job = Some(format!("{job_label}: reading challenge fragments and reconstructing candidate proofs")));
                    if selection.as_ref().is_some_and(|selection| selection.backend == SelectedGpuBackend::Cuda) {
                        let mut args = vec!["--prove-plot".into(), path.into_os_string(), "--challenge".into(), hex::encode(challenge).into(),
                            "--memory-mib".into(), (memory_bytes / 1024 / 1024).to_string().into(),
                            "--max-entries".into(), proof_limits.max_entries.to_string().into(), "--max-work".into(), max_work.to_string().into()];
                        if testnet { args.push("--testnet".into()); }
                        plot = Some(tokio::spawn(async move {
                            let result = run_cuda(&settings, args, &cancelled, Duration::from_secs(3600)).await;
                            update(&state, |state| state.plot_job = Some(format!("{job_label}: {}", result.unwrap_or_else(|error| format!("GPU proof check stopped: {error}")))));
                        }));
                    } else {
                        plot = Some(tokio::task::spawn_blocking(move || {
                            let result = (|| {
                                let mut harvester = dg_xch_farmer::harvesters::pos2::DiskHarvester::open(&path, testnet, proof_limits, &cancelled)?;
                                let limits = dg_xch_pos2::chainer::SearchLimits { max_hashes: 100_000_000, max_results: 1024 };
                                let proofs = if let Some(selection) = selection {
                                    let mut engine = dg_xch_pos2::vulkan::Hasher::for_params(harvester.params(), selection.device.ordinal)?;
                                    harvester.challenge_with_engine(challenge, limits, &cancelled, &mut engine)?
                                } else {
                                    harvester.challenge(challenge, limits, &cancelled)?
                                };
                                Ok::<_, Error>(format!("{} independently validated candidate proofs from stored fragments; not submitted to the network.", proofs.len()))
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
                        update(&state, |state| { state.notice = format!("Plot scan finished: {} entries. Directory errors, if any, appear in the inventory.", plots.len()); state.notice_error = false; state.inventory = plots; });
                    }));
                },
            }
            Ok(())
        }.await;
        if let Err(error) = result {
            update(&state, |state| {
                state.notice = error.to_string();
                state.notice_error = true;
                if importing {
                    state.import_result = Some(Err(state.notice.clone()));
                }
            });
        } else {
            update(&state, |state| {
                if state.notice == "Working…" {
                    state.notice =
                        "Request accepted. Check the job status below for progress.".into();
                }
            });
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

type FarmingKeys = (blst::min_pk::SecretKey, blst::min_pk::SecretKey);

async fn account_farming_keys(
    account: &Account,
    password: Zeroizing<String>,
    unlocked: &HashMap<String, FarmingKeys>,
) -> Result<FarmingKeys, Error> {
    if let Some(keys) = unlocked.get(&account.id) {
        return Ok(keys.clone());
    }
    let account = account.clone();
    tokio::task::spawn_blocking(move || {
        let secret = account.unlock(&password)?;
        Ok((
            dg_xch_keys::master_sk_to_farmer_sk(&secret)?,
            dg_xch_keys::master_sk_to_pool_sk(&secret)?,
        ))
    })
    .await
    .map_err(Error::other)?
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

fn publish_plot(
    staging: tempfile::TempDir,
    info: &dg_xch_plotter::PlotInfo,
    started: time::OffsetDateTime,
) -> Result<PathBuf, Error> {
    let directory = staging
        .path()
        .parent()
        .ok_or_else(|| Error::other("plot staging directory has no parent"))?;
    let filename = format!(
        "plot-k{}-{:04}-{:02}-{:02}-{:02}-{:02}-{}.plot",
        info.k,
        started.year(),
        u8::from(started.month()),
        started.day(),
        started.hour(),
        started.minute(),
        hex::encode(info.plot_id)
    );
    let destination = directory.join(filename);
    let mut temporary = tempfile::TempPath::try_from_path(staging.path().join("plot.partial"))?;
    temporary.disable_cleanup(true);
    if let Err(error) = temporary.persist_noclobber(&destination) {
        let recovery = staging.keep();
        return Err(Error::new(
            error.error.kind(),
            format!(
                "could not publish plot: {error}; completed plot retained at {}",
                recovery.join("plot.partial").display()
            ),
        ));
    }
    Ok(destination)
}

#[cfg(test)]
mod tests {
    #[test]
    fn generated_plot_name_preserves_identity_and_never_overwrites() {
        let root = tempfile::tempdir().unwrap();
        let info = dg_xch_plotter::PlotInfo {
            plot_id: [0xab; 32],
            k: 28,
            strength: 2,
            index: 0,
            meta_group: 0,
            portable: true,
            chunks: 0,
            file_bytes: 5,
        };
        let started = time::OffsetDateTime::UNIX_EPOCH;
        let staging = tempfile::tempdir_in(root.path()).unwrap();
        std::fs::write(staging.path().join("plot.partial"), b"first").unwrap();
        let output = super::publish_plot(staging, &info, started).unwrap();
        assert_eq!(
            output.file_name().unwrap().to_str().unwrap(),
            format!("plot-k28-1970-01-01-00-00-{}.plot", "ab".repeat(32))
        );
        assert_eq!(output.parent().unwrap(), root.path());
        let duplicate = tempfile::tempdir_in(root.path()).unwrap();
        let recovery = duplicate.path().join("plot.partial");
        std::fs::write(&recovery, b"second").unwrap();
        assert!(super::publish_plot(duplicate, &info, started).is_err());
        assert_eq!(std::fs::read(output).unwrap(), b"first");
        assert_eq!(std::fs::read(recovery).unwrap(), b"second");
    }

    use super::*;

    #[test]
    fn imports_report_failures_success_and_duplicates_without_overwriting() {
        let directory = tempfile::tempdir().unwrap();
        let paths = AppPaths {
            config: directory.path().join("config"),
            data: directory.path().join("data"),
        };
        let backend = Backend::for_smoke_test(paths.clone(), Settings::default()).unwrap();
        let mnemonic = bip39::Mnemonic::from_entropy(&[19; 32])
            .unwrap()
            .to_string();
        let wait = || {
            let deadline = Instant::now() + Duration::from_secs(30);
            loop {
                let snapshot = backend.snapshot();
                if let Some(result) = snapshot.import_result {
                    return result;
                }
                assert!(Instant::now() < deadline, "import timed out");
                std::thread::sleep(Duration::from_millis(20));
            }
        };
        for (password, succeeds) in [
            ("short", false),
            ("temporary-wallet-password", true),
            ("temporary-wallet-password", false),
        ] {
            backend.command(Command::Import {
                name: "Temporary wallet".into(),
                mnemonic: Zeroizing::new(mnemonic.clone()),
                password: Zeroizing::new(password.into()),
            });
            assert_eq!(wait().is_ok(), succeeds);
            assert_eq!(backend.snapshot().notice_error, !succeeds);
        }
        assert_eq!(backend.snapshot().accounts.len(), 1);
        let id = backend.snapshot().accounts[0].account.id.clone();
        assert!(
            Account::load(&paths.accounts(), &id)
                .unwrap()
                .unlock("temporary-wallet-password")
                .is_ok()
        );
    }

    #[tokio::test]
    async fn unlocked_farming_keys_do_not_need_another_password() {
        let phrase = bip39::Mnemonic::from_entropy(&[21; 32])
            .unwrap()
            .to_string();
        let account = Account::import(
            "Temporary wallet".into(),
            "mainnet".into(),
            &phrase,
            "temporary-wallet-password",
        )
        .unwrap();
        let mut unlocked = HashMap::new();
        let keys = account_farming_keys(
            &account,
            Zeroizing::new("temporary-wallet-password".into()),
            &unlocked,
        )
        .await
        .unwrap();
        let public_key = keys.0.sk_to_pk().to_bytes();
        unlocked.insert(account.id.clone(), keys);
        assert_eq!(
            account_farming_keys(&account, Zeroizing::new(String::new()), &unlocked)
                .await
                .unwrap()
                .0
                .sk_to_pk()
                .to_bytes(),
            public_key
        );
        unlocked.remove(&account.id);
        assert!(
            account_farming_keys(&account, Zeroizing::new(String::new()), &unlocked)
                .await
                .is_err()
        );
    }

    #[test]
    fn plotting_keys_load_without_a_node_or_wallet_session() {
        let directory = tempfile::tempdir().unwrap();
        let paths = AppPaths {
            config: directory.path().join("config"),
            data: directory.path().join("data"),
        };
        let mnemonic = bip39::Mnemonic::from_entropy(&[7; 32]).unwrap().to_string();
        let password = "local-test-password";
        let account = Account::import(
            "Plotting fixture".into(),
            "mainnet".into(),
            &mnemonic,
            password,
        )
        .unwrap();
        account.save_new(&paths.accounts()).unwrap();
        let secret = dg_xch_keys::key_from_mnemonic_str(&mnemonic).unwrap();
        let farmer = hex::encode(
            dg_xch_keys::master_sk_to_farmer_sk(&secret)
                .unwrap()
                .sk_to_pk()
                .to_bytes(),
        );
        let pool = hex::encode(
            dg_xch_keys::master_sk_to_pool_sk(&secret)
                .unwrap()
                .sk_to_pk()
                .to_bytes(),
        );
        let backend = Backend::for_smoke_test(paths.clone(), Settings::default()).unwrap();
        backend.command(Command::PlottingKeys {
            id: account.id.clone(),
            password: Zeroizing::new(password.into()),
        });
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let state = backend.snapshot();
            if let Some(keys) = state.plotting_keys {
                assert_eq!(keys, (account.id.clone(), farmer, pool));
                assert!(state.node.is_none());
                assert!(state.accounts.iter().all(|account| !account.unlocked));
                assert!(!paths.data.join("wallets").exists());
                break;
            }
            assert!(
                Instant::now() < deadline,
                "public-key derivation timed out: {}",
                state.notice
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

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
