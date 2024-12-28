// wallet_scene.rs

use std::sync::{Arc, OnceLock};
use std::sync::atomic::Ordering;
use eframe::egui;
use crate::app::DgXchGui;
use crate::scenes::Scene;
use eframe::egui::Context;
use eframe::egui::mutex::Mutex;
use log::error;
use dg_xch_clients::api::full_node::FullnodeAPI;
use dg_xch_core::blockchain::spend_bundle::SpendBundle;
use crate::scenes::fullnode_overview::FullNodeOverviewScene;
use crate::scenes::wallet_import_mnemonic::WalletImportMnemonicScene;
use crate::scenes::wallet_overview::WalletOverviewScene;
use crate::scenes::wallet_transactions::WalletTransactionsScene;
use crate::state::{WalletState, WalletTab};

static OVERVIEW: OnceLock<Mutex<WalletOverviewScene>> = OnceLock::new();
static MNEMONIC: OnceLock<Mutex<WalletImportMnemonicScene>> = OnceLock::new();
static TRANSACTIONS: OnceLock<Mutex<WalletTransactionsScene>> = OnceLock::new();

pub struct WalletScene {
    pub shared_state: Arc<WalletState>,
    pub tabs: [(String, WalletTab); 3],
    pub selected_tab: WalletTab,
}

impl WalletScene {
    pub fn new(gui: &DgXchGui) -> Self {
        let shared_state = Arc::new(WalletState::default());
        let background_state = shared_state.clone();
        let client = gui.state.wallet_client.clone();
        let shutdown_signal = gui.state.shutdown_signal.clone();
        WalletScene {
            shared_state: Arc::new(Default::default()),
            tabs: [
                (String::from("Overview"), WalletTab::Overview),
                (String::from("Mnemonic"), WalletTab::Mnemonic),
                (String::from("Transactions"), WalletTab::Transactions),
            ],
            selected_tab: WalletTab::Overview,
        }
    }
}

impl Scene for WalletScene {
    
    fn update(&mut self, gui: &mut DgXchGui, ctx: &Context, frame: &mut eframe::Frame) {
        egui::SidePanel::left("fullnode_nav").resizable(false).show(ctx, |ui| {
            ui.vertical(|ui| {
                for (label, tab) in &self.tabs {
                    if ui.selectable_label(self.selected_tab == *tab, label).clicked() {
                        self.selected_tab = *tab;
                    }
                }
            });
        });
        let state = self.shared_state.clone();
        match self.selected_tab {
            WalletTab::Overview => {
                OVERVIEW.get_or_init(|| {
                    Mutex::new(WalletOverviewScene::new(state))
                }).lock().update(gui, ctx, frame);
            }
            WalletTab::Mnemonic => {
                crate::scenes::wallet::MNEMONIC.get_or_init(|| {
                    Mutex::new(WalletImportMnemonicScene::new(state))
                }).lock().update(gui, ctx, frame);
            }
            WalletTab::Transactions => {
                crate::scenes::wallet::TRANSACTIONS.get_or_init(|| {
                    Mutex::new(WalletTransactionsScene::new(state))
                }).lock().update(gui, ctx, frame);
            }
        }
        
        ctx.request_repaint_after(std::time::Duration::from_secs(1));
    }
}


