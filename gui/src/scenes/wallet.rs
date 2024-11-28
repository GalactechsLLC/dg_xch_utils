use eframe::egui;
use crate::app::DgXchGui;
use crate::scenes::Scene;

pub struct WalletScene {}
impl WalletScene {
    pub fn new() -> Self {
        WalletScene {}
    }
}

impl Scene for WalletScene {
    fn update(&mut self, _gui: &mut DgXchGui, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(20.0);
                ui.heading("Wallet");
                ui.add_space(20.0);
            });
        });

    }
}