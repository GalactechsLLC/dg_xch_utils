use std::sync::Arc;
use eframe::egui;
use crate::app::DgXchGui;
use crate::scenes::Scene;
use crate::state::{FullNodeState};

pub struct FullNodeOverviewScene {
    pub shared_state: Arc<FullNodeState>,
}
impl FullNodeOverviewScene {
    pub fn new(shared_state: Arc<FullNodeState>) -> Self {
        FullNodeOverviewScene {
            shared_state,
        }
    }
}

impl Scene for FullNodeOverviewScene {
    fn update(&mut self, _gui: &mut DgXchGui, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let (synced, height, mut space, difficulty) = match &*self.shared_state.blockchain_state.lock() {
            None => {
                (false, 0, 0, 0)
            }
            Some(state) => {
                ctx.request_repaint();
                (
                    state.sync.synced,
                    state.peak.as_ref().map(|p| p.height).unwrap_or_default(),
                    state.space,
                    state.difficulty,
                )
            }
        };
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(20.0);
                    ui.heading("FullNode");
                    ui.add_space(20.0);
                });
                ui.horizontal_wrapped(|ui| {
                    ui.add_enabled_ui(true, |ui| {
                        ui.add_space(10.0);
                        ui.heading("Synced");
                        ui.label(format!("{synced}"));
                        ui.add_space(20.0);
                    });
                    ui.add_enabled_ui(true, |ui| {
                        ui.add_space(10.0);
                        ui.heading("Height");
                        ui.label(format!("{height}"));
                        ui.add_space(20.0);
                    });
                    ui.add_enabled_ui(true, |ui| {
                        ui.add_space(10.0);
                        let mut label = " Bytes";
                        if space > 1024 { //Convert to KB
                            space = space / 1024;
                            label = " Kib";
                        }
                        if space > 1024 { //Convert to MB
                            space = space / 1024;
                            label = " Mib";
                        }
                        let mut space = space as f64;
                        if space > 1024f64 { //Convert to GB
                            space = space / 1024f64;
                            label = " Gib";
                        }
                        if space > 1024f64 { //Convert to TB
                            space = space / 1024f64;
                            label = " Tib";
                        }
                        if space > 1024f64 { //Convert to PB
                            space = space / 1024f64;
                            label = " Pib";
                        }
                        if space > 1024f64 { //Convert to EB
                            space = space / 1024f64;
                            label = " Eib";
                        }
                        ui.heading("Space");
                        ui.label(format!("{space:.3} {label}"));
                        ui.add_space(20.0);
                    });
                    ui.add_enabled_ui(true, |ui| {
                        ui.add_space(10.0);
                        ui.heading("Difficulty");
                        ui.label(format!("{difficulty}"));
                        ui.add_space(20.0);
                    });
                });
            });
        });
    }
}
