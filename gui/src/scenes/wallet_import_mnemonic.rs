use crate::app::DgXchGui;
use crate::scenes::Scene;
use crate::state::WalletState;
use arboard::Clipboard;
use blst::min_pk::SecretKey;
use dg_xch_keys::{key_from_mnemonic_str, master_sk_to_wallet_sk};
use eframe::egui::Context;
use eframe::{egui, Frame};
use std::sync::Arc;

pub struct WalletImportMnemonicScene {
    mnemonic_words: Vec<String>,
    success_message: Option<String>,
    error_message: Option<String>,
    mnemonic_length: usize,
}

impl WalletImportMnemonicScene {
    pub fn new(_wallet_state: Arc<WalletState>) -> Self {
        WalletImportMnemonicScene {
            mnemonic_words: vec![String::new(); 12],
            success_message: None,
            error_message: None,
            mnemonic_length: 12,
        }
    }

    fn handle_paste(&mut self, pasted_text: &str) {
        // Split the pasted text into words
        let words: Vec<&str> = pasted_text
            .split_whitespace()
            .filter(|w| !w.is_empty())
            .collect();

        if words.len() != 12 && words.len() != 24 {
            self.error_message = Some(format!("Expected 12 or 24 words, but got {}", words.len()));
            return;
        }

        // Update mnemonic length and words
        self.mnemonic_length = words.len();
        self.mnemonic_words = words.iter().map(|s| s.to_string()).collect();

        self.error_message = None;
    }
}

impl Scene for WalletImportMnemonicScene {
    fn update(&mut self, gui: &mut DgXchGui, ctx: &Context, _frame: &mut Frame) {
        let mut pasted_texts = Vec::new();
        ctx.input(|i| {
            for event in &i.events {
                if let egui::Event::Paste(pasted_text) = event {
                    pasted_texts.push(pasted_text.clone());
                }
            }
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Import Mnemonic");
            ui.separator();

            if let Some(ref error) = self.error_message {
                ui.colored_label(egui::Color32::RED, error);
            }

            ui.label("Select mnemonic length:");

            // Allow the user to select between 12 and 24-word mnemonics
            ui.horizontal(|ui| {
                ui.radio_value(&mut self.mnemonic_length, 12, "12 words");
                ui.radio_value(&mut self.mnemonic_length, 24, "24 words");
            });

            // Adjust the mnemonic_words vector to match the selected length
            if self.mnemonic_words.len() != self.mnemonic_length {
                self.mnemonic_words
                    .resize(self.mnemonic_length, String::new());
            }

            ui.add_space(10.0);

            // "Paste Mnemonic" button to handle clipboard input
            if ui.button("Paste Mnemonic").clicked() {
                match Clipboard::new() {
                    Ok(mut clipboard) => match clipboard.get_text() {
                        Ok(pasted_text) => {
                            self.handle_paste(&pasted_text);
                        }
                        Err(err) => {
                            self.error_message =
                                Some(format!("Failed to read clipboard content: {}", err));
                        }
                    },
                    Err(err) => {
                        self.error_message = Some(format!("Failed to access clipboard: {}", err));
                    }
                }
            }

            ui.add_space(10.0);

            // Display input fields for mnemonic words
            for i in 0..self.mnemonic_words.len() {
                ui.horizontal(|ui| {
                    ui.label(format!("{}:", i + 1));
                    ui.text_edit_singleline(&mut self.mnemonic_words[i]);
                });
            }

            ui.add_space(20.0);

            if ui.button("Import Mnemonic").clicked() {
                let mnemonic = &self.mnemonic_words;
                match import_wallet_with_mnemonic(gui, mnemonic) {
                    Ok((master_sk, wallet_sk)) => {
                        self.success_message = Some(format!(
                            "Mnemonic imported successfully!\nMaster SK: {:?}\nWallet SK: {:?}",
                            master_sk, wallet_sk
                        ));
                        self.error_message = None;
                    }
                    Err(e) => {
                        self.error_message = Some(format!("Error: {}", e));
                        self.success_message = None;
                    }
                }
            }
            if let Some(ref error) = self.error_message {
                ui.colored_label(egui::Color32::RED, error);
            } else if let Some(ref message) = self.success_message {
                ui.colored_label(egui::Color32::GREEN, message);
            }
        });
    }
}

fn import_wallet_with_mnemonic(
    _gui: &mut DgXchGui,
    mnemonic_vec: &[String],
) -> Result<(SecretKey, SecretKey), String> {
    let word_count = mnemonic_vec.len();

    if word_count != 12 && word_count != 24 {
        return Err(format!(
            "Mnemonic must be exactly 12 or 24 words, not {}",
            word_count
        ));
    }

    let mnemonic_str = mnemonic_vec.join(" ");
    let mut master_sk = SecretKey::default();
    let mut wallet_sk = SecretKey::default();

    if let Ok(key) = key_from_mnemonic_str(&mnemonic_str) {
        master_sk = key;
    }

    if let Ok(key) = master_sk_to_wallet_sk(&master_sk, 0) {
        wallet_sk = key;
    }

    //should return memory wallet?
    Ok((master_sk, wallet_sk))
}
