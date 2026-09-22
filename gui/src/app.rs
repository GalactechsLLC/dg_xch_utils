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
    proof_file: String,
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
        crate::theme::install_fonts(&context.egui_ctx);
        let plot_output = settings
            .plot_directories
            .first()
            .map(|path| path.display().to_string())
            .unwrap_or_default();
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
            plot_output,
            proof_file: String::new(),
            farmer_key: String::new(),
            pool_binding: String::new(),
            portable: true,
            plot_k: 28,
            plot_strength: 2,
            plot_index: 0,
            plot_meta: 0,
            plot_testnet: false,
            plot_memory_mib: 12288,
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
        heading(ui, "Overview", "Your node, wallets, and farm at a glance.");
        ui.weak(format!(
            "{} · Local keys · Native desktop",
            self.settings.network
        ));
        ui.add_space(24.0);
        ui.columns(3, |columns| {
            card(
                &mut columns[0],
                "Block height",
                &state
                    .node
                    .as_ref()
                    .and_then(|node| node.peak.as_ref())
                    .map(|peak| peak.height.to_string())
                    .unwrap_or_else(|| "Unavailable".into()),
            );
            card(
                &mut columns[1],
                "Unlocked wallets",
                &state
                    .accounts
                    .iter()
                    .filter(|account| account.unlocked)
                    .count()
                    .to_string(),
            );
            card(
                &mut columns[2],
                "Farmer",
                if state.farmer_running {
                    "Running"
                } else {
                    "Stopped"
                },
            );
        });
        ui.add_space(24.0);
        ui.heading("Your wallets");
        for account in &state.accounts {
            ui.horizontal(|ui| {
                if ui.selectable_label(false, &account.account.name).clicked() {
                    self.selected_account = Some(account.account.id.clone());
                    self.page = Page::Wallets;
                }
                ui.label(&account.account.network);
                if let Some(snapshot) = &account.snapshot {
                    if !snapshot.synced {
                        ui.colored_label(ui.visuals().warn_fg_color, "Cached wallet state. Payments stay disabled until a successful node sync.");
                    }
                    ui.monospace(format_mojos(snapshot.confirmed));
                    if account.error.is_some() {
                        ui.colored_label(ui.visuals().warn_fg_color, "stale");
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
            ui.label("No wallets yet. Create one or import an existing recovery phrase.");
            if primary_button(ui, "Add your first wallet").clicked() {
                self.page = Page::Wallets;
            }
        }
        ui.add_space(24.0);
        ui.heading("Connection");
        ui.label(format!(
            "{} · {}:{}",
            self.settings.network, self.settings.node_host, self.settings.node_port
        ));
        if state.node.is_some() && state.node_error.is_none() {
            ui.colored_label(crate::theme::GREEN, "Connected to your node");
            if ui.button("View sync progress").clicked() {
                self.page = Page::Node;
            }
        }
        if let Some(error) = &state.node_error {
            ui.colored_label(ui.visuals().warn_fg_color, "Waiting for your node");
            ui.weak("Start dgx full-node, or configure an existing node in Settings.");
            if ui
                .button("Open node details")
                .on_hover_text(error)
                .clicked()
            {
                self.page = Page::Node;
            }
        }
        ui.weak("This desktop trusts your configured full node. It is not a light-wallet consensus verifier.");
    }

    fn wallets(&mut self, ui: &mut Ui, state: &State) {
        heading(
            ui,
            "Wallets",
            "Manage your accounts and keep track of your balances.",
        );
        notice(
            ui,
            "Use test funds",
            "Wallet functionality is experimental. Unlocked accounts update in the background; sending requires a synchronized, trusted node.",
        );
        if !state.accounts.is_empty() {
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
        }
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
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
                if let Some(snapshot) = &account.snapshot {
                    ui.columns(3, |columns| {
                        card(
                            &mut columns[0],
                            "Confirmed",
                            &format_mojos(snapshot.confirmed),
                        );
                        card(
                            &mut columns[1],
                            "Spendable",
                            &format_mojos(snapshot.spendable),
                        );
                        card(
                            &mut columns[2],
                            "Pending change",
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
                    section(ui, "Send a standard coin payment", |ui| {
                        field(ui, "Destination", &mut self.destination);
                        field(ui, "Amount (coins)", &mut self.amount);
                        field(ui, "Fee (coins)", &mut self.fee);
                        let ready = snapshot.synced
                            && snapshot.height.is_some()
                            && account.error.is_none()
                            && account.updated.is_some_and(|updated| {
                                updated.elapsed().as_secs() < self.settings.poll_seconds * 3
                            });
                        if ui
                            .add_enabled(ready, egui::Button::new("Review payment"))
                            .clicked()
                        {
                            match (parse_mojos(&self.amount), parse_mojos(&self.fee)) {
                                (Ok(amount), Ok(fee)) if amount > 0 => self.transfer = Some(Transfer { id: account.account.id.clone(), address: self.destination.trim().to_string(), amount, fee }),
                                _ => self.message = "Enter a nonzero amount and valid fee with at most 12 decimal places.".into(),
                            }
                        }
                    });
                    section(ui, "Coin history", |ui| {
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
                    section(ui, "Submission journal", |ui| {
                        ui.label("Reservations survive restart and reorgs. Rejected or ambiguous submissions stay reserved; automatic release is not yet implemented.");
                        for transaction in &snapshot.pending {
                            ui.monospace(transaction.to_string());
                        }
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
        ui.add_space(6.0);
        section(ui, "Add a wallet", |ui| {
            field(ui, "Account name", &mut self.account_name);
            ui.label(format!("Network: {}", self.settings.network));
            ui.label("Recovery phrase");
            ui.weak("Paste an existing phrase, or generate a new one. Never share it.");
            ui.add(
                egui::TextEdit::multiline(&mut self.mnemonic)
                    .desired_rows(3)
                    .desired_width(f32::INFINITY),
            );
            if ui.button("Generate recovery phrase").clicked() {
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
                    .hint_text("Choose an encryption password")
                    .desired_width(f32::INFINITY)
                    .margin(egui::vec2(12.0, 10.0)),
            );
            ui.weak("Use a strong password of at least 12 bytes. Recovery phrases are never sent to the node.");
            ui.checkbox(
                &mut self.backup_confirmed,
                "I have backed up this mnemonic outside this application",
            );
            if ui
                .add_enabled(
                    self.backup_confirmed,
                    egui::Button::new(RichText::new("Create wallet").color(Color32::WHITE))
                        .fill(crate::theme::GREEN),
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
        heading(
            ui,
            "Node",
            "Synchronization, connected peers, and live node diagnostics.",
        );
        if let Some(updated) = state.node_updated {
            ui.weak(format!(
                "Last successful sample: {} seconds ago",
                updated.elapsed().as_secs()
            ));
        }
        if let Some(error) = &state.node_error {
            ui.colored_label(
                ui.visuals().error_fg_color,
                "Node disconnected or data is stale.",
            );
            ui.label("Start dgx full-node in a terminal, then check the connection and TLS paths in Settings.");
            ui.label("Sync progress appears here after a successful connection. You can create accounts and plots meanwhile.");
            section(ui, "Connection error details", |ui| {
                ui.label(error);
            });
        }
        if let Some(node) = &state.node {
            ui.columns(3, |columns| {
                card(
                    &mut columns[0],
                    "Sync status",
                    if node.sync.synced && !node.sync.sync_mode {
                        "Synchronized"
                    } else {
                        if node.sync.sync_tip_height == 0 {
                            "Awaiting peers"
                        } else {
                            "Catching up"
                        }
                    },
                );
                card(&mut columns[1], "Difficulty", &node.difficulty.to_string());
                card(
                    &mut columns[2],
                    "Pending transactions",
                    &node.mempool_size.to_string(),
                );
            });
            section(ui, "Network activity", |ui| {
                let progress =
                    node.sync.sync_progress_height as f32 / node.sync.sync_tip_height.max(1) as f32;
                if node.sync.sync_tip_height == 0 {
                    ui.weak(
                        "Waiting for a chain tip. This node has not established sync progress yet.",
                    );
                } else {
                    ui.add(egui::ProgressBar::new(progress.min(1.0)).text(format!(
                        "Sync {} / {}",
                        node.sync.sync_progress_height, node.sync.sync_tip_height
                    )));
                }
                let capacity = node.mempool_cost as f64 / node.mempool_max_total_cost.max(1) as f64;
                ui.add(
                    egui::ProgressBar::new(capacity.min(1.0) as f32).text(format!(
                        "Mempool cost {} / {}",
                        node.mempool_cost, node.mempool_max_total_cost
                    )),
                );
            });
            section(ui, "Node details", |ui| {
                egui::Grid::new("node_details")
                    .min_col_width(180.0)
                    .max_col_width((ui.available_width() - 204.0).max(140.0))
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
            });
            section(ui, "Peak block and synchronization snapshot", |ui| {
                data_view(ui, &serde_json::to_string_pretty(node).unwrap_or_default());
            });
        }
        section(ui, "Block counters", |ui| {
            data_view(ui, &state.node_metrics);
        });
        section(ui, "Live diagnostics", |ui| {
            data_view(ui, &state.node_details);
        });
        section(ui, "Network rules", |ui| match self.settings.constants() {
            Ok(constants) => {
                data_view(
                    ui,
                    &serde_json::to_string_pretty(&constants).unwrap_or_default(),
                );
            }
            Err(error) => {
                ui.label(error.to_string());
            }
        });
        ui.weak("RPC samples are diagnostic, not atomic snapshots. Live internals require a dg_xch node exposing get_node_details.");
    }

    fn farm(&mut self, ui: &mut Ui, state: &State) {
        heading(ui, "Farm", "Manage your farmer and the plots it uses.");
        ui.columns(2, |columns| {
            card(
                &mut columns[0],
                "Farmer status",
                if state.farmer_running {
                    "Running"
                } else {
                    "Stopped"
                },
            );
            card(
                &mut columns[1],
                "Discovered plots",
                &state.inventory.len().to_string(),
            );
        });
        ui.add_space(16.0);
        ui.weak("The farmer runs while this desktop is open. Closing it stops farming.");
        section(ui, "Account farmer", |ui| {
            if state.accounts.is_empty() {
                ui.weak("Add a wallet to start farming with encrypted keys.");
                if ui.button("Add a wallet").clicked() {
                    self.page = Page::Wallets;
                }
            }
            for account in &state.accounts {
                ui.selectable_value(
                    &mut self.selected_account,
                    Some(account.account.id.clone()),
                    &account.account.name,
                );
            }
            if !state.accounts.is_empty() {
                password_field(ui, &mut self.password);
            }
            ui.weak("Uses your saved payout address and plot directories. Account farming keeps keys encrypted on disk.");
            if ui
                .add_enabled(
                    !state.farmer_running && self.selected_account.is_some(),
                    egui::Button::new("Start account farmer"),
                )
                .on_disabled_hover_text(
                    "Choose a wallet first. Stop the active farmer before starting another.",
                )
                .clicked()
                && let Some(id) = &self.selected_account
            {
                self.backend.command(Command::StartAccountFarmer {
                    id: id.clone(),
                    password: Zeroizing::new(std::mem::take(&mut self.password)),
                });
            }
        });
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    !state.farmer_running && !self.settings.farmer_config.is_empty(),
                    egui::Button::new("Start from configuration"),
                )
                .on_disabled_hover_text(
                    "Select a farmer YAML file in Settings. Stop any active farmer first.",
                )
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
        if !self.settings.farmer_config.is_empty() {
            ui.label(format!("Configuration: {}", self.settings.farmer_config));
        }
        ui.weak("A configuration file is optional for account farming. For an existing setup, select a farmer YAML file in Settings and protect its keys.");
        ui.separator();
        ui.heading("Plot inventory");
        if ui.button("Refresh plot directories").clicked() {
            self.backend.command(Command::ScanPlots);
        }
        if state.inventory.is_empty() {
            ui.weak("No plots discovered yet. Create a plot or add existing plot directories in Settings.");
            ui.horizontal(|ui| {
                if primary_button(ui, "Create a plot").clicked() {
                    self.page = Page::Plots;
                }
                if ui.button("Manage directories").clicked() {
                    self.page = Page::Settings;
                }
            });
        }
        for (path, description) in &state.inventory {
            ui.group(|ui| {
                ui.label(path.display().to_string());
                ui.weak(description);
            });
        }
        ui.weak("PoS1 and PoS2 farming follow the selected network's activation rules. Loaded plots alone do not guarantee eligible proofs or rewards.");
    }

    fn plots(&mut self, ui: &mut Ui, state: &State) {
        heading(
            ui,
            "Plots",
            "Create plots while your node continues to synchronize.",
        );
        notice(
            ui,
            "Before you plot",
            "You can plot during node sync. Leave enough memory for the node. PoS2 farming depends on network activation; small test plots are not mainnet plots.",
        );
        if ui.available_width() >= 850.0 {
            ui.columns(2, |columns| {
                self.plotting_account(&mut columns[0], state);
                self.plot_file(&mut columns[1]);
            });
        } else {
            self.plotting_account(ui, state);
            self.plot_file(ui);
        }
        section(ui, "Size & performance", |ui| {
            ui.weak("PoS2 in-memory plotting supports even sizes k18–k32. Larger sizes need more memory.");
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
                "Use GPU compute backend selected in Settings",
            );
            ui.horizontal(|ui| {
                ui.label("RAM budget (MiB)");
                ui.add(egui::DragValue::new(&mut self.plot_memory_mib).range(128..=524288));
            });
            ui.horizontal(|ui| {
            ui.label("Work limit").on_hover_text("Maximum 16-round AES evaluations. This safety limit stops a job before it exceeds your configured compute budget.");
                ui.add(egui::DragValue::new(&mut self.plot_max_work).range(1..=u64::MAX));
            });
        });
        ui.horizontal_wrapped(|ui| {
            if primary_button(ui, "Create plot").on_hover_text("Create a new plot using the file, keys, and resource limits above. Existing files are never overwritten.").clicked() {
                match self.plot_request() {
                    Ok(request) if !self.plot_output.trim().is_empty() => {
                        self.backend.command(Command::Plot {
                            request,
                            output: PathBuf::from(self.plot_output.trim()),
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
                    Ok(_) => self.message = "Choose an output directory".into(),
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
        section(ui, "Verify an existing plot", |ui| {
            ui.label("Choose an existing plot. Uses the resource budgets above; unread chunks are not verified and proofs are not submitted to the network.");
            field(ui, "Existing plot file", &mut self.proof_file);
            field(ui, "Challenge (32-byte hex)", &mut self.proof_challenge);
            if ui.button("Run proof check").clicked() {
                use std::str::FromStr;
                match dg_xch_core::blockchain::sized_bytes::Bytes32::from_str(&self.proof_challenge)
                {
                    Ok(challenge) => self.backend.command(Command::ProvePlot {
                        path: PathBuf::from(self.proof_file.trim()),
                        challenge,
                        testnet: self.plot_testnet,
                        gpu: self.plot_gpu,
                        memory_bytes: self.plot_memory_mib * 1024 * 1024,
                        max_work: self.plot_max_work,
                    }),
                    Err(error) => self.message = error.to_string(),
                }
            }
        });
        ui.weak("Auto prefers a usable native Rust CUDA helper, otherwise hardware Vulkan. This is a vendor policy, not a speed benchmark. The job reports its chosen device; failures do not switch backends. Disable GPU for the portable CPU path.");
    }

    fn plotting_account(&mut self, ui: &mut Ui, state: &State) {
        section(ui, "Plotting account", |ui| {
            if state.accounts.is_empty() {
                ui.weak("Choose a wallet to fill in public plotting keys, or enter your own keys below.");
                if ui.button("Add a wallet").clicked() {
                    self.page = Page::Wallets;
                }
            }
            for account in &state.accounts {
                ui.selectable_value(
                    &mut self.selected_account,
                    Some(account.account.id.clone()),
                    &account.account.name,
                );
            }
            if !state.accounts.is_empty() {
                password_field(ui, &mut self.password);
            }
            if ui
                .add_enabled(
                    self.selected_account.is_some(),
                    egui::Button::new("Load public plotting keys"),
                )
                .on_disabled_hover_text("Add or select a wallet, then enter its password. No node connection is required.")
                .clicked()
                && let Some(id) = &self.selected_account
            {
                self.backend.command(Command::PlottingKeys {
                    id: id.clone(),
                    password: Zeroizing::new(std::mem::take(&mut self.password)),
                });
            }
            if let Some((id, farmer, pool)) = &state.plotting_keys
                && self.selected_account.as_ref() == Some(id)
            {
                if ui
                    .button("Use loaded keys for a self-farming plot")
                    .clicked()
                {
                    self.farmer_key.clone_from(farmer);
                    self.pool_binding.clone_from(pool);
                    self.portable = false;
                }
                ui.label("For a portable plot, keep your own pool contract hash instead of using a pool public key.");
            }
        });
    }

    fn plot_file(&mut self, ui: &mut Ui) {
        section(ui, "Plot file & ownership", |ui| {
            field(ui, "Output directory", &mut self.plot_output);
            ui.weak("Filenames are generated automatically.")
                .on_hover_text("plot-k<size>-YYYY-MM-DD-HH-MM-<plot ID>.plot (UTC)");
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
        });
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
        heading(ui, "Settings", "Connections, storage, and appearance.");
        ui.horizontal(|ui| {
            ui.label("Appearance");
            ui.selectable_value(
                &mut self.settings_draft.theme,
                Theme::Midnight,
                "Forest dark",
            );
            ui.selectable_value(
                &mut self.settings_draft.theme,
                Theme::Daylight,
                "Garden light",
            );
            if primary_button(ui, "Save settings").clicked() {
                self.backend
                    .command(Command::Settings(self.settings_draft.clone()));
            }
        });
        ui.weak("Lock your wallets and stop farming before changing connections. Theme changes preview immediately.");
        ui.add_space(18.0);
        if ui.available_width() >= 850.0 {
            ui.columns(2, |columns| {
                self.connection_settings(&mut columns[0]);
                self.farming_settings(&mut columns[1]);
            });
        } else {
            self.connection_settings(ui);
            self.farming_settings(ui);
        }
        section(ui, "Storage", |ui| {
            ui.label("Plot directories");
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

            ui.weak(format!("Configuration: {}", self.paths.config.display()));
            ui.weak(format!("Wallet data: {}", self.paths.data.display()));
        });
    }

    fn connection_settings(&mut self, ui: &mut Ui) {
        section(ui, "Node connection", |ui| {
            field(ui, "Node hostname", &mut self.settings_draft.node_host);
            ui.horizontal(|ui| {
                ui.label("RPC port");
                ui.add(egui::DragValue::new(&mut self.settings_draft.node_port).range(1..=65535));
            });
            let mut selected = if self.settings_draft.is_custom_chain() {
                "custom".to_owned()
            } else {
                self.settings_draft.network.clone()
            };
            let previous = selected.clone();
            ui.label("Network");
            egui::ComboBox::from_id_salt("chain_network")
                .selected_text(match selected.as_str() {
                    "mainnet" => "Chia mainnet",
                    "testnet11" => "Chia testnet11",
                    _ => "Custom",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut selected, "mainnet".into(), "Chia mainnet");
                    ui.selectable_value(&mut selected, "testnet11".into(), "Chia testnet11");
                    ui.selectable_value(&mut selected, "custom".into(), "Custom");
                });
            if selected != previous {
                self.settings_draft.custom_chain = selected == "custom";
                self.settings_draft.chain_definition_path.clear();
                self.settings_draft.genesis_header_hash.clear();
                if selected != "custom" {
                    self.settings_draft.network = selected;
                    if let Err(error) = self.settings_draft.normalize_network() {
                        self.message = error.to_string();
                    }
                }
            }
            if self.settings_draft.is_custom_chain() {
                field(
                    ui,
                    "Chain definition JSON file",
                    &mut self.settings_draft.chain_definition_path,
                );
                field(
                    ui,
                    "Trusted genesis block header hash",
                    &mut self.settings_draft.genesis_header_hash,
                );
                ui.weak("Obtain the header hash at height 0 from a trusted source. This is not the genesis challenge. Wallets refuse mismatches.");
                ui.weak("The network ID is read from the JSON when you save.");
            } else {
                if let Ok(hash) = self.settings_draft.trusted_genesis() {
                    ui.weak("Trusted genesis: built in (read-only)")
                        .on_hover_text(format!("Genesis block header hash: {}", hex::encode(hash)));
                }
            }
        });
        section(ui, "Secure connection", |ui| {
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
        });
    }

    fn farming_settings(&mut self, ui: &mut Ui) {
        section(ui, "Farmer connection", |ui| {
            field(
                ui,
                "Farmer configuration file (optional)",
                &mut self.settings_draft.farmer_config,
            );
            field(
                ui,
                "Farmer full-node WebSocket hostname",
                &mut self.settings_draft.farmer_ws_host,
            );
            ui.horizontal(|ui| {
                ui.label("Farmer WebSocket port");
                ui.add(
                    egui::DragValue::new(&mut self.settings_draft.farmer_ws_port).range(1..=65535),
                );
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
        });
        section(ui, "Compute & performance", |ui| {
            ui.horizontal_wrapped(|ui| {
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
                ui.label("Vulkan device").on_hover_text(
                    "Zero-based adapter number. Applies only when Vulkan is explicitly selected.",
                );
                ui.add(egui::DragValue::new(&mut self.settings_draft.vulkan_device).range(0..=31));
            });
            ui.weak("Auto tries CUDA first, then hardware Vulkan. CUDA and Vulkan device numbers are independent.");
            field(
                ui,
                "CUDA helper path (optional)",
                &mut self.settings_draft.cuda_executable,
            );
            ui.weak(
                "Use an absolute path to a trusted CUDA helper. Leave empty for Vulkan or CPU.",
            );
            ui.horizontal(|ui| {
                ui.label("CUDA device");
                ui.add(egui::DragValue::new(&mut self.settings_draft.cuda_device).range(0..=31));
            });
            ui.horizontal(|ui| {
                ui.label("Refresh interval (seconds)");
                ui.add(egui::DragValue::new(&mut self.settings_draft.poll_seconds).range(5..=300));
            });
        });
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
        let state = self.backend.snapshot();
        if let Some(settings) = &state.settings
            && self.smoke_test.is_none()
        {
            self.settings = settings.clone();
        }
        crate::theme::apply(
            &context,
            if self.page == Page::Settings && self.smoke_test.is_none() {
                self.settings_draft.theme
            } else {
                self.settings.theme
            },
        );
        egui::Panel::left("navigation")
            .resizable(false)
            .exact_size(220.0)
            .frame(
                egui::Frame::new()
                    .fill(context.global_style().visuals.window_fill)
                    .inner_margin(18),
            )
            .show(ui, |ui| {
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    let (bounds, _) =
                        ui.allocate_exact_size(egui::vec2(28.0, 30.0), egui::Sense::hover());
                    let center = bounds.center();
                    ui.painter().line_segment(
                        [
                            center + egui::vec2(0.0, 11.0),
                            center - egui::vec2(0.0, 5.0),
                        ],
                        egui::Stroke::new(2.0, crate::theme::GREEN),
                    );
                    ui.painter().circle_filled(
                        center + egui::vec2(-6.0, -4.0),
                        6.0,
                        crate::theme::GREEN,
                    );
                    ui.painter().circle_filled(
                        center + egui::vec2(5.0, -8.0),
                        7.0,
                        crate::theme::GREEN,
                    );
                    ui.label(RichText::new("Druid Garden").size(19.0).strong());
                });
                ui.add_space(6.0);
                ui.weak("Your Chia workspace");
                ui.add_space(30.0);
                for (page, label) in [
                    (Page::Overview, "Overview"),
                    (Page::Wallets, "Wallets"),
                    (Page::Node, "Node"),
                    (Page::Farm, "Farm"),
                    (Page::Plots, "Plots"),
                    (Page::Settings, "Settings"),
                ] {
                    let selected = self.page == page;
                    let response = ui.add_sized(
                        [ui.available_width(), 44.0],
                        egui::Button::new(RichText::new(label).size(15.0)).selected(selected),
                    );
                    if response.clicked() {
                        self.page = page;
                    }
                    ui.add_space(3.0);
                }
                ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                    ui.weak(format!("dgx {}", crate::version()));
                    ui.label(&self.settings.network);
                    ui.separator();
                });
            });
        egui::Panel::bottom("status")
            .frame(
                egui::Frame::new()
                    .fill(context.global_style().visuals.window_fill)
                    .inner_margin(egui::Margin::symmetric(24, 12)),
            )
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    let (label, color) = if state.node_error.is_some() {
                        ("Node offline", ui.visuals().warn_fg_color)
                    } else if state.node.is_some() {
                        ("Node connected", crate::theme::GREEN)
                    } else {
                        ("Connecting", ui.visuals().weak_text_color())
                    };
                    ui.colored_label(color, label);
                    if !state.notice.is_empty() {
                        ui.separator();
                        ui.label(&state.notice);
                    }
                    if !self.message.is_empty() {
                        ui.colored_label(ui.visuals().error_fg_color, &self.message);
                        if ui.small_button("Dismiss").clicked() {
                            self.message.clear();
                        }
                    }
                });
            });
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(context.global_style().visuals.panel_fill)
                    .inner_margin(28),
            )
            .show(ui, |ui| {
                crate::theme::paint_background(ui);
                egui::ScrollArea::vertical()
                    .id_salt(self.page as u8)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.set_max_width(ui.available_width().min(1180.0));
                        match self.page {
                            Page::Overview => self.overview(ui, &state),
                            Page::Wallets => self.wallets(ui, &state),
                            Page::Node => self.node(ui, &state),
                            Page::Farm => self.farm(ui, &state),
                            Page::Plots => self.plots(ui, &state),
                            Page::Settings => self.settings(ui),
                        }
                        ui.add_space(24.0);
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
                        ui.visuals().warn_fg_color,
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

fn heading(ui: &mut Ui, title: &str, subtitle: &str) {
    ui.heading(RichText::new(title).size(30.0).strong());
    ui.weak(subtitle);
    ui.add_space(22.0);
}

fn primary_button(ui: &mut Ui, label: &str) -> egui::Response {
    ui.add(
        egui::Button::new(RichText::new(label).color(Color32::WHITE).strong())
            .fill(crate::theme::GREEN),
    )
}

fn field(ui: &mut Ui, label: &str, value: &mut String) {
    ui.label(RichText::new(label).size(13.0));
    ui.add(
        egui::TextEdit::singleline(value)
            .desired_width(f32::INFINITY)
            .margin(egui::vec2(12.0, 10.0)),
    );
    ui.add_space(3.0);
}

fn password_field(ui: &mut Ui, value: &mut String) {
    ui.label("Wallet password");
    ui.add(
        egui::TextEdit::singleline(value)
            .password(true)
            .hint_text("Enter your wallet password")
            .desired_width(f32::INFINITY)
            .margin(egui::vec2(12.0, 10.0)),
    );
}

fn notice(ui: &mut Ui, title: &str, message: &str) {
    egui::Frame::new()
        .fill(ui.visuals().faint_bg_color)
        .stroke(ui.visuals().window_stroke)
        .corner_radius(10)
        .inner_margin(14)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.strong(title);
            ui.label(message);
        });
    ui.add_space(16.0);
}

fn section<Response>(
    ui: &mut Ui,
    title: &str,
    content: impl FnOnce(&mut Ui) -> Response,
) -> Response {
    let response = ui
        .push_id(title, |ui| {
            egui::Frame::new()
                .fill(ui.visuals().window_fill)
                .stroke(ui.visuals().window_stroke)
                .corner_radius(12)
                .inner_margin(20)
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new(title).size(17.0).strong());
                    ui.add_space(12.0);
                    content(ui)
                })
                .inner
        })
        .inner;
    ui.add_space(16.0);
    response
}

fn card(ui: &mut Ui, title: &str, value: &str) {
    egui::Frame::new()
        .fill(ui.visuals().window_fill)
        .stroke(ui.visuals().window_stroke)
        .corner_radius(12)
        .inner_margin(20)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.set_min_height(78.0);
            ui.weak(title);
            ui.add_space(8.0);
            ui.label(RichText::new(value).size(27.0).strong());
        });
}

fn detail(ui: &mut Ui, label: &str, value: String) {
    ui.weak(label);
    ui.add(egui::Label::new(RichText::new(value).monospace()).wrap());
    ui.end_row();
}

fn data_view(ui: &mut Ui, data: &str) {
    if data.trim().is_empty() {
        ui.weak("Waiting for a node sample. No data is available yet.");
        return;
    }
    if ui.small_button("Copy data").clicked() {
        ui.ctx().copy_text(data.to_owned());
    }
    match serde_json::from_str::<serde_json::Value>(data) {
        Ok(value) => {
            let mut remaining = 512;
            egui::Grid::new("data")
                .striped(true)
                .min_col_width((ui.available_width() * 0.40).max(100.0))
                .max_col_width((ui.available_width() * 0.46).max(120.0))
                .spacing([24.0, 10.0])
                .show(ui, |ui| {
                    data_rows(ui, "", &value, &mut remaining);
                });
            if remaining == 0 {
                ui.weak("Showing the first 512 values. Copy data includes the complete sample.");
            }
        }
        Err(_) => {
            ui.monospace(data);
        }
    }
}

fn data_rows(ui: &mut Ui, path: &str, value: &serde_json::Value, remaining: &mut usize) {
    if *remaining == 0 {
        return;
    }
    match value {
        serde_json::Value::Object(fields) if !fields.is_empty() => {
            for (name, value) in fields {
                let name = name.replace('_', " ");
                let next = if path.is_empty() {
                    name
                } else {
                    format!("{path} / {name}")
                };
                data_rows(ui, &next, value, remaining);
                if *remaining == 0 {
                    break;
                }
            }
        }
        serde_json::Value::Array(values) if !values.is_empty() => {
            for (index, value) in values.iter().enumerate() {
                data_rows(ui, &format!("{path} [{index}]"), value, remaining);
                if *remaining == 0 {
                    break;
                }
            }
        }
        _ => {
            *remaining -= 1;
            ui.weak(path);
            ui.add(
                egui::Label::new(
                    RichText::new(match value {
                        serde_json::Value::String(text) => text.clone(),
                        serde_json::Value::Null => "Not available".into(),
                        _ => value.to_string(),
                    })
                    .monospace(),
                )
                .wrap(),
            );
            ui.end_row();
        }
    }
}
