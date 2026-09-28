use crate::backend::{Backend, Command, OfferAction, State};
use crate::config::{AppPaths, GpuBackend, Settings, Theme};
use crate::{format_mojos, parse_mojos};
use dg_xch_plotter::{PlotRequest, PoolBinding};
use dg_xch_pos2::plotting::PlotLimits;
use dg_xch_wallet::assets::{AssetAction, AssetKind, NftLaunch};
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
    Tools,
    Settings,
}

struct Transfer {
    id: String,
    address: String,
    amount: u64,
    fee: u64,
}

struct AssetConfirmation {
    id: String,
    genesis: dg_xch_core::blockchain::sized_bytes::Bytes32,
    action: AssetAction,
    fee: u64,
    description: String,
}

#[derive(Default)]
struct OfferForm {
    give_asset: String,
    give_amount: String,
    receive_asset: String,
    receive_amount: String,
    fee: String,
    imported: String,
}

struct OfferConfirmation {
    id: String,
    genesis: dg_xch_core::blockchain::sized_bytes::Bytes32,
    action: OfferAction,
    fee: u64,
    description: String,
}

fn offer_amount_label(amount: &dg_xch_wallet::offers::OfferAmount) -> String {
    match amount.asset {
        dg_xch_wallet::offers::OfferAsset::Xch => format!(
            "{} XCH ({} mojos)",
            format_mojos(u128::from(amount.amount)),
            amount.amount
        ),
        dg_xch_wallet::offers::OfferAsset::Cat2(asset) => {
            format!("{} base units of CAT2 {asset}", amount.amount)
        }
    }
}

#[derive(Default)]
struct AssetForm {
    selected_coin: Option<dg_xch_core::blockchain::sized_bytes::Bytes32>,
    show_spent: bool,
    supply: String,
    uri: String,
    hash: String,
    royalty: String,
    royalty_address: String,
    edition: String,
    editions: String,
    fee: String,
    watched_cat: String,
    destination: String,
    amount: String,
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
    create_password: String,
    farmer_password: String,
    plotting_password: String,
    import_pending: bool,
    import_error: String,
    show_wallet_form: bool,
    dismissed_notice: String,
    dismissed_revision: u64,
    backup_confirmed: bool,
    destination: String,
    amount: String,
    fee: String,
    transfer: Option<Transfer>,
    message: String,
    message_error: bool,
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
    tool_tab: u8,
    converter_input: String,
    converter_prefix: String,
    converter_result: Option<(String, String)>,
    pool_draft: Option<(dg_xch_farmer::pool_management::PoolSettings, String, String)>,
    pool_confirm: bool,
    asset_form: AssetForm,
    asset_confirmation: Option<AssetConfirmation>,
    offer_form: OfferForm,
    offer_confirmation: Option<OfferConfirmation>,
    wallet_tab: u8,
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
            create_password: String::new(),
            farmer_password: String::new(),
            plotting_password: String::new(),
            import_pending: false,
            import_error: String::new(),
            show_wallet_form: false,
            dismissed_notice: String::new(),
            dismissed_revision: 0,
            backup_confirmed: false,
            destination: String::new(),
            amount: String::new(),
            fee: "0".into(),
            transfer: None,
            message: String::new(),
            message_error: false,
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
            tool_tab: 0,
            offer_form: OfferForm {
                fee: "0".into(),
                ..OfferForm::default()
            },
            offer_confirmation: None,
            converter_input: String::new(),
            converter_prefix: "xch".into(),
            converter_result: None,
            pool_draft: None,
            pool_confirm: false,
            asset_form: AssetForm {
                fee: "0".into(),
                royalty: "0".into(),
                edition: "1".into(),
                editions: "1".into(),
                ..Default::default()
            },
            asset_confirmation: None,
            wallet_tab: 0,
            smoke_test: None,
        }
    }

    pub fn with_smoke_test(mut self, completed: Arc<AtomicBool>) -> Self {
        self.smoke_test = Some((0, completed));
        self
    }

    fn feedback(&mut self, message: impl Into<String>, error: bool) {
        self.message = message.into();
        self.message_error = error;
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
            ui.label("Choose a wallet");
            ui.horizontal_wrapped(|ui| {
                for account in &state.accounts {
                    if selection_button(
                        ui,
                        &format!(
                            "{} · {}",
                            account.account.name,
                            if account.unlocked {
                                "Unlocked"
                            } else {
                                "Locked"
                            }
                        ),
                        self.selected_account.as_ref() == Some(&account.account.id),
                    )
                    .clicked()
                    {
                        self.selected_account = Some(account.account.id.clone());
                        self.show_wallet_form = false;
                        self.password.zeroize();
                        self.farmer_password.zeroize();
                        self.plotting_password.zeroize();
                        self.transfer = None;
                    }
                }
                if ui.button("+ Add wallet").clicked() {
                    self.show_wallet_form = true;
                }
            });
            ui.separator();
        }
        if self.show_wallet_form || state.accounts.is_empty() {
            self.wallet_form(ui, state);
            return;
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
                ui.colored_label(
                    crate::theme::GREEN,
                    "Unlocked · tracking balance in the background",
                );
                if ui.button("Lock this wallet").clicked() {
                    self.backend
                        .command(Command::Lock(account.account.id.clone()));
                    self.transfer = None;
                }
                if let Some(error) = &account.error {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
                if let Some(snapshot) = &account.snapshot {
                    ui.horizontal_wrapped(|ui| {
                        for (index, name) in ["Chia", "Tokens", "NFTs", "DIDs"].iter().enumerate() {
                            if selection_button(ui, name, self.wallet_tab == index as u8).clicked()
                            {
                                self.wallet_tab = index as u8;
                            }
                        }
                    });
                    if self.wallet_tab != 0 {
                        self.wallet_assets(ui, &account.account.id, snapshot);
                        return;
                    }
                    ui.columns(3, |columns| {
                        card(
                            &mut columns[0],
                            "Confirmed (coins)",
                            &balance_text(snapshot.confirmed),
                        );
                        card(
                            &mut columns[1],
                            "Spendable (coins)",
                            &balance_text(snapshot.spendable),
                        );
                        card(
                            &mut columns[2],
                            "Pending change (coins)",
                            &balance_text(snapshot.pending_change),
                        );
                    });
                    if let Ok(constants) = self.settings.constants()
                        && let Ok(address) = dg_xch_keys::encode_puzzle_hash(
                            &snapshot.receive_puzzle_hash,
                            constants.bech32_prefix,
                        )
                    {
                        ui.label("Receive address");
                        ui.horizontal_wrapped(|ui| {
                            ui.monospace(&address);
                            if ui.button("Copy").clicked() {
                                ui.ctx().copy_text(address);
                                self.feedback("Receive address copied.", false);
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
                            .on_disabled_hover_text("Payments require a fresh, successful wallet sync with your trusted node.")
                            .clicked()
                        {
                            match (parse_mojos(&self.amount), parse_mojos(&self.fee)) {
                                (Ok(amount), Ok(fee)) if amount > 0 => self.transfer = Some(Transfer { id: account.account.id.clone(), address: self.destination.trim().to_string(), amount, fee }),
                                _ => self.feedback("Enter a nonzero amount and valid fee with at most 12 decimal places.", true),
                            }
                        }
                    });
                    if !snapshot.synced {
                        ui.weak("Sending is unavailable until this wallet has synchronized with your trusted node.");
                    }
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
                        if snapshot.coins.is_empty() {
                            ui.weak("No coins found yet.");
                        }
                    });
                    section(ui, "Submission journal", |ui| {
                        ui.label("Reservations survive restart and reorgs. Rejected or ambiguous submissions stay reserved; automatic release is not yet implemented.");
                        for transaction in &snapshot.pending {
                            ui.monospace(transaction.to_string());
                        }
                        if snapshot.pending.is_empty() {
                            ui.weak("No submitted payments.");
                        }
                    });
                } else {
                    ui.spinner();
                    ui.label("Discovering addresses and loading coins…");
                }
            } else {
                section(ui, "Unlock wallet", |ui| {
                    ui.weak("Unlock this account to show its receive address and track its balance. Other unlocked wallets keep updating.");
                    ui.push_id(&account.account.id, |ui| {
                        password_field(ui, &mut self.password)
                    });
                    if primary_button(ui, "Unlock and track balance").clicked() {
                        self.backend.command(Command::Unlock {
                            id: account.account.id.clone(),
                            password: Zeroizing::new(std::mem::take(&mut self.password)),
                        });
                    }
                });
            }
        }
        ui.weak("CAT1 is read-only. CAT2 and NFT1 transfers are available after synchronization. XCH/CAT2 offers are under Tools. Hardware signing and automatic recovery are not yet available.");
    }

    fn wallet_form(&mut self, ui: &mut Ui, state: &State) {
        ui.add_space(6.0);
        if self.show_wallet_form || state.accounts.is_empty() {
            section(ui, "Add a wallet", |ui| {
                ui.add_enabled_ui(!self.import_pending, |ui| {
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
                            Err(error) => self.feedback(error.to_string(), true),
                        }
                    }
                    ui.add(
                        egui::TextEdit::singleline(&mut self.create_password)
                            .id_salt("create_password")
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
                        .on_disabled_hover_text("Back up your recovery phrase and check the confirmation above before creating a wallet.")
                        .clicked()
                    {
                        match validate_wallet_form(&self.account_name, &self.mnemonic, &self.create_password) {
                            Ok(()) => {
                                self.import_error.clear();
                                self.import_pending = true;
                                self.backend.command(Command::Import {
                                    name: self.account_name.trim().to_owned(),
                                    mnemonic: Zeroizing::new(self.mnemonic.clone()),
                                    password: Zeroizing::new(self.create_password.clone()),
                                });
                            }
                            Err(error) => self.import_error = error,
                        }
                    }
                });
                if self.import_pending {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Encrypting and saving your wallet…");
                    });
                }
                if !self.import_error.is_empty() {
                    ui.colored_label(ui.visuals().error_fg_color, &self.import_error);
                }
                if !state.accounts.is_empty()
                    && !self.import_pending
                    && ui.button("Cancel adding wallet").clicked()
                {
                    self.show_wallet_form = false;
                    self.mnemonic.zeroize();
                    self.create_password.zeroize();
                    self.backup_confirmed = false;
                    self.import_error.clear();
                }
            });
        }
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
            let stale = state.node_error.is_some()
                || state.node_updated.is_none_or(|updated| {
                    updated.elapsed().as_secs() >= self.settings.poll_seconds * 3
                });
            let values = [
                (
                    "Block height",
                    node.peak
                        .as_ref()
                        .map(|peak| peak.height.to_string())
                        .unwrap_or_else(|| "Unavailable".into()),
                    "Height is the zero-based position of the current peak block. Genesis has height 0. A reorganization can change the peak.",
                ),
                (
                    "Sync status",
                    node_sync_label(node, stale).into(),
                    "The connected node's reported synchronization state, not an independent verification of the entire network. Stale samples are never shown as synced.",
                ),
                (
                    "Difficulty",
                    node.difficulty.to_string(),
                    "Current consensus difficulty. Each block adds its difficulty to cumulative chain weight. This is not the block height.",
                ),
                (
                    "Pending transactions",
                    node.mempool_size.to_string(),
                    "Transactions accepted into this node's mempool and waiting for inclusion in a block. Other nodes may have different pending transactions.",
                ),
            ];
            let count = if ui.available_width() >= 850.0 { 4 } else { 2 };
            for group in values.chunks(count) {
                ui.columns(count, |columns| {
                    for (column, (title, value, help)) in columns.iter_mut().zip(group) {
                        column
                            .scope(|ui| card(ui, title, value))
                            .response
                            .on_hover_text(*help);
                    }
                });
                ui.add_space(12.0);
            }
            section(ui, "Synchronization", |ui| {
                if stale {
                    ui.colored_label(ui.visuals().warn_fg_color, "Showing the last successful sample. Reconnect before relying on these values.");
                } else if node.sync.synced && !node.sync.sync_mode && node.peak.is_some() {
                    ui.colored_label(
                        crate::theme::GREEN,
                        "Following the chain · the node reports it is up to date",
                    );
                } else if node.sync.sync_tip_height > 0 {
                    let progress =
                        node.sync.sync_progress_height as f32 / node.sync.sync_tip_height as f32;
                    ui.add(egui::ProgressBar::new(progress.min(1.0)).text(format!("Syncing {} / {}", node.sync.sync_progress_height, node.sync.sync_tip_height)))
                        .on_hover_text("Downloaded/validated progress and target reported by this node's sync process. The target can increase as new blocks arrive.");
                } else {
                    ui.weak("Waiting for peers to establish a synchronization target.");
                }
                ui.weak(format!(
                    "{} · {}:{}",
                    self.settings.network, self.settings.node_host, self.settings.node_port
                ));
            });
            section(ui, "Chain progress", |ui| {
                ui.weak("Height counts blocks. Weight adds up their difficulty; it is not another height counter.");
                metric_grid(ui, "chain_progress").show(ui, |ui| {
                    if let Some(peak) = &node.peak {
                        detail(ui, "Cumulative chain weight", peak.weight.to_string());
                        detail(ui, "Total VDF iterations", peak.total_iters.to_string());
                        detail(ui, "Peak block hash", peak.header_hash.to_string());
                        detail(ui, "Previous block hash", peak.prev_hash.to_string());
                    } else {
                        detail(ui, "Peak block", "No accepted block yet".into());
                    }
                    detail(
                        ui,
                        "Iterations per sub-slot",
                        node.sub_slot_iters.to_string(),
                    );
                    detail(ui, "Estimated network space", format_space(node.space));
                });
            });
            section(ui, "Transaction pool", |ui| {
                let capacity = node.mempool_cost as f64 / node.mempool_max_total_cost.max(1) as f64;
                ui.label(format!(
                    "{:.1}% of transaction pool capacity used",
                    capacity * 100.0
                ));
                capacity_bar(ui, capacity as f32)
                    .on_hover_text("Capacity is measured in transaction execution cost, not bytes or a percentage of confirmed transactions.");
                metric_grid(ui, "mempool_details").show(ui, |ui| {
                    detail(
                        ui,
                        "Current / maximum cost",
                        format!("{} / {}", node.mempool_cost, node.mempool_max_total_cost),
                    );
                    detail(ui, "Block CLVM cost limit", node.block_max_cost.to_string());
                    detail(
                        ui,
                        "Minimum fee per cost",
                        node.mempool_min_fees.cost_5000000.to_string(),
                    );
                });
            });
            section(ui, "Node identity", |ui| {
                ui.weak("Public identifier of the connected node, not a wallet address.");
                ui.horizontal_wrapped(|ui| {
                    ui.monospace(node.node_id.to_string());
                    if ui.small_button("Copy node ID").clicked() {
                        ui.ctx().copy_text(node.node_id.to_string());
                    }
                });
            });
            data_section(
                ui,
                "Peak block and synchronization snapshot",
                &serde_json::to_string_pretty(node).unwrap_or_default(),
            );
        }
        data_section(ui, "Block counters", &state.node_metrics);
        data_section(ui, "Live diagnostics", &state.node_details);
        match self.settings.constants() {
            Ok(constants) => {
                data_section(
                    ui,
                    "Network rules",
                    &serde_json::to_string_pretty(&constants).unwrap_or_default(),
                );
            }
            Err(error) => {
                ui.label(error.to_string());
            }
        };
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
                if selection_button(
                    ui,
                    &account.account.name,
                    self.selected_account.as_ref() == Some(&account.account.id),
                )
                .clicked()
                {
                    self.selected_account = Some(account.account.id.clone());
                    self.farmer_password.zeroize();
                }
            }
            if selected_unlocked(state, self.selected_account.as_deref()) {
                ui.colored_label(
                    crate::theme::GREEN,
                    "Wallet already unlocked. No additional password needed.",
                );
            } else if self.selected_account.is_some() {
                password_field(ui, &mut self.farmer_password);
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
                    password: Zeroizing::new(std::mem::take(&mut self.farmer_password)),
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
        self.pool_settings(ui, state);
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

    fn pool_settings(&mut self, ui: &mut Ui, state: &State) {
        section(ui, "Pool settings", |ui| {
            ui.weak("Settings are read from the pool, not imposed by this farmer. Only changes you confirm are sent. Authentication keys are never rotated automatically.");
            ui.weak("Displayed values are the last loaded snapshot. Reload to see changes made elsewhere.");
            if ui
                .add_enabled(
                    state.farmer_running,
                    egui::Button::new("Load current pool settings"),
                )
                .clicked()
            {
                self.pool_draft = None;
                self.pool_confirm = false;
                self.backend.command(Command::LoadPools);
            }
            for pool in &state.pool_settings {
                ui.label(format!("{} · {}", pool.info.name, pool.config.launcher_id));
                detail(ui, "Pool", pool.config.pool_url.clone());
                detail(
                    ui,
                    "Payout instructions",
                    pool.farmer.payout_instructions.clone(),
                );
                detail(
                    ui,
                    "Current difficulty",
                    pool.farmer.current_difficulty.to_string(),
                );
                detail(
                    ui,
                    "Minimum difficulty",
                    pool.info.minimum_difficulty.to_string(),
                );
                detail(ui, "Points", pool.farmer.current_points.to_string());
                detail(ui, "Pool fee", pool.info.fee.clone());
                if ui
                    .push_id(pool.config.launcher_id.to_string(), |ui| {
                        ui.button("Edit pool settings")
                    })
                    .inner
                    .clicked()
                {
                    self.pool_draft = Some((
                        pool.clone(),
                        pool.farmer.payout_instructions.clone(),
                        pool.farmer.current_difficulty.to_string(),
                    ));
                    self.pool_confirm = false;
                }
            }
            let mut clear = false;
            let mut error = None;
            if let Some((expected, payout, difficulty)) = &mut self.pool_draft {
                ui.separator();
                ui.label(format!("Editing {}", expected.info.name));
                field(ui, "Payout address or puzzle hash", payout);
                field(ui, "Requested difficulty", difficulty);
                ui.weak("The pool may adjust difficulty later. This does not change pool membership or the on-chain pooling contract.");
                if ui.button("Review changes").clicked() {
                    self.pool_confirm = true;
                }
                if self.pool_confirm {
                    ui.label(format!(
                        "Payout: {} → {}",
                        expected.farmer.payout_instructions, payout
                    ));
                    ui.label(format!(
                        "Difficulty: {} → {}",
                        expected.farmer.current_difficulty, difficulty
                    ));
                    ui.weak("Settings are checked again before saving. Reload if another farmer has changed them.");
                    if primary_button(ui, "Confirm pool changes").clicked() {
                        match difficulty.trim().parse::<u64>() {
                            Ok(difficulty) => {
                                self.backend.command(Command::UpdatePool {
                                    expected: Box::new(expected.clone()),
                                    payout: payout.clone(),
                                    difficulty,
                                });
                                self.pool_confirm = false;
                            }
                            Err(_) => error = Some("Enter a whole-number difficulty."),
                        }
                    }
                }
                if ui.button("Cancel editing").clicked() {
                    clear = true;
                }
            }
            if clear {
                self.pool_draft = None;
                self.pool_confirm = false;
            }
            if let Some(error) = error {
                self.feedback(error, true);
            }
        });
    }

    fn wallet_assets(
        &mut self,
        ui: &mut Ui,
        id: &str,
        snapshot: &dg_xch_wallet::accounts::WalletSnapshot,
    ) {
        section(
            ui,
            if self.wallet_tab == 1 {
                "Tokens"
            } else if self.wallet_tab == 2 {
                "NFTs"
            } else {
                "DIDs"
            },
            |ui| {
                ui.weak("CAT1 is read-only. CAT2 amounts use base units (1 token = 1000 units). NFTs use the NFT1 standard. Untrusted metadata is displayed as text, never fetched or executed.");
                if self.wallet_tab == 3 {
                    ui.weak("Chia DID1 identities. Creation uses one mojo, with social recovery disabled. JuliaDID support is planned but unavailable.");
                    field(ui, "Transaction fee (XCH)", &mut self.asset_form.fee);
                    if ui
                        .add_enabled(snapshot.synced, egui::Button::new("Review DID creation"))
                        .clicked()
                    {
                        match parse_mojos(&self.asset_form.fee).map_err(std::io::Error::other).and_then(|fee| {
                            Ok(AssetConfirmation {
                                id: id.into(), genesis: self.settings.trusted_genesis()?,
                                action: AssetAction::LaunchDid(dg_xch_wallet::assets::DidType::Cni), fee,
                                description: "Create a Chia DID1 identity using one mojo. Social recovery is disabled; keep your wallet recovery phrase safe.".into(),
                            })
                        }) {
                            Ok(confirmation) => self.asset_confirmation = Some(confirmation),
                            Err(error) => self.feedback(error.to_string(), true),
                        }
                    }
                }
                if self.wallet_tab == 1 {
                    field(
                        ui,
                        "CAT asset ID to watch (64 hex characters)",
                        &mut self.asset_form.watched_cat,
                    );
                    if ui.button("Watch CAT asset").clicked() {
                        match dg_xch_wallet::assets::parse_asset_hash(&self.asset_form.watched_cat)
                        {
                            Ok(asset_id) => self.backend.command(Command::WatchCat {
                                id: id.into(),
                                asset_id,
                            }),
                            Err(error) => self.feedback(format!("Invalid asset ID: {error}"), true),
                        }
                    }
                }
                let show_nfts = self.wallet_tab == 2;
                if self.wallet_tab == 1 {
                    let mut balances = std::collections::BTreeMap::<String, u128>::new();
                    for asset in snapshot.assets.iter().filter(|asset| {
                        !asset.record.spent
                            && matches!(asset.kind, AssetKind::Cat1 | AssetKind::Cat2)
                    }) {
                        *balances
                            .entry(format!("{:?} · {}", asset.kind, asset.asset_id))
                            .or_default() += u128::from(asset.record.coin.amount);
                    }
                    for (asset, balance) in balances {
                        detail(ui, &asset, format!("{balance} base units"));
                    }
                }
                ui.checkbox(&mut self.asset_form.show_spent, "Include spent asset coins");
                let visible: Vec<_> = snapshot
                    .assets
                    .iter()
                    .filter(|asset| {
                        (!asset.record.spent || self.asset_form.show_spent)
                            && match self.wallet_tab {
                                1 => matches!(asset.kind, AssetKind::Cat1 | AssetKind::Cat2),
                                2 => asset.kind == AssetKind::Nft1,
                                _ => matches!(asset.kind, AssetKind::Did(_)),
                            }
                    })
                    .collect();
                if visible.is_empty() {
                    ui.weak(if show_nfts { "No NFTs discovered." } else if self.wallet_tab == 3 { "No DIDs discovered." } else { "No tokens discovered. Watch a CAT asset ID to find older coins without address hints." });
                }
                for asset in visible {
                    ui.push_id(asset.record.coin.name().to_string(), |ui| {
                    section(ui, &format!("{:?} · {}", asset.kind, asset.asset_id), |ui| {
                        detail(ui, "Coin ID", asset.record.coin.name().to_string());
                        detail(ui, "State", if asset.record.spent { "Spent".into() } else { "Unspent".into() });
                        detail(ui, "Amount (base units)", asset.record.coin.amount.to_string());
                        if let Some(royalty) = asset.royalty_basis_points { detail(ui, "Royalty (basis points)", royalty.to_string()); }
                        if let Some(owner) = asset.did_owner { detail(ui, "DID owner", owner.to_string()); }
                        if let Some(metadata) = &asset.metadata_summary { data_section(ui, "Metadata", metadata); }
                        if asset.record.spent {
                            ui.weak("Historical coin; no longer spendable.");
                        } else if asset.reserved {
                            ui.weak("Reserved by a submitted transaction. Sending is disabled until reconciliation.");
                        } else if asset.kind == AssetKind::Cat1 {
                            ui.weak("Retired CAT1 asset — viewing only. This wallet will not sign a CAT1 spend.");
                        } else if self.asset_form.selected_coin == Some(asset.record.coin.name()) {
                            field(ui, "Recipient address", &mut self.asset_form.destination);
                            if asset.kind == AssetKind::Cat2 { field(ui, "Amount (base units)", &mut self.asset_form.amount); }
                            field(ui, "Transaction fee (XCH)", &mut self.asset_form.fee);
                            if ui.add_enabled(snapshot.synced, egui::Button::new("Review asset transfer")).clicked() {
                                let result = (|| -> Result<AssetConfirmation, std::io::Error> {
                                    let constants = self.settings.constants()?;
                                    let address = self.asset_form.destination.trim();
                                    let (destination, encoded) = dg_xch_keys::convert_address(address, constants.bech32_prefix)?;
                                    if encoded != address.to_lowercase() { return Err(std::io::Error::other("enter an address for the selected network")); }
                                    let amount = if asset.kind != AssetKind::Cat2 { asset.record.coin.amount } else { self.asset_form.amount.trim().parse::<u64>().map_err(std::io::Error::other)? };
                                    if amount == 0 { return Err(std::io::Error::other("amount must be positive")); }
                                    let note = match asset.kind {
                                        AssetKind::Cat2 => "Additional spendable coins of this token may be combined. Remaining tokens return as change.",
                                        AssetKind::Nft1 => "This transfers the entire NFT and clears its DID owner assignment.",
                                        _ => "This transfers ownership of the entire identity to the recipient.",
                                    };
                                    Ok(AssetConfirmation { id: id.into(), genesis: self.settings.trusted_genesis()?, action: AssetAction::Transfer { coin_id: asset.record.coin.name(), destination, amount }, fee: parse_mojos(&self.asset_form.fee).map_err(std::io::Error::other)?, description: format!("Transfer {amount} base units of {:?} {}\nStarting coin: {}\nRecipient: {encoded}\n{note}", asset.kind, asset.asset_id, asset.record.coin.name()) })
                                })();
                                match result { Ok(confirmation) => self.asset_confirmation = Some(confirmation), Err(error) => self.feedback(error.to_string(), true) }
                            }
                            if ui.button("Close transfer form").clicked() { self.asset_form.selected_coin = None; }
                        } else if ui.button("Send this asset").clicked() {
                            self.asset_form.selected_coin = Some(asset.record.coin.name());
                            self.asset_form.destination.clear();
                            self.asset_form.amount.clear();
                        }
                    });
                });
                }
            },
        );
    }

    fn offers(&mut self, ui: &mut Ui, state: &State) {
        use dg_xch_wallet::offers::{OfferAmount, OfferAsset};
        section(ui, "Wallet", |ui| {
            for account in state.accounts.iter().filter(|account| account.unlocked) {
                if selection_button(
                    ui,
                    &account.account.name,
                    self.selected_account.as_ref() == Some(&account.account.id),
                )
                .clicked()
                {
                    self.selected_account = Some(account.account.id.clone());
                }
            }
            ui.weak("XCH and standard CAT2 offers are supported. NFT trades and restricted CATs are not supported yet. Amounts are integer base units, not display token units.");
            field(ui, "Transaction fee (XCH)", &mut self.offer_form.fee);
        });
        let selected = self.selected_account.clone();
        let ready = selected.as_ref().is_some_and(|id| {
            state.accounts.iter().any(|account| {
                account.account.id == *id
                    && account.unlocked
                    && account.error.is_none()
                    && account
                        .snapshot
                        .as_ref()
                        .is_some_and(|snapshot| snapshot.synced)
            })
        });
        let mut action = None;
        section(ui, "Create offer", |ui| {
            field(
                ui,
                "Give asset (blank for XCH, otherwise CAT2 ID)",
                &mut self.offer_form.give_asset,
            );
            field(
                ui,
                "Give amount (base units)",
                &mut self.offer_form.give_amount,
            );
            field(
                ui,
                "Receive asset (blank for XCH, otherwise CAT2 ID)",
                &mut self.offer_form.receive_asset,
            );
            field(
                ui,
                "Receive amount (base units)",
                &mut self.offer_form.receive_amount,
            );
            if ui
                .add_enabled(ready, egui::Button::new("Review new offer"))
                .clicked()
            {
                let parsed = (|| -> Result<_, std::io::Error> {
                    let amount =
                        |asset: &str, amount: &str| -> Result<OfferAmount, std::io::Error> {
                            let asset = if asset.trim().is_empty() {
                                OfferAsset::Xch
                            } else {
                                OfferAsset::Cat2(dg_xch_wallet::assets::parse_asset_hash(asset)?)
                            };
                            let amount = amount
                                .trim()
                                .parse::<u64>()
                                .map_err(std::io::Error::other)?;
                            if amount == 0 {
                                return Err(std::io::Error::other("amount must be positive"));
                            }
                            Ok(OfferAmount { asset, amount })
                        };
                    let give = amount(&self.offer_form.give_asset, &self.offer_form.give_amount)?;
                    let receive = amount(
                        &self.offer_form.receive_asset,
                        &self.offer_form.receive_amount,
                    )?;
                    if give.asset == receive.asset {
                        return Err(std::io::Error::other("choose two different assets"));
                    }
                    let description = format!(
                        "Give: {}\nReceive: {}",
                        offer_amount_label(&give),
                        offer_amount_label(&receive)
                    );
                    Ok((OfferAction::Create { give, receive }, description))
                })();
                match parsed {
                    Ok(value) => action = Some(value),
                    Err(error) => self.feedback(error.to_string(), true),
                }
            }
        });
        section(ui, "Import offer", |ui| {
            ui.add(
                egui::TextEdit::multiline(&mut self.offer_form.imported)
                    .id_salt("import_offer")
                    .char_limit(1024 * 1024)
                    .desired_rows(4)
                    .hint_text("Paste an offer1… string"),
            );
            if ui
                .add_enabled(ready, egui::Button::new("Review acceptance"))
                .clicked()
            {
                match dg_xch_wallet::offers::review(&self.offer_form.imported) {
                    Ok(terms) => {
                        action = Some((
                            OfferAction::Take(self.offer_form.imported.clone()),
                            format!(
                                "You receive:\n{}\nYou pay:\n{}\nReview does not prove the offer is still spendable; the node validates the completed transaction.",
                                terms
                                    .offered
                                    .iter()
                                    .map(offer_amount_label)
                                    .collect::<Vec<_>>()
                                    .join("\n"),
                                terms
                                    .requested
                                    .iter()
                                    .map(offer_amount_label)
                                    .collect::<Vec<_>>()
                                    .join("\n")
                            ),
                        ))
                    }
                    Err(error) => self.feedback(error.to_string(), true),
                }
            }
        });
        if let Some(saved) = selected.as_ref().and_then(|id| state.offers.get(id)) {
            for (id, text, active) in saved {
                data_section(ui, &format!("Saved offer {id}"), text);
                if *active
                    && ui
                        .add_enabled(ready, egui::Button::new("Review on-chain cancellation"))
                        .clicked()
                {
                    action = Some((
                        OfferAction::Cancel(*id),
                        format!(
                            "Cancel offer {id} by spending its owned inputs back to this wallet. This can race with acceptance. A CAT-only offer needs a zero cancellation fee unless it already includes XCH inputs."
                        ),
                    ));
                }
            }
        }
        if let Some((action, description)) = action {
            let result = (|| -> Result<OfferConfirmation, std::io::Error> {
                Ok(OfferConfirmation {
                    id: selected
                        .ok_or_else(|| std::io::Error::other("unlock and select a wallet"))?,
                    genesis: self.settings.trusted_genesis()?,
                    action,
                    fee: parse_mojos(&self.offer_form.fee).map_err(std::io::Error::other)?,
                    description,
                })
            })();
            match result {
                Ok(confirmation) => self.offer_confirmation = Some(confirmation),
                Err(error) => self.feedback(error.to_string(), true),
            }
        }
    }

    fn tools(&mut self, ui: &mut Ui, state: &State) {
        heading(ui, "Tools", "Address utilities and asset creation.");
        ui.horizontal_wrapped(|ui| {
            for (index, name) in ["Address converter", "Launch CAT2", "Launch NFT", "Offers"]
                .iter()
                .enumerate()
            {
                if selection_button(ui, name, self.tool_tab == index as u8).clicked() {
                    self.tool_tab = index as u8;
                }
            }
        });
        if self.tool_tab == 3 {
            self.offers(ui, state);
            return;
        }
        if self.tool_tab == 0 {
            section(ui, "Bech32m address converter", |ui| {
                field(
                    ui,
                    "Address or 32-byte puzzle hash",
                    &mut self.converter_input,
                );
                field(ui, "Output prefix", &mut self.converter_prefix);
                ui.weak("Chia uses xch on mainnet and txch on testnet. Changing a prefix does not move funds between networks.");
                if primary_button(ui, "Convert address").clicked() {
                    match dg_xch_keys::convert_address(
                        &self.converter_input,
                        self.converter_prefix.trim(),
                    ) {
                        Ok((hash, address)) => {
                            self.converter_result = Some((hash.to_string(), address));
                            self.feedback("Address converted.", false);
                        }
                        Err(error) => {
                            self.converter_result = None;
                            self.feedback(error.to_string(), true);
                        }
                    }
                }
                if let Some((hash, address)) = &self.converter_result {
                    data_section(ui, "Address", address);
                    data_section(ui, "Puzzle hash", hash);
                }
            });
        } else {
            section(ui, "Funding wallet", |ui| {
                for account in state.accounts.iter().filter(|account| account.unlocked) {
                    if selection_button(
                        ui,
                        &account.account.name,
                        self.selected_account.as_ref() == Some(&account.account.id),
                    )
                    .clicked()
                    {
                        self.selected_account = Some(account.account.id.clone());
                    }
                }
                ui.weak("Unlock a funded wallet on the Wallets page first. Every launch is reviewed before signing. Keep the resulting asset or launcher ID; token names are not on-chain identities.");
            });
            let account = state.accounts.iter().find(|account| {
                self.selected_account.as_ref() == Some(&account.account.id) && account.unlocked
            });
            let ready = account.is_some_and(|account| {
                account.error.is_none()
                    && account
                        .snapshot
                        .as_ref()
                        .is_some_and(|snapshot| snapshot.synced)
            });
            section(
                ui,
                if self.tool_tab == 1 {
                    "Launch CAT2"
                } else {
                    "Launch NFT"
                },
                |ui| {
                    if self.tool_tab == 1 {
                        field(
                            ui,
                            "Fixed supply (base units, 1000 = 1 token)",
                            &mut self.asset_form.supply,
                        );
                        ui.weak("Creates a new fixed-supply CAT2 using the genesis-by-coin-ID issuance policy. The supply locks the same number of XCH mojos, in addition to the fee. There is no later minting authority.");
                    } else {
                        field(
                            ui,
                            "Content URI (https:// or ipfs://)",
                            &mut self.asset_form.uri,
                        );
                        field(
                            ui,
                            "Content SHA-256 hash (64 hex characters)",
                            &mut self.asset_form.hash,
                        );
                        field(
                            ui,
                            "Royalty (basis points, 100 = 1%)",
                            &mut self.asset_form.royalty,
                        );
                        field(
                            ui,
                            "Royalty address (blank = this wallet)",
                            &mut self.asset_form.royalty_address,
                        );
                        field(ui, "Edition number", &mut self.asset_form.edition);
                        field(ui, "Edition total", &mut self.asset_form.editions);
                        ui.weak("Creates one NFT1, costing one mojo plus the fee. Supply your own content hash; the app does not upload, download or verify the hosted file. No DID is assigned at minting.");
                    }
                    field(ui, "Transaction fee (XCH)", &mut self.asset_form.fee);
                    if ui
                        .add_enabled(ready, egui::Button::new("Review launch"))
                        .on_disabled_hover_text("Unlock a wallet and wait for a successful sync.")
                        .clicked()
                    {
                        let result = (|| -> Result<AssetConfirmation, std::io::Error> {
                            let account = account.ok_or_else(|| {
                                std::io::Error::other("choose an unlocked wallet")
                            })?;
                            let snapshot = account
                                .snapshot
                                .as_ref()
                                .ok_or_else(|| std::io::Error::other("wallet has not synced"))?;
                            let action = if self.tool_tab == 1 {
                                let amount = self
                                    .asset_form
                                    .supply
                                    .trim()
                                    .parse::<u64>()
                                    .map_err(std::io::Error::other)?;
                                if amount == 0 {
                                    return Err(std::io::Error::other("supply must be positive"));
                                }
                                AssetAction::LaunchCat2 { amount }
                            } else {
                                let royalty_puzzle_hash =
                                    if self.asset_form.royalty_address.trim().is_empty() {
                                        snapshot.receive_puzzle_hash
                                    } else {
                                        let address = self.asset_form.royalty_address.trim();
                                        let (hash, encoded) = dg_xch_keys::convert_address(
                                            address,
                                            self.settings.constants()?.bech32_prefix,
                                        )?;
                                        if encoded != address.to_lowercase() {
                                            return Err(std::io::Error::other(
                                                "royalty address belongs to another network",
                                            ));
                                        }
                                        hash
                                    };
                                AssetAction::LaunchNft(NftLaunch {
                                    data_uri: self.asset_form.uri.trim().into(),
                                    data_hash: dg_xch_wallet::assets::parse_asset_hash(
                                        &self.asset_form.hash,
                                    )?,
                                    royalty_basis_points: self
                                        .asset_form
                                        .royalty
                                        .trim()
                                        .parse()
                                        .map_err(std::io::Error::other)?,
                                    royalty_puzzle_hash,
                                    edition_number: self
                                        .asset_form
                                        .edition
                                        .trim()
                                        .parse()
                                        .map_err(std::io::Error::other)?,
                                    edition_total: self
                                        .asset_form
                                        .editions
                                        .trim()
                                        .parse()
                                        .map_err(std::io::Error::other)?,
                                })
                            };
                            let details = match &action {
                                AssetAction::LaunchCat2 { amount } => format!(
                                    "Create a fixed supply of {amount} CAT2 base units.\nXCH locked in the token supply: {}",
                                    format_mojos(u128::from(*amount))
                                ),
                                AssetAction::LaunchNft(request) => {
                                    request.validate()?;
                                    format!(
                                        "Mint one NFT1, edition {} of {}.\nContent: {}\nSHA-256: {}\nRoyalty: {}.{:02}%\nRoyalty puzzle hash: {}\nXCH locked in the NFT: 0.000000000001",
                                        request.edition_number,
                                        request.edition_total,
                                        request.data_uri,
                                        request.data_hash,
                                        request.royalty_basis_points / 100,
                                        request.royalty_basis_points % 100,
                                        request.royalty_puzzle_hash
                                    )
                                }
                                AssetAction::LaunchDid(_) | AssetAction::Transfer { .. } => {
                                    return Err(std::io::Error::other("unexpected launch action"));
                                }
                            };
                            let description = format!(
                                "{details}\nRecipient wallet: {}\nReceive puzzle hash: {}",
                                account.account.name, snapshot.receive_puzzle_hash
                            );
                            Ok(AssetConfirmation {
                                id: account.account.id.clone(),
                                genesis: self.settings.trusted_genesis()?,
                                action,
                                fee: parse_mojos(&self.asset_form.fee)
                                    .map_err(std::io::Error::other)?,
                                description,
                            })
                        })();
                        match result {
                            Ok(confirmation) => self.asset_confirmation = Some(confirmation),
                            Err(error) => self.feedback(error.to_string(), true),
                        }
                    }
                },
            );
        }
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
            metric_grid(ui, "plot_parameters").show(ui, |ui| {
                ui.label("Plot size").on_hover_text("Only even k sizes are supported. Small sizes are for development, not mainnet farming.");
                egui::ComboBox::from_id_salt("plot_size").width(180.0).selected_text(format!("k{}", self.plot_k)).show_ui(ui, |ui| {
                    for size in (18..=32).step_by(2) {
                        ui.selectable_value(&mut self.plot_k, size, format!("k{size}"));
                    }
                });
                ui.end_row();
                ui.label("Strength").on_hover_text("Controls the proof-of-space work parameter. Valid values depend on plot size.");
                let maximum = self.plot_k
                    - if self.plot_k < 28 {
                        2
                    } else {
                        self.plot_k - 26
                    }
                    - 1;
                self.plot_strength = self.plot_strength.clamp(2, maximum);
                ui.add_sized([180.0, 36.0], egui::DragValue::new(&mut self.plot_strength).range(2..=maximum));
                ui.end_row();
                ui.label("Compute device");
                egui::ComboBox::from_id_salt("plot_compute").width(180.0).selected_text(if self.plot_gpu { "GPU (Settings backend)" } else { "CPU" }).show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.plot_gpu, false, "CPU");
                    ui.selectable_value(&mut self.plot_gpu, true, "GPU (Settings backend)");
                });
                ui.end_row();
                ui.label("Memory budget (MiB)").on_hover_text("A maximum, not a reservation. Leave memory for the node and other applications.");
                ui.add_sized([180.0, 36.0], egui::DragValue::new(&mut self.plot_memory_mib).range(128..=524288));
                ui.end_row();
            });
        });
        section(ui, "Advanced plot parameters", |ui| {
            ui.weak("Keep these defaults unless your network or plotting setup requires a change.");
            metric_grid(ui, "plot_advanced").show(ui, |ui| {
                ui.label("Plot index").on_hover_text("Distinguishes plots with the same keys and parameters.");
                ui.add_sized([180.0, 36.0], egui::DragValue::new(&mut self.plot_index));
                ui.end_row();
                ui.label("Meta group").on_hover_text("PoS2 plot identity parameter. Leave at zero unless you are managing plot groups.");
                ui.add_sized([180.0, 36.0], egui::DragValue::new(&mut self.plot_meta));
                ui.end_row();
                ui.label("Hash domain");
                egui::ComboBox::from_id_salt("plot_domain").width(180.0).selected_text(if self.plot_testnet { "PoS2 testnet" } else { "Production" }).show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.plot_testnet, false, "Production");
                    ui.selectable_value(&mut self.plot_testnet, true, "PoS2 testnet");
                });
                ui.end_row();
                ui.label("Compute work limit").on_hover_text("Maximum 16-round AES evaluations. Stops the job before it exceeds this compute budget.");
                ui.add_sized([180.0, 36.0], egui::DragValue::new(&mut self.plot_max_work).range(1..=u64::MAX));
                ui.end_row();
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
                    Ok(_) => self.feedback("Choose an output directory", true),
                    Err(error) => self.feedback(error, true),
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
                    Err(error) => self.feedback(error.to_string(), true),
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
                if selection_button(
                    ui,
                    &account.account.name,
                    self.selected_account.as_ref() == Some(&account.account.id),
                )
                .clicked()
                {
                    self.selected_account = Some(account.account.id.clone());
                    self.plotting_password.zeroize();
                }
            }
            if selected_unlocked(state, self.selected_account.as_deref()) {
                ui.colored_label(crate::theme::GREEN, "Using your unlocked wallet.");
            } else if self.selected_account.is_some() {
                password_field(ui, &mut self.plotting_password);
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
                    password: Zeroizing::new(std::mem::take(&mut self.plotting_password)),
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
                    self.feedback(
                        "Public plotting keys applied. Choose a directory and plot size below.",
                        false,
                    );
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
        fn bytes<const SIZE: usize>(label: &str, value: &str) -> Result<[u8; SIZE], String> {
            let mut result = [0; SIZE];
            hex::decode_to_slice(value.trim().trim_start_matches("0x"), &mut result)
                .map_err(|_| format!("{label} must contain {SIZE} bytes of hexadecimal data. Load your wallet's public plotting keys or enter a valid value."))?;
            Ok(result)
        }
        Ok(PlotRequest {
            farmer_public_key: bytes("Farmer public key", &self.farmer_key)?,
            pool: if self.portable {
                PoolBinding::Contract(bytes("Pool contract puzzle hash", &self.pool_binding)?)
            } else {
                PoolBinding::PublicKey(bytes("Pool public key", &self.pool_binding)?)
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
            if selection_button(
                ui,
                "Forest dark",
                self.settings_draft.theme == Theme::Midnight,
            )
            .clicked()
            {
                self.settings_draft.theme = Theme::Midnight;
            }
            if selection_button(
                ui,
                "Garden light",
                self.settings_draft.theme == Theme::Daylight,
            )
            .clicked()
            {
                self.settings_draft.theme = Theme::Daylight;
            }
            if primary_button(ui, "Save settings").clicked() {
                self.backend
                    .command(Command::Settings(self.settings_draft.clone()));
            }
        });
        ui.weak("Themes preview immediately and can be saved while wallets are unlocked. Lock wallets and stop farming before changing other settings.");
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
                        self.feedback(error.to_string(), true);
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
            ui.label("Compute backend");
            egui::ComboBox::from_id_salt("gpu_backend")
                .width(180.0)
                .selected_text(match self.settings_draft.gpu_backend {
                    GpuBackend::Auto => "Auto",
                    GpuBackend::Cuda => "NVIDIA CUDA",
                    GpuBackend::Vulkan => "Vulkan",
                })
                .show_ui(ui, |ui| {
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
            if *frame >= 20 {
                completed.store(true, Ordering::Release);
                context.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }
            self.page = match *frame % 10 {
                0 => Page::Overview,
                1 => Page::Wallets,
                2 => Page::Node,
                3 => Page::Farm,
                4 => Page::Plots,
                5..=8 => Page::Tools,
                _ => Page::Settings,
            };
            self.tool_tab = (*frame % 10).saturating_sub(5).min(3);
            self.settings.theme = if *frame < 10 {
                Theme::Midnight
            } else {
                Theme::Daylight
            };
            *frame += 1;
            context.request_repaint();
        }
        context.request_repaint_after(Duration::from_millis(500));
        let state = self.backend.snapshot();
        if self.selected_account.is_none() {
            self.selected_account = state
                .accounts
                .first()
                .map(|account| account.account.id.clone());
        }
        if self.import_pending
            && let Some(result) = &state.import_result
        {
            self.import_pending = false;
            match result {
                Ok(id) => {
                    self.selected_account = Some(id.clone());
                    self.account_name.clear();
                    self.mnemonic.zeroize();
                    self.create_password.zeroize();
                    self.backup_confirmed = false;
                    self.show_wallet_form = false;
                    self.import_error.clear();
                }
                Err(error) => self.import_error = error.clone(),
            }
        }
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
                    (Page::Tools, "Tools"),
                    (Page::Settings, "Settings"),
                ] {
                    let selected = self.page == page;
                    let response = ui.add_sized(
                        [ui.available_width(), 44.0],
                        egui::Button::new(RichText::new(label).size(15.0)).selected(selected),
                    );
                    if response.clicked() {
                        self.page = page;
                        self.message.clear();
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
                if !self.message.is_empty() {
                    ui.horizontal_wrapped(|ui| {
                        ui.colored_label(
                            if self.message_error {
                                ui.visuals().error_fg_color
                            } else {
                                crate::theme::GREEN
                            },
                            if self.message_error {
                                "Please check"
                            } else {
                                "Done"
                            },
                        );
                        ui.label(&self.message);
                        if ui.small_button("Dismiss message").clicked() {
                            self.message.clear();
                        }
                    });
                    ui.add_space(12.0);
                }
                if !state.notice.is_empty()
                    && (state.notice != self.dismissed_notice
                        || state.notice_revision != self.dismissed_revision)
                {
                    ui.horizontal_wrapped(|ui| {
                        ui.colored_label(
                            if state.notice_error {
                                ui.visuals().error_fg_color
                            } else {
                                crate::theme::GREEN
                            },
                            if state.notice_error {
                                "Action failed"
                            } else {
                                "Status"
                            },
                        );
                        ui.label(&state.notice);
                        if ui.small_button("Dismiss").clicked() {
                            self.dismissed_notice.clone_from(&state.notice);
                            self.dismissed_revision = state.notice_revision;
                        }
                    });
                    ui.add_space(12.0);
                }
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
                            Page::Tools => self.tools(ui, &state),
                            Page::Settings => self.settings(ui),
                        }
                        ui.add_space(24.0);
                    });
            });
        if let Some(confirmation) = &self.offer_confirmation {
            let mut submit = false;
            let mut cancel = false;
            egui::Modal::new(egui::Id::new("offer_confirmation")).show(&context, |ui| {
                ui.set_max_width(560.0);
                ui.heading("Review offer operation");
                ui.label(format!("Network: {}", self.settings.network));
                egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| { ui.label(&confirmation.description); });
                ui.label(format!("Fee: {} XCH", format_mojos(u128::from(confirmation.fee))));
                ui.weak("Creating an offer reserves coins and signs a shareable commitment. Taking or cancelling broadcasts a transaction. Cancellation is effective only after confirmation.");
                let ready = self.settings.trusted_genesis().ok() == Some(confirmation.genesis) && selected_unlocked(&state, Some(&confirmation.id));
                submit = ui.add_enabled(ready, egui::Button::new("Confirm and sign")).clicked();
                cancel = ui.button("Back").clicked();
            });
            if submit {
                if let Some(confirmation) = self.offer_confirmation.take() {
                    self.backend.command(Command::Offer {
                        id: confirmation.id,
                        genesis: confirmation.genesis,
                        action: confirmation.action,
                        fee: confirmation.fee,
                    });
                }
            } else if cancel {
                self.offer_confirmation = None;
            }
        }
        if let Some(confirmation) = &self.asset_confirmation {
            let mut submit = false;
            let mut cancel = false;
            egui::Modal::new(egui::Id::new("asset_confirmation")).show(&context, |ui| {
                ui.set_width((context.content_rect().width() - 64.0).clamp(240.0, 560.0));
                ui.heading("Review asset transaction");
                ui.add_space(12.0);
                egui::ScrollArea::vertical().max_height((context.content_rect().height() - 240.0).max(160.0)).show(ui, |ui| {
                    ui.label(format!("Network: {}", self.settings.network));
                    ui.label(&confirmation.description);
                    ui.label(format!("Fee: {} XCH", format_mojos(u128::from(confirmation.fee))));
                    ui.weak("This signs and broadcasts a real transaction. Confirmed transactions cannot be undone.");
                });
                let same_wallet = self.settings.trusted_genesis().ok() == Some(confirmation.genesis) && selected_unlocked(&state, Some(&confirmation.id));
                ui.horizontal(|ui| {
                    submit = ui.add_enabled(same_wallet, egui::Button::new("Sign and broadcast")).on_disabled_hover_text("The wallet was locked or the network changed. Cancel and review again.").clicked();
                    cancel = ui.button("Cancel").clicked();
                });
            });
            if submit {
                if let Some(confirmation) = self.asset_confirmation.take() {
                    self.backend.command(Command::Asset {
                        id: confirmation.id,
                        genesis: confirmation.genesis,
                        action: confirmation.action,
                        fee: confirmation.fee,
                    });
                }
            } else if cancel {
                self.asset_confirmation = None;
            }
        }
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
        self.create_password.zeroize();
        self.farmer_password.zeroize();
        self.plotting_password.zeroize();
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

fn selection_button(ui: &mut Ui, label: &str, selected: bool) -> egui::Response {
    let button = egui::Button::new(RichText::new(label).color(if selected {
        Color32::WHITE
    } else {
        ui.visuals().text_color()
    }))
    .min_size(egui::vec2(140.0, 40.0));
    let response = ui.add(if selected {
        button.fill(crate::theme::GREEN)
    } else {
        button
    });
    if selected {
        let bounds = response.rect;
        ui.painter().line_segment(
            [
                bounds.left_bottom() + egui::vec2(12.0, -4.0),
                bounds.right_bottom() + egui::vec2(-12.0, -4.0),
            ],
            egui::Stroke::new(2.0, Color32::WHITE),
        );
    }
    response
}

fn selected_unlocked(state: &State, id: Option<&str>) -> bool {
    state
        .accounts
        .iter()
        .any(|account| Some(account.account.id.as_str()) == id && account.unlocked)
}

fn capacity_bar(ui: &mut Ui, fraction: f32) -> egui::Response {
    let (bounds, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 12.0), egui::Sense::hover());
    ui.painter()
        .rect_filled(bounds, 4, ui.visuals().faint_bg_color);
    ui.painter().rect_stroke(
        bounds,
        4,
        ui.visuals().window_stroke,
        egui::StrokeKind::Inside,
    );
    if fraction > 0.0 {
        let fill = egui::Rect::from_min_size(
            bounds.min,
            egui::vec2(bounds.width() * fraction.clamp(0.0, 1.0), bounds.height()),
        );
        ui.painter().rect_filled(fill, 4, crate::theme::GREEN);
    }
    response
}

fn validate_wallet_form(name: &str, mnemonic: &str, password: &str) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err("Enter a name for this wallet.".into());
    }
    if password.len() < 12 {
        return Err("Password is too short. Use at least 12 bytes (12 characters for an ASCII password). Your entries have been kept.".into());
    }
    bip39::Mnemonic::parse(mnemonic)
        .map_err(|_| "Enter a valid recovery phrase, or generate a new one.".to_owned())?;
    Ok(())
}

fn balance_text(amount: u128) -> String {
    format_mojos(amount)
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_owned()
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
    section_with_copy(ui, title, None, content)
}

fn data_section(ui: &mut Ui, title: &str, data: &str) {
    section_with_copy(ui, title, Some(data), |ui| data_view(ui, data));
}

fn section_with_copy<Response>(
    ui: &mut Ui,
    title: &str,
    data: Option<&str>,
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
                    ui.horizontal(|ui| {
                        if let Some(data) = data {
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    let id = ui.id().with("copied_at");
                                    let now = ui.input(|input| input.time);
                                    let copied = ui
                                        .ctx()
                                        .data(|memory| memory.get_temp::<f64>(id))
                                        .is_some_and(|time| now - time < 2.0);
                                    if ui
                                        .add_enabled(
                                            !data.trim().is_empty(),
                                            egui::Button::new(if copied {
                                                "Copied"
                                            } else {
                                                "Copy data"
                                            })
                                            .min_size(egui::vec2(105.0, 32.0)),
                                        )
                                        .clicked()
                                    {
                                        ui.ctx().copy_text(data.to_owned());
                                        ui.ctx().data_mut(|memory| memory.insert_temp(id, now));
                                    }
                                    ui.with_layout(
                                        egui::Layout::left_to_right(egui::Align::Center),
                                        |ui| {
                                            ui.label(RichText::new(title).size(17.0).strong());
                                        },
                                    );
                                },
                            );
                        } else {
                            ui.label(RichText::new(title).size(17.0).strong());
                        }
                    });
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

fn metric_grid(ui: &Ui, id: &str) -> egui::Grid {
    let column_width = ((ui.available_width() - 24.0) / 2.0).max(1.0);
    egui::Grid::new(id)
        .num_columns(2)
        .striped(true)
        .min_col_width(column_width)
        .max_col_width(column_width)
        .min_row_height(32.0)
        .spacing([24.0, 10.0])
}

fn detail(ui: &mut Ui, label: &str, value: String) {
    let help = node_metric_help(label);
    ui.add(egui::Label::new(RichText::new(label).weak()).wrap())
        .on_hover_text(help);
    ui.add(egui::Label::new(RichText::new(value).monospace()).wrap())
        .on_hover_text(help);
    ui.end_row();
}

fn data_view(ui: &mut Ui, data: &str) {
    if data.trim().is_empty() {
        ui.weak("Waiting for a node sample. No data is available yet.");
        return;
    }
    match serde_json::from_str::<serde_json::Value>(data) {
        Ok(value) => {
            let mut remaining = 512;
            metric_grid(ui, "data").show(ui, |ui| {
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
            ui.add(egui::Label::new(RichText::new(path).weak()).wrap())
                .on_hover_text(node_metric_help(path));
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

fn node_sync_label(
    node: &dg_xch_core::blockchain::blockchain_state::BlockchainState,
    stale: bool,
) -> &'static str {
    if stale {
        "Stale"
    } else if node.peak.is_none() {
        "Waiting"
    } else if node.sync.synced && !node.sync.sync_mode {
        "Synced"
    } else {
        "Syncing"
    }
}

fn format_space(bytes: u128) -> String {
    if bytes == 0 {
        return "Not available".into();
    }
    let units = ["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB", "ZiB", "YiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < units.len() {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.2} {} (estimate)", units[unit])
}

fn node_metric_help(label: &str) -> &'static str {
    let field = label.rsplit(" / ").next().unwrap_or(label);
    match field {
        "Cumulative chain weight" | "weight" => {
            "Sum of block difficulties from genesis through this peak. Consensus uses weight to compare competing chains. At constant difficulty 1, weight is height + 1; it is not the block height."
        }
        "height" | "sync progress height" => {
            "Zero-based block height. Genesis is height 0. This identifies a block's position, not cumulative chain weight."
        }
        "difficulty" => {
            "Difficulty added by a block to cumulative chain weight. Difficulty can change as the chain adjusts to farming capacity."
        }
        "Total VDF iterations" | "total iters" => {
            "Cumulative verifiable-delay-function iterations up to this block's infusion point. This is proof-of-time work, not a block count or elapsed seconds."
        }
        "Iterations per sub-slot" | "sub slot iters" => {
            "Number of VDF iterations allocated to one sub-slot. This controls proof-of-time scheduling, not the number of blocks in a slot."
        }
        "Peak block hash" | "header hash" => {
            "Cryptographic header identifier of the current peak block. Nodes at the same height can briefly have different hashes during a fork."
        }
        "Previous block hash" | "prev hash" => {
            "Header hash of this block's parent. Genesis points to the network's genesis challenge instead."
        }
        "Estimated network space" | "space" => {
            "An estimate of space participating in farming, inferred from chain data. It is not the size of your plots, free disk space, or a precise measurement."
        }
        "Block CLVM cost limit" | "block max cost" => {
            "Maximum transaction execution cost permitted in one block. CLVM cost is a consensus resource budget, not bytes or a fee."
        }
        "Minimum fee per cost" | "cost 5000000" => {
            "Minimum fee rate, in mojos per unit of cost, reported for admitting a transaction with cost 5,000,000 into this mempool. This is not a guaranteed confirmation fee."
        }
        "Current / maximum cost" | "mempool cost" | "mempool max total cost" => {
            "Execution-cost usage or capacity of this node's pending transaction pool. This is independent of block height and disk usage."
        }
        "mempool size" => {
            "Number of pending transactions held by this node, not a count of confirmed transactions."
        }
        "inbound peer count" => {
            "Connections initiated by other peers to this node. This does not include outbound connections and is not the total peer count."
        }
        "claimed peer peak" => {
            "Highest peak claimed by peers. Peer announcements are not proof that this node has validated that height."
        }
        "synced" | "sync mode" | "sync tip height" => {
            "Node-reported sync state or target. A zero target can mean bulk sync is inactive; it does not mean the current chain height is zero."
        }
        "cached sub slots" => {
            "Sub-slot records currently retained in the node's live timing cache, not a lifetime total."
        }
        "unfinished blocks received" | "unfinished blocks requested" | "unfinished hashes seen" => {
            "Live unfinished-block cache or request count. These candidates still need infusion and validation before they become accepted blocks."
        }
        "timestamp" => {
            "Unix timestamp carried by a transaction block. Non-transaction blocks may not have their own timestamp."
        }
        "node id" => {
            "Public network identifier of this node. It is not a wallet address or a chain identifier."
        }
        _ => {
            "Diagnostic value reported by the connected node or its configured consensus rules. Samples can update independently; unavailable values are not zero."
        }
    }
}

#[cfg(test)]
mod node_view_tests {
    use super::*;

    #[test]
    fn wallet_form_validation_keeps_input_and_explains_errors() {
        let phrase = bip39::Mnemonic::from_entropy(&[15; 32])
            .unwrap()
            .to_string();
        assert!(
            validate_wallet_form("Test", &phrase, "short")
                .unwrap_err()
                .contains("too short")
        );
        assert!(
            validate_wallet_form("", &phrase, "a long test password")
                .unwrap_err()
                .contains("name")
        );
        assert!(
            validate_wallet_form("Test", "not a phrase", "a long test password")
                .unwrap_err()
                .contains("recovery phrase")
        );
        assert!(validate_wallet_form("Test", &phrase, "a long test password").is_ok());
        assert_eq!(
            phrase,
            bip39::Mnemonic::from_entropy(&[15; 32])
                .unwrap()
                .to_string()
        );
    }

    #[test]
    fn compact_balances_preserve_every_nonzero_decimal() {
        assert_eq!(balance_text(0), "0");
        assert_eq!(balance_text(1), "0.000000000001");
        assert_eq!(balance_text(1_500_000_000_000), "1.5");
        assert_eq!(balance_text(10_000_000_000_000), "10");
    }

    #[test]
    fn theme_selection_does_not_resize_on_hover() {
        for theme in [Theme::Daylight, Theme::Midnight] {
            let context = egui::Context::default();
            crate::theme::install_fonts(&context);
            let mut previous = None;
            for position in [
                egui::pos2(500.0, 500.0),
                egui::pos2(50.0, 20.0),
                egui::pos2(500.0, 500.0),
            ] {
                crate::theme::apply(&context, theme);
                let mut output = context.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(800.0, 600.0),
                        )),
                        events: vec![egui::Event::PointerMoved(position)],
                        ..Default::default()
                    },
                    |ui| {
                        let bounds = selection_button(ui, "Garden light", true).rect;
                        if let Some(previous) = previous {
                            assert_eq!(bounds, previous);
                        }
                        previous = Some(bounds);
                    },
                );
                output.textures_delta.clear();
            }
        }
    }

    #[test]
    fn metric_values_have_room_without_stacking_digits() {
        for width in [480.0, 960.0] {
            let context = egui::Context::default();
            for _ in 0..3 {
                let input = egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(width, 600.0),
                    )),
                    ..Default::default()
                };
                let mut output = context.run_ui(input, |ui| {
                    egui::CentralPanel::default().show(ui, |ui| {
                        let available = ui.available_width();
                        let response = metric_grid(ui, "metrics").show(ui, |ui| {
                            ui.label("Current / maximum cost");
                            let value = ui.add(
                                egui::Label::new(RichText::new("0 / 110000000000").monospace())
                                    .wrap(),
                            );
                            assert!(value.rect.height() < 32.0);
                            ui.end_row();
                            detail(ui, "Peak block hash", "a".repeat(64));
                        });
                        assert!(response.response.rect.width() >= available * 0.9);
                        assert!(response.response.rect.width() <= available + 2.0);
                    });
                });
                output.textures_delta.clear();
            }
        }
    }

    #[test]
    fn chain_metric_labels_distinguish_height_weight_and_cost() {
        assert!(node_metric_help("weight").contains("Sum of block difficulties"));
        assert!(node_metric_help("peak / height").contains("Zero-based"));
        assert!(node_metric_help("Minimum fee per cost").contains("mojos"));
        assert_eq!(format_space(0), "Not available");
        assert_eq!(format_space(1024_u128.pow(4)), "1.00 TiB (estimate)");
    }

    #[test]
    fn stale_and_empty_samples_do_not_claim_synced() {
        let mut node = dg_xch_core::blockchain::blockchain_state::BlockchainState::default();
        node.sync.synced = true;
        assert_eq!(node_sync_label(&node, false), "Waiting");
        assert_eq!(node_sync_label(&node, true), "Stale");
    }
}
