// wallet_scene.rs

use std::sync::{Arc, OnceLock};
use eframe::egui;
use crate::app::DgXchGui;
use crate::scenes::Scene;
use eframe::egui::Context;
use eframe::egui::mutex::Mutex;
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
    pub fn new(_gui: &DgXchGui) -> Self {
        let shared_state = Arc::new(WalletState::default());
        WalletScene {
            shared_state,
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


