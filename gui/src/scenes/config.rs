use eframe::egui;
use crate::app::DgXchGui;
use crate::scenes::Scene;
use std::path::Path;
use crate::config::Config;

pub struct ConfigScene {
    last_message: Option<String>,
    message_time: f64, // Time when the message was set
}

impl ConfigScene {
    pub fn new() -> Self {
        ConfigScene {
            last_message: None,
            message_time: 0.0,
        }
    }
}

impl Scene for ConfigScene {
    fn update(&mut self, gui: &mut DgXchGui, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            // Header
            ui.vertical_centered(|ui| {
                ui.add_space(20.0);
                ui.heading("Config");
                ui.add_space(20.0);
            });

            // Main Config Enabled Checkbox
            ui.checkbox(&mut gui.config.enabled, "Enabled");

            ui.separator();

            // FullNodeConfig Section
            ui.collapsing("Full Node Config", |ui| {
                ui.checkbox(&mut gui.config.full_node_config.enabled, "Enabled");
                ui.horizontal(|ui| {
                    ui.label("Hostname:");
                    ui.text_edit_singleline(&mut gui.config.full_node_config.full_node_hostname);
                });
                ui.horizontal(|ui| {
                    ui.label("WS Port:");
                    ui.add(egui::DragValue::new(&mut gui.config.full_node_config.full_node_ws_port));
                });
                ui.horizontal(|ui| {
                    ui.label("RPC Port:");
                    ui.add(egui::DragValue::new(&mut gui.config.full_node_config.full_node_rpc_port));
                });
                ui.horizontal(|ui| {
                    ui.label("SSL:");
                    if let Some(ref mut ssl) = &mut gui.config.full_node_config.full_node_ssl {
                        ui.text_edit_singleline(ssl);
                        if ui.button("Clear SSL").clicked() {
                            gui.config.full_node_config.full_node_ssl = None;
                        }
                    } else if ui.button("Set SSL").clicked() {
                        gui.config.full_node_config.full_node_ssl = Some(String::new());
                    }
                });
            });

            // WalletConfig Section
            ui.collapsing("Wallet Config", |ui| {
                ui.checkbox(&mut gui.config.wallet_config.enabled, "Enabled");
                ui.horizontal(|ui| {
                    ui.label("Hostname:");
                    ui.text_edit_singleline(&mut gui.config.wallet_config.full_node_hostname);
                });
                ui.horizontal(|ui| {
                    ui.label("RPC Port:");
                    ui.add(egui::DragValue::new(&mut gui.config.wallet_config.full_node_rpc_port));
                });
                ui.horizontal(|ui| {
                    ui.label("SSL:");
                    if let Some(ref mut ssl) = &mut gui.config.wallet_config.full_node_ssl {
                        ui.text_edit_singleline(ssl);
                        if ui.button("Clear SSL").clicked() {
                            gui.config.wallet_config.full_node_ssl = None;
                        }
                    } else if ui.button("Set SSL").clicked() {
                        gui.config.wallet_config.full_node_ssl = Some(String::new());
                    }
                });
            });

            // SimulatorConfig Section
            ui.collapsing("Simulator Config", |ui| {
                ui.checkbox(&mut gui.config.simulator_config.enabled, "Enabled");
                ui.horizontal(|ui| {
                    ui.label("Hostname:");
                    ui.text_edit_singleline(&mut gui.config.simulator_config.full_node_hostname);
                });
                ui.horizontal(|ui| {
                    ui.label("RPC Port:");
                    ui.add(egui::DragValue::new(&mut gui.config.simulator_config.full_node_rpc_port));
                });
            });

            // FarmerConfig Section
            ui.collapsing("Farmer Config", |ui| {
                ui.checkbox(&mut gui.config.farmer_config.enabled, "Enabled");
                ui.horizontal(|ui| {
                    ui.label("Hostname:");
                    ui.text_edit_singleline(&mut gui.config.farmer_config.full_node_hostname);
                });
                ui.horizontal(|ui| {
                    ui.label("RPC Port:");
                    ui.add(egui::DragValue::new(&mut gui.config.farmer_config.full_node_rpc_port));
                });
                ui.horizontal(|ui| {
                    ui.label("SSL:");
                    if let Some(ref mut ssl) = &mut gui.config.farmer_config.full_node_ssl {
                        ui.text_edit_singleline(ssl);
                        if ui.button("Clear SSL").clicked() {
                            gui.config.farmer_config.full_node_ssl = None;
                        }
                    } else if ui.button("Set SSL").clicked() {
                        gui.config.farmer_config.full_node_ssl = Some(String::new());
                    }
                });
            });

            ui.separator();

            // Save and Load Buttons
            ui.horizontal(|ui| {
                if ui.button("Save Config").clicked() {
                    match gui.config.save_as_yaml("config.yaml") {
                        Ok(_) => {
                            self.last_message = Some("Config saved successfully.".to_string());
                            self.message_time = ctx.input(|i| i.time);
                        }
                        Err(e) => {
                            self.last_message = Some(format!("Error saving config: {:?}", e));
                            self.message_time = ctx.input(|i| i.time);
                        }
                    }
                }

                if ui.button("Load Config").clicked() {
                    match Config::try_from(Path::new("config.yaml")) {
                        Ok(config) => {
                            gui.config = config;
                            self.last_message = Some("Config loaded successfully.".to_string());
                            self.message_time = ctx.input(|i| i.time);
                        }
                        Err(e) => {
                            self.last_message = Some(format!("Error loading config: {:?}", e));
                            self.message_time = ctx.input(|i| i.time);
                        }
                    }
                }
            });

            // Display the last message if it's been less than 5 seconds
            if let Some(ref message) = self.last_message {
                let elapsed = ctx.input(|i| i.time) - self.message_time;
                if elapsed < 5.0 {
                    ui.colored_label(egui::Color32::GREEN, message);
                } else {
                    // Clear the message after 5 seconds
                    self.last_message = None;
                }
            }
        });
    }
}
