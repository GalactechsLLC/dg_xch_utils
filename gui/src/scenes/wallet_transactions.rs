use std::sync::Arc;
use eframe::egui;
use eframe::egui::Context;
use crate::app::DgXchGui;
use crate::scenes::Scene;
use crate::state::WalletState;

pub struct WalletTransactionsScene {}

impl WalletTransactionsScene {
    pub fn new(_wallet_state: Arc<WalletState>) -> Self {
        WalletTransactionsScene {}
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
