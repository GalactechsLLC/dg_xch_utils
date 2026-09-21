use crate::backend::{Backend, Command, State};
use crate::config::{AppPaths, GpuBackend, Settings, Theme};
use crate::{format_mojos, parse_mojos};
use dg_xch_plotter::{PlotRequest, PoolBinding};
use dg_xch_pos2::plotting::PlotLimits;
use eframe::egui::{self, Color32, RichText, Ui};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use zeroize::{Zeroize, Zeroizing};

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Page {
    #[default]
    Overview,
    Wallets,
    Node,
    Farm,
    Plots,
    Settings,
}

struct Transfer {
    id: String,
    address: String,
    amount: u64,
    fee: u64,
}

pub struct Desktop {
    backend: Backend,
    paths: AppPaths,
    settings: Settings,
    settings_draft: Settings,
    page: Page,
    selected_account: Option<String>,
    account_name: String,
    mnemonic: String,
    password: String,
    backup_confirmed: bool,
    destination: String,
    amount: String,
    fee: String,
    transfer: Option<Transfer>,
    message: String,
    plot_output: String,
    farmer_key: String,
    pool_binding: String,
    portable: bool,
    plot_k: u8,
    plot_strength: u8,
    plot_index: u16,
    plot_meta: u8,
    plot_testnet: bool,
    plot_memory_mib: u64,
    plot_max_work: u64,
    plot_directory: String,
    plot_gpu: bool,
    proof_challenge: String,
    smoke_test: Option<(u8, Arc<AtomicBool>)>,
}

impl Desktop {
    pub fn new(
        context: &eframe::CreationContext<'_>,
        paths: AppPaths,
        settings: Settings,
        backend: Backend,
    ) -> Self {
        context.egui_ctx.set_pixels_per_point(1.15);
        Self {
            backend,
            paths,
            settings_draft: settings.clone(),
            settings,
            page: Page::Overview,
            selected_account: None,
            account_name: String::new(),
            mnemonic: String::new(),
            password: String::new(),
            backup_confirmed: false,
            destination: String::new(),
            amount: String::new(),
            fee: "0".into(),
            transfer: None,
            message: String::new(),
            plot_output: String::new(),
            farmer_key: String::new(),
            pool_binding: String::new(),
            portable: true,
            plot_k: 18,
            plot_strength: 2,
            plot_index: 0,
            plot_meta: 0,
            plot_testnet: false,
            plot_memory_mib: 512,
            plot_max_work: 10_000_000_000,
            plot_directory: String::new(),
            plot_gpu: false,
            proof_challenge: String::new(),
            smoke_test: None,
        }
    }

    pub fn with_smoke_test(mut self, completed: Arc<AtomicBool>) -> Self {
        self.smoke_test = Some((0, completed));
        self
    }

    fn overview(&mut self, ui: &mut Ui, state: &State) {
        heading(ui, "NETWORK DESK", "Your network. One native workspace.");
        ui.label("An independent view of your node, accounts and storage.");
        ui.add_space(24.0);
        ui.columns(3, |columns| {
            card(
                &mut columns[0],
                "CHAIN HEIGHT",
                &state
                    .node
                    .as_ref()
                    .and_then(|node| node.peak.as_ref())
                    .map(|peak| peak.height.to_string())
                    .unwrap_or_else(|| "Unavailable".into()),
            );
            card(
                &mut columns[1],
                "OPEN WALLETS",
                &state
                    .accounts
                    .iter()
                    .filter(|account| account.unlocked)
                    .count()
                    .to_string(),
            );
            card(
                &mut columns[2],
                "FARMER",
                if state.farmer_running {
                    "Running"
                } else {
                    "Stopped"
                },
            );
        });
        ui.add_space(24.0);
        ui.heading("Live accounts");
        for account in &state.accounts {
            ui.horizontal(|ui| {
                if ui.selectable_label(false, &account.account.name).clicked() {
                    self.selected_account = Some(account.account.id.clone());
                    self.page = Page::Wallets;
                }
                ui.label(&account.account.network);
                if let Some(snapshot) = &account.snapshot {
                    if !snapshot.synced {
                        ui.colored_label(Color32::YELLOW, "Cached wallet state. Payments stay disabled until a successful node sync.");
                    }
                    ui.monospace(format_mojos(snapshot.confirmed));
                    if account.error.is_some() {
                        ui.colored_label(Color32::YELLOW, "stale");
                    }
                } else {
                    ui.weak(if account.unlocked {
                        "Syncing"
                    } else {
                        "Locked"
                    });
                }
            });
        }
        if state.accounts.is_empty() {
            ui.label("Add a wallet in Accounts to begin. No private keys are sent to the node.");
        }
        ui.add_space(24.0);
        ui.heading("Connection");
        ui.label(format!(
            "{} · {}:{}",
            self.settings.network, self.settings.node_host, self.settings.node_port
        ));
        if let Some(error) = &state.node_error {
            ui.colored_label(Color32::LIGHT_RED, error);
        }
        ui.weak("This desktop trusts your configured full node. It is not a light-wallet consensus verifier.");
    }

    fn wallets(&mut self, ui: &mut Ui, state: &State) {
        heading(ui, "ACCOUNTS", "Separate keys. Simultaneous sessions.");
        ui.label("Every unlocked wallet synchronizes independently, including while another page is open.");
        ui.weak("Experimental wallet: use test funds. Your trusted full node must enable the coin-index feature.");
        ui.horizontal_wrapped(|ui| {
            for account in &state.accounts {
                if ui
                    .selectable_label(
                        self.selected_account.as_ref() == Some(&account.account.id),
                        &account.account.name,
                    )
                    .clicked()
                {
                    self.selected_account = Some(account.account.id.clone());
                    self.password.zeroize();
                }
            }
        });
        ui.separator();
        if let Some(account) = state
            .accounts
            .iter()
            .find(|account| Some(&account.account.id) == self.selected_account.as_ref())
        {
            ui.heading(&account.account.name);
            ui.weak(format!(
                "{} · {}",
                account.account.network, account.account.id
            ));
            if account.unlocked {
                if ui.button("Lock this wallet").clicked() {
                    self.backend
                        .command(Command::Lock(account.account.id.clone()));
                    self.transfer = None;
                }
                if let Some(error) = &account.error {
                    ui.colored_label(Color32::LIGHT_RED, error);
                }
                if let Some(snapshot) = &account.snapshot {
                    ui.columns(3, |columns| {
                        card(
                            &mut columns[0],
                            "CONFIRMED",
                            &format_mojos(snapshot.confirmed),
                        );
                        card(
                            &mut columns[1],
                            "SPENDABLE",
                            &format_mojos(snapshot.spendable),
                        );
                        card(
                            &mut columns[2],
                            "PENDING CHANGE",
                            &format_mojos(snapshot.pending_change),
                        );
                    });
                    if let Ok(constants) = self.settings.constants()
                        && let Ok(address) = dg_xch_keys::encode_puzzle_hash(
                            &snapshot.receive_puzzle_hash,
                            constants.bech32_prefix,
                        )
                    {
                        ui.label("Receive address");
                        ui.horizontal(|ui| {
                            ui.monospace(&address);
                            if ui.button("Copy").clicked() {
                                ui.ctx().copy_text(address);
                            }
                        });
                    }
                    ui.collapsing("Send a standard coin payment", |ui| {
                        field(ui, "Destination", &mut self.destination);
                        field(ui, "Amount (coins)", &mut self.amount);
                        field(ui, "Fee (coins)", &mut self.fee);
                        let ready = snapshot.synced && snapshot.height.is_some() && account.error.is_none() && account.updated.is_some_and(|updated| updated.elapsed().as_secs() < self.settings.poll_seconds * 3);
                        if ui.add_enabled(ready, egui::Button::new("Review payment")).clicked() {
                            match (parse_mojos(&self.amount), parse_mojos(&self.fee)) {
                                (Ok(amount), Ok(fee)) if amount > 0 => self.transfer = Some(Transfer { id: account.account.id.clone(), address: self.destination.trim().to_string(), amount, fee }),
                                _ => self.message = "Enter a nonzero amount and valid fee with at most 12 decimal places.".into(),
                            }
                        }
                    });
                    ui.collapsing("Coin history", |ui| {
                        egui::Grid::new("coin_history")
                            .striped(true)
                            .show(ui, |ui| {
                                ui.strong("Height");
                                ui.strong("Amount");
                                ui.strong("State");
                                ui.strong("Coin ID");
                                ui.end_row();
                                for coin in snapshot.coins.iter().rev().take(200) {
                                    ui.label(coin.confirmed_block_index.to_string());
                                    ui.monospace(format_mojos(u128::from(coin.coin.amount)));
                                    ui.label(if coin.spent { "Spent" } else { "Unspent" });
                                    ui.monospace(coin.coin.name().to_string());
                                    ui.end_row();
                                }
                            });
                    });
                    ui.collapsing("Submission journal", |ui| {
                        ui.label("Reservations survive restart and reorgs. Rejected or ambiguous submissions stay reserved; automatic release is not yet implemented.");
                        for transaction in &snapshot.pending { ui.monospace(transaction.to_string()); }
                    });
                } else {
                    ui.spinner();
                    ui.label("Discovering addresses and loading coins…");
                }
            } else {
                ui.add(
                    egui::TextEdit::singleline(&mut self.password)
                        .password(true)
                        .hint_text("Account password"),
                );
                if ui.button("Unlock and track balance").clicked() {
                    self.backend.command(Command::Unlock {
                        id: account.account.id.clone(),
                        password: Zeroizing::new(std::mem::take(&mut self.password)),
                    });
                }
            }
        }
        ui.add_space(18.0);
        ui.collapsing("Add an encrypted account", |ui| {
            field(ui, "Account name", &mut self.account_name);
            ui.label(format!("Network: {}", self.settings.network));
            ui.label("Mnemonic — never stored in settings or sent to the node");
            ui.add(
                egui::TextEdit::multiline(&mut self.mnemonic)
                    .desired_rows(3)
                    .desired_width(f32::INFINITY),
            );
            if ui.button("Generate a new 24-word wallet").clicked() {
                self.mnemonic.zeroize();
                match bip39::Mnemonic::generate(24) {
                    Ok(mnemonic) => {
                        self.mnemonic = mnemonic.to_string();
                        self.backup_confirmed = false;
                    }
                    Err(error) => self.message = error.to_string(),
                }
            }
            ui.add(
                egui::TextEdit::singleline(&mut self.password)
                    .password(true)
                    .hint_text("Encryption password (12+ bytes)"),
            );
            ui.checkbox(
                &mut self.backup_confirmed,
                "I have backed up this mnemonic outside this application",
            );
            if ui
                .add_enabled(
                    self.backup_confirmed,
                    egui::Button::new("Create encrypted account"),
                )
                .clicked()
            {
                self.backend.command(Command::Import {
                    name: std::mem::take(&mut self.account_name),
                    mnemonic: Zeroizing::new(std::mem::take(&mut self.mnemonic)),
                    password: Zeroizing::new(std::mem::take(&mut self.password)),
                });
                self.backup_confirmed = false;
            }
        });
        ui.weak("Standard wallets only in this build. CATs, NFTs, offers, hardware signing and wallet recovery tools are not yet exposed.");
    }

    fn node(&mut self, ui: &mut Ui, state: &State) {
        heading(ui, "NODE DETAILS", "Inside the chain engine.");
        if let Some(updated) = state.node_updated {
            ui.weak(format!(
                "Last successful sample: {} seconds ago",
                updated.elapsed().as_secs()
            ));
        }
        if let Some(error) = &state.node_error {
            ui.colored_label(Color32::LIGHT_RED, format!("Disconnected / stale: {error}"));
        }
        if let Some(node) = &state.node {
            ui.columns(3, |columns| {
                card(
                    &mut columns[0],
                    "SYNC",
                    if node.sync.synced && !node.sync.sync_mode {
                        "Synchronized"
                    } else {
                        "Catching up"
                    },
                );
                card(&mut columns[1], "DIFFICULTY", &node.difficulty.to_string());
                card(
                    &mut columns[2],
                    "MEMPOOL ITEMS",
                    &node.mempool_size.to_string(),
                );
            });
            let progress =
                node.sync.sync_progress_height as f32 / node.sync.sync_tip_height.max(1) as f32;
            ui.add(egui::ProgressBar::new(progress.min(1.0)).text(format!(
                "Sync {} / {}",
                node.sync.sync_progress_height, node.sync.sync_tip_height
            )));
            let capacity = node.mempool_cost as f64 / node.mempool_max_total_cost.max(1) as f64;
            ui.add(
                egui::ProgressBar::new(capacity.min(1.0) as f32).text(format!(
                    "Mempool cost {} / {}",
                    node.mempool_cost, node.mempool_max_total_cost
                )),
            );
            egui::Grid::new("node_details")
                .striped(true)
                .show(ui, |ui| {
                    detail(ui, "Node identity", node.node_id.to_string());
                    detail(ui, "Sub-slot iterations", node.sub_slot_iters.to_string());
                    detail(ui, "Estimated network bytes", node.space.to_string());
                    detail(ui, "Block CLVM cost limit", node.block_max_cost.to_string());
                    detail(
                        ui,
                        "Minimum fee / cost",
                        node.mempool_min_fees.cost_5000000.to_string(),
                    );
                    if let Some(peak) = &node.peak {
                        detail(ui, "Peak hash", peak.header_hash.to_string());
                        detail(ui, "Previous hash", peak.prev_hash.to_string());
                        detail(ui, "Weight", peak.weight.to_string());
                        detail(ui, "Total iterations", peak.total_iters.to_string());
                    }
                });
            ui.collapsing("Peak block and synchronization snapshot", |ui| {
                ui.monospace(serde_json::to_string_pretty(node).unwrap_or_default());
            });
        }
        ui.collapsing("Block counters", |ui| {
            ui.monospace(&state.node_metrics);
        });
        ui.collapsing(
            "Live internals: queues, caches, peer state and consensus",
            |ui| {
                ui.monospace(&state.node_details);
            },
        );
        ui.collapsing("Configured consensus and fork parameters", |ui| match self
            .settings
            .constants()
        {
            Ok(constants) => {
                ui.monospace(serde_json::to_string_pretty(&constants).unwrap_or_default());
            }
            Err(error) => {
                ui.label(error.to_string());
            }
        });
        ui.weak("RPC samples are diagnostic, not atomic snapshots. Live internals require a dg_xch node exposing get_node_details.");
    }

    fn farm(&mut self, ui: &mut Ui, state: &State) {
        heading(ui, "FARM", "Farmer and harvester, together.");
        ui.label("The imported FastFarmer engine runs inside this native process. No central farmer or legacy TUI is launched.");
        ui.collapsing("Farm using an encrypted account", |ui| {
            for account in &state.accounts {
                ui.selectable_value(&mut self.selected_account, Some(account.account.id.clone()), &account.account.name);
            }
            ui.add(egui::TextEdit::singleline(&mut self.password).password(true).hint_text("Account password"));
            ui.label("Uses farmer connection, payout and plot directories from Preferences. This mode derives keys in memory and currently supports OG/self-farming setup; imported pool configurations use the YAML mode below.");
            if ui.add_enabled(!state.farmer_running && self.selected_account.is_some(), egui::Button::new("Start account farmer")).clicked()
                && let Some(id) = &self.selected_account {
                self.backend.command(Command::StartAccountFarmer { id: id.clone(), password: Zeroizing::new(std::mem::take(&mut self.password)) });
            }
        });
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!state.farmer_running, egui::Button::new("Start farmer"))
                .clicked()
            {
                self.backend.command(Command::StartFarmer);
            }
            if ui
                .add_enabled(state.farmer_running, egui::Button::new("Stop farmer"))
                .clicked()
            {
                self.backend.command(Command::StopFarmer);
            }
        });
        ui.monospace(&state.farmer_stats);
        ui.label(format!(
            "Farmer configuration: {}",
            self.settings.farmer_config
        ));
        ui.label("Select your existing FastFarmer YAML in Settings. Legacy key files contain plaintext farming keys; protect them and do not share them.");
        ui.separator();
        ui.heading("Plot inventory");
        if ui.button("Refresh plot directories").clicked() {
            self.backend.command(Command::ScanPlots);
        }
        for (path, description) in &state.inventory {
            ui.group(|ui| {
                ui.label(path.display().to_string());
                ui.weak(description);
            });
        }
        ui.weak("PoS1 farming is integrated. PoS2 network farming is not enabled until disk proving, filter/activation rules and timely GPU solving are validated.");
    }

    fn plots(&mut self, ui: &mut Ui, state: &State) {
        heading(ui, "PLOT WORKSHOP", "Turn storage into proof.");
        ui.colored_label(
            Color32::from_rgb(219, 160, 96),
            "PoS2 RAM plotter · even k18–k32 · configure memory and work budgets for larger plots",
        );
        field(
            ui,
            "Output .plot file (must not exist)",
            &mut self.plot_output,
        );
        field(ui, "Farmer public key (48-byte hex)", &mut self.farmer_key);
        ui.checkbox(&mut self.portable, "Portable / pool contract plot");
        field(
            ui,
            if self.portable {
                "Pool contract puzzle hash (32-byte hex)"
            } else {
                "Pool public key (48-byte hex)"
            },
            &mut self.pool_binding,
        );
        ui.horizontal(|ui| {
            ui.label("k");
            ui.add(
                egui::DragValue::new(&mut self.plot_k)
                    .range(18..=32)
                    .speed(2),
            );
            ui.label("Strength");
            let maximum = self.plot_k
                - if self.plot_k < 28 {
                    2
                } else {
                    self.plot_k - 26
                }
                - 1;
            ui.add(egui::DragValue::new(&mut self.plot_strength).range(2..=maximum));
        });
        ui.horizontal(|ui| {
            ui.label("Index");
            ui.add(egui::DragValue::new(&mut self.plot_index));
            ui.label("Meta group");
            ui.add(egui::DragValue::new(&mut self.plot_meta));
        });
        ui.checkbox(&mut self.plot_testnet, "Use PoS2 testnet hash domain");
        ui.checkbox(
            &mut self.plot_gpu,
            "Use GPU compute backend selected in Preferences",
        );
        ui.horizontal(|ui| {
            ui.label("RAM budget (MiB)");
            ui.add(egui::DragValue::new(&mut self.plot_memory_mib).range(128..=524288));
        });
        ui.horizontal(|ui| {
            ui.label("Work budget (16-round AES evaluations)");
            ui.add(egui::DragValue::new(&mut self.plot_max_work).range(1..=u64::MAX));
        });
        ui.horizontal(|ui| {
            if ui.button("Create plot").clicked() {
                match self.plot_request() {
                    Ok(request) if !self.plot_output.trim().is_empty() => {
                        self.backend.command(Command::Plot {
                            request,
                            output: PathBuf::from(&self.plot_output),
                            limits: PlotLimits {
                                memory_bytes: self.plot_memory_mib * 1024 * 1024,
                                max_entries: usize::try_from(
                                    (1u64 << self.plot_k) + (1u64 << self.plot_k) / 8 + 65_536,
                                )
                                .unwrap_or(usize::MAX),
                                max_work: self.plot_max_work,
                            },
                            gpu: self.plot_gpu,
                        })
                    }
                    Ok(_) => self.message = "Choose an output filename".into(),
                    Err(error) => self.message = error,
                }
            }
            if ui.button("Cancel job").clicked() {
                self.backend.command(Command::CancelPlot);
            }
        });
        if let Some(job) = &state.plot_job {
            ui.label(job);
        }
        ui.collapsing("Check an existing PoS2 plot against a challenge", |ui| {
            ui.label("Uses the output path and resource budgets above. Reads challenge fragments and recovers independently validated proofs on CPU or GPU. Unread chunks are not verified and proofs are not submitted to the network.");
            field(ui, "Challenge (32-byte hex)", &mut self.proof_challenge);
            if ui.button("Run proof check").clicked() {
                use std::str::FromStr;
                match dg_xch_core::blockchain::sized_bytes::Bytes32::from_str(&self.proof_challenge) {
                    Ok(challenge) => self.backend.command(Command::ProvePlot { path: PathBuf::from(&self.plot_output), challenge, testnet: self.plot_testnet, gpu: self.plot_gpu, memory_bytes: self.plot_memory_mib * 1024 * 1024, max_work: self.plot_max_work }),
                    Err(error) => self.message = error.to_string(),
                }
            }
        });
        ui.weak("Auto prefers a usable native Rust CUDA helper, otherwise hardware Vulkan. This is a vendor policy, not a speed benchmark. The job reports its chosen device; failures do not switch backends. Disable GPU for the portable CPU path.");
    }

    fn plot_request(&self) -> Result<PlotRequest, String> {
        fn bytes<const SIZE: usize>(value: &str) -> Result<[u8; SIZE], String> {
            let mut result = [0; SIZE];
            hex::decode_to_slice(value.trim().trim_start_matches("0x"), &mut result)
                .map_err(|error| error.to_string())?;
            Ok(result)
        }
        Ok(PlotRequest {
            farmer_public_key: bytes(&self.farmer_key)?,
            pool: if self.portable {
                PoolBinding::Contract(bytes(&self.pool_binding)?)
            } else {
                PoolBinding::PublicKey(bytes(&self.pool_binding)?)
            },
            k: self.plot_k,
            strength: self.plot_strength,
            index: self.plot_index,
            meta_group: self.plot_meta,
            testnet: self.plot_testnet,
        })
    }

    fn settings(&mut self, ui: &mut Ui) {
        heading(ui, "PREFERENCES", "Local by default. Explicit by design.");
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.settings_draft.theme, Theme::Midnight, "Midnight");
            ui.selectable_value(&mut self.settings_draft.theme, Theme::Daylight, "Daylight");
        });
        field(ui, "Node hostname", &mut self.settings_draft.node_host);
        ui.horizontal(|ui| {
            ui.label("RPC port");
            ui.add(egui::DragValue::new(&mut self.settings_draft.node_port).range(1..=65535));
        });
        field(ui, "Network", &mut self.settings_draft.network);
        field(
            ui,
            "Custom chain definition JSON (optional)",
            &mut self.settings_draft.chain_definition_path,
        );
        field(
            ui,
            "Trusted genesis BLOCK HEADER hash",
            &mut self.settings_draft.genesis_header_hash,
        );
        ui.weak("Obtain the header hash at height 0 from a trusted source. This is not the genesis challenge. Wallets refuse mismatches.");
        field(
            ui,
            "Client certificate PEM",
            &mut self.settings_draft.certificate,
        );
        field(
            ui,
            "Client private key PEM",
            &mut self.settings_draft.private_key,
        );
        field(
            ui,
            "Trusted CA PEM",
            &mut self.settings_draft.certificate_authority,
        );
        ui.weak("TLS certificate and hostname verification are mandatory. No insecure bypass.");
        field(
            ui,
            "FastFarmer YAML",
            &mut self.settings_draft.farmer_config,
        );
        field(
            ui,
            "Farmer full-node WebSocket hostname",
            &mut self.settings_draft.farmer_ws_host,
        );
        ui.horizontal(|ui| {
            ui.label("Farmer WebSocket port");
            ui.add(egui::DragValue::new(&mut self.settings_draft.farmer_ws_port).range(1..=65535));
        });
        field(
            ui,
            "Farmer SSL root directory",
            &mut self.settings_draft.farmer_ssl_root,
        );
        field(
            ui,
            "Farmer payout address",
            &mut self.settings_draft.farmer_payout_address,
        );
        ui.horizontal(|ui| {
            ui.label("Compute backend");
            ui.selectable_value(
                &mut self.settings_draft.gpu_backend,
                GpuBackend::Auto,
                "Auto",
            );
            ui.selectable_value(
                &mut self.settings_draft.gpu_backend,
                GpuBackend::Cuda,
                "NVIDIA CUDA",
            );
            ui.selectable_value(
                &mut self.settings_draft.gpu_backend,
                GpuBackend::Vulkan,
                "Vulkan",
            );
        });
        ui.horizontal(|ui| {
            ui.label("Vulkan adapter ordinal (explicit Vulkan only)");
            ui.add(egui::DragValue::new(&mut self.settings_draft.vulkan_device).range(0..=31));
        });
        ui.weak("Auto prefers the configured CUDA device after a driver probe; otherwise it prefers non-NVIDIA Vulkan hardware. CUDA and Vulkan ordinals are independent. Select an explicit backend to pin a GPU. Vulkan uses WGSL hashing with Rust host matching; CUDA retains Rust GPU kernels. Neither is a production-size disk farmer.");
        field(
            ui,
            "Trusted CUDA executable (absolute path)",
            &mut self.settings_draft.cuda_executable,
        );
        ui.horizontal(|ui| {
            ui.label("CUDA device ordinal");
            ui.add(egui::DragValue::new(&mut self.settings_draft.cuda_device).range(0..=31));
        });
        ui.horizontal(|ui| {
            ui.label("Background poll seconds");
            ui.add(egui::DragValue::new(&mut self.settings_draft.poll_seconds).range(5..=300));
        });
        ui.heading("Plot directories");
        let mut remove = None;
        for (index, path) in self.settings_draft.plot_directories.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.label(path.display().to_string());
                if ui.small_button("Remove").clicked() {
                    remove = Some(index);
                }
            });
        }
        if let Some(index) = remove {
            self.settings_draft.plot_directories.remove(index);
        }
        ui.horizontal(|ui| {
            ui.text_edit_singleline(&mut self.plot_directory);
            if ui.button("Add directory").clicked() && !self.plot_directory.trim().is_empty() {
                self.settings_draft
                    .plot_directories
                    .push(PathBuf::from(std::mem::take(&mut self.plot_directory)));
            }
        });
        if ui
            .button("Save and reconnect (wallets must be locked)")
            .clicked()
        {
            self.backend
                .command(Command::Settings(self.settings_draft.clone()));
        }
        ui.weak(format!("Settings: {}", self.paths.config.display()));
        ui.weak(format!(
            "Encrypted accounts and SQLite wallets: {}",
            self.paths.data.display()
        ));
    }
}

impl eframe::App for Desktop {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let context = ui.ctx().clone();
        if let Some((frame, completed)) = &mut self.smoke_test {
            if *frame >= 12 {
                completed.store(true, Ordering::Release);
                context.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }
            self.page = match *frame % 6 {
                0 => Page::Overview,
                1 => Page::Wallets,
                2 => Page::Node,
                3 => Page::Farm,
                4 => Page::Plots,
                _ => Page::Settings,
            };
            self.settings.theme = if *frame < 6 {
                Theme::Midnight
            } else {
                Theme::Daylight
            };
            *frame += 1;
            context.request_repaint();
        }
        context.request_repaint_after(Duration::from_millis(500));
        let mut visuals = if self.settings.theme == Theme::Midnight {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        };
        visuals.selection.bg_fill = Color32::from_rgb(132, 89, 52);
        if self.settings.theme == Theme::Midnight {
            visuals.panel_fill = Color32::from_rgb(18, 25, 35);
            visuals.window_fill = Color32::from_rgb(24, 32, 44);
        }
        context.set_visuals(visuals);
        let state = self.backend.snapshot();
        if let Some(settings) = &state.settings
            && self.smoke_test.is_none()
        {
            self.settings = settings.clone();
        }
        egui::Panel::left("navigation")
            .resizable(false)
            .default_size(205.0)
            .show(ui, |ui| {
                ui.add_space(24.0);
                ui.label(
                    RichText::new("GALACTECHS")
                        .size(23.0)
                        .strong()
                        .color(Color32::from_rgb(216, 167, 112)),
                );
                ui.weak("NETWORK DESK");
                ui.add_space(28.0);
                for (page, label) in [
                    (Page::Overview, "Overview"),
                    (Page::Wallets, "Accounts"),
                    (Page::Node, "Node details"),
                    (Page::Farm, "Farm"),
                    (Page::Plots, "Plot workshop"),
                    (Page::Settings, "Preferences"),
                ] {
                    ui.add_space(8.0);
                    if ui
                        .selectable_label(self.page == page, RichText::new(label).size(17.0))
                        .clicked()
                    {
                        self.page = page;
                    }
                }
                ui.add_space(30.0);
                ui.weak(format!("{} · {}", self.settings.network, crate::version()));
                ui.weak("Native Rust / wgpu");
            });
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(if state.node_error.is_some() {
                    "NODE OFFLINE / STALE"
                } else if state.node.is_some() {
                    "NODE CONNECTED"
                } else {
                    "CONNECTING"
                });
                ui.separator();
                ui.label(&state.notice);
                if !self.message.is_empty() {
                    ui.colored_label(Color32::LIGHT_RED, &self.message);
                    if ui.small_button("Dismiss").clicked() {
                        self.message.clear();
                    }
                }
            });
        });
        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.add_space(20.0);
                match self.page {
                    Page::Overview => self.overview(ui, &state),
                    Page::Wallets => self.wallets(ui, &state),
                    Page::Node => self.node(ui, &state),
                    Page::Farm => self.farm(ui, &state),
                    Page::Plots => self.plots(ui, &state),
                    Page::Settings => self.settings(ui),
                }
            });
        });
        if let Some(transfer) = &self.transfer {
            let mut submit = false;
            let mut cancel = false;
            egui::Window::new("Confirm signed payment")
                .collapsible(false)
                .resizable(false)
                .show(&context, |ui| {
                    ui.label(format!("Wallet: {}", transfer.id));
                    ui.label(format!("Network: {}", self.settings.network));
                    ui.label("Recipient");
                    ui.monospace(&transfer.address);
                    ui.label(format!(
                        "Amount: {}",
                        format_mojos(u128::from(transfer.amount))
                    ));
                    ui.label(format!("Fee: {}", format_mojos(u128::from(transfer.fee))));
                    ui.colored_label(
                        Color32::YELLOW,
                        "Broadcasting a confirmed transaction cannot be undone.",
                    );
                    ui.horizontal(|ui| {
                        submit = ui.button("Sign and broadcast").clicked();
                        cancel = ui.button("Cancel").clicked();
                    });
                });
            if submit {
                if let Some(transfer) = self.transfer.take() {
                    self.backend.command(Command::Send {
                        id: transfer.id,
                        address: transfer.address,
                        amount: transfer.amount,
                        fee: transfer.fee,
                    });
                }
            } else if cancel {
                self.transfer = None;
            }
        }
    }
}

impl Drop for Desktop {
    fn drop(&mut self) {
        self.mnemonic.zeroize();
        self.password.zeroize();
    }
}

fn heading(ui: &mut Ui, eyebrow: &str, title: &str) {
    ui.label(
        RichText::new(eyebrow)
            .small()
            .color(Color32::from_rgb(216, 167, 112)),
    );
    ui.heading(RichText::new(title).size(29.0));
    ui.add_space(16.0);
}

fn field(ui: &mut Ui, label: &str, value: &mut String) {
    ui.label(label);
    ui.add(egui::TextEdit::singleline(value).desired_width(f32::INFINITY));
}

fn card(ui: &mut Ui, title: &str, value: &str) {
    ui.group(|ui| {
        ui.set_min_height(76.0);
        ui.weak(title);
        ui.label(RichText::new(value).size(23.0).strong());
    });
}

fn detail(ui: &mut Ui, label: &str, value: String) {
    ui.label(label);
    ui.monospace(value);
    ui.end_row();
}
