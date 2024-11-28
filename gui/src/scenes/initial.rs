use bip39::Mnemonic;
use eframe::egui;
use crate::app::DgXchGui;
use crate::scenes::Scene;

pub struct InitialScene {
    pub input_mnemonic: String,
}
impl InitialScene {
    pub fn new() -> Self {
        InitialScene {
            input_mnemonic: String::default(),
        }
    }
}

impl Scene for InitialScene {
    fn update(&mut self, gui: &mut DgXchGui, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.heading("Chia Wallet");
                ui.add_space(20.0);
                ui.group(|ui| {
                    ui.vertical_centered(|ui| {
                        ui.label("Enter Mnemonic:");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.input_mnemonic)
                                .hint_text("Your mnemonic phrase"),
                        );

                        ui.add_space(10.0);

                        ui.horizontal(|ui| {
                            if ui.button("Create New Wallet").clicked() {
                                let mnemonic = Mnemonic::generate(24).unwrap();
                                gui.wallet = Some(mnemonic.to_string());
                            }
                            if ui.button("Import From Mnemonic").clicked() {
                                gui.wallet = Some(self.input_mnemonic.clone());
                                self.input_mnemonic.clear();
                            }
                        });
                    });
                });
                ui.add_space(20.0);
            });
        });

    }
}
