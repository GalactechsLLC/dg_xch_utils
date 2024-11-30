use std::sync::Arc;
use eframe::egui;
use eframe::egui::Context;
use crate::app::DgXchGui;
use crate::scenes::Scene;
use crate::state::WalletState;

pub struct WalletOverviewScene {}

impl WalletOverviewScene {
    pub fn new(wallet_state: Arc<WalletState>) -> Self {
        WalletOverviewScene {}
    }
}

impl Scene for WalletOverviewScene {
    fn update(&mut self, gui: &mut DgXchGui, ctx: &Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Overview");
            ui.separator();
        });
    }
}
