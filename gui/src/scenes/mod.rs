use crate::app::DgXchGui;
use eframe::egui;

pub mod config;
pub mod farmer;
pub mod fullnode;
pub mod fullnode_overview;
pub mod wallet;
mod wallet_import_mnemonic;
mod wallet_overview;
mod wallet_transactions;

pub trait Scene {
    fn update(&mut self, gui: &mut DgXchGui, ctx: &egui::Context, frame: &mut eframe::Frame);
}
