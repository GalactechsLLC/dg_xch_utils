use crate::app::DgXchGui;
use crate::scenes::Scene;
use crate::state::WalletState;
use eframe::egui;
use eframe::egui::Context;
use std::sync::Arc;

pub struct WalletTransactionsScene {
    _wallet_state: Arc<WalletState>,
}

impl WalletTransactionsScene {
    pub fn new(wallet_state: Arc<WalletState>) -> Self {
        WalletTransactionsScene {
            _wallet_state: wallet_state,
        }
    }
}

impl Scene for WalletTransactionsScene {
    fn update(&mut self, _gui: &mut DgXchGui, ctx: &Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Transactions");
            ui.separator();
        });
    }
}
