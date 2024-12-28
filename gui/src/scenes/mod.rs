use eframe::egui;
use crate::app::DgXchGui;

pub mod wallet;
pub mod config;
pub mod fullnode;
pub mod farmer;
pub mod fullnode_overview;
mod wallet_overview;
mod wallet_import_mnemonic;
mod wallet_transactions;

pub trait Scene {
    fn update(&mut self, gui: &mut DgXchGui, ctx: &egui::Context, frame: &mut eframe::Frame);
}