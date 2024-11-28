use eframe::egui;
use crate::app::DgXchGui;
use crate::scenes::Scene;

pub struct FarmerScene {}
impl FarmerScene {
    pub fn new() -> Self {
        FarmerScene {}
    }
}

impl Scene for FarmerScene {
    fn update(&mut self, _gui: &mut DgXchGui, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(20.0);
                ui.heading("FullNode");
                ui.add_space(20.0);
            });
        });

    }
}
