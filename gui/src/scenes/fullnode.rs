use std::sync::{Arc, OnceLock};
use std::sync::atomic::Ordering;
use eframe::egui;
use eframe::egui::mutex::Mutex;
use log::error;
use dg_xch_clients::api::full_node::FullnodeAPI;
use crate::app::DgXchGui;
use crate::scenes::fullnode_overview::FullNodeOverviewScene;
use crate::scenes::Scene;
use crate::state::{FullNodeState, FullNodeTab};

static OVERVIEW: OnceLock<Mutex<FullNodeOverviewScene>> = OnceLock::new();
static COINS: OnceLock<Mutex<FullNodeOverviewScene>> = OnceLock::new();
static BLOCKS: OnceLock<Mutex<FullNodeOverviewScene>> = OnceLock::new();

pub struct FullNodeScene {
    pub shared_state: Arc<FullNodeState>,
    pub tabs: [(String, FullNodeTab); 3],
    pub selected_tab: FullNodeTab,
}
impl FullNodeScene {
    pub fn new(gui: &DgXchGui) -> Self {
        let shared_state = Arc::new(FullNodeState::default());
        let background_state = shared_state.clone();
        let client = gui.state.full_node_client.clone();
        let shutdown_signal = gui.state.shutdown_signal.clone();
        tokio::spawn(async move {
            while shutdown_signal.load(Ordering::SeqCst) {
                match client.get_blockchain_state().await {
                    Ok(state) => {
                        *background_state.blockchain_state.lock() = Some(state);
                    }
                    Err(e) => {
                        error!("Error getting blockchain state: {}", e);
                    }
                }
                tokio::time::sleep(std::time::Duration::from_secs(4)).await;
            }
        });
        FullNodeScene {
            shared_state,
            tabs: [
                (String::from("Overview"), FullNodeTab::Overview),
                (String::from("Coins"), FullNodeTab::Coins),
                (String::from("Blocks"), FullNodeTab::Blocks),
            ],
            selected_tab: FullNodeTab::Overview
        }
    }
}

impl Scene for FullNodeScene {
    fn update(&mut self, gui: &mut DgXchGui, ctx: &egui::Context, frame: &mut eframe::Frame) {
        egui::SidePanel::left("fullnode_nav").resizable(false).show(ctx, |ui| {
            ui.vertical(|ui| {
                for (label, tab) in &self.tabs {
                    if ui.selectable_label(self.selected_tab == *tab, label).clicked() {
                        self.selected_tab = *tab;
                    }
                }
            });
        });
        let state = self.shared_state.clone();
        match self.selected_tab {
            FullNodeTab::Overview => {
                OVERVIEW.get_or_init(|| {
                    Mutex::new(FullNodeOverviewScene::new(state))
                }).lock().update(gui, ctx, frame);
            }
            FullNodeTab::Coins => {
                COINS.get_or_init(|| {
                    Mutex::new(FullNodeOverviewScene::new(state))
                }).lock().update(gui, ctx, frame);
            }
            FullNodeTab::Blocks => {
                BLOCKS.get_or_init(|| {
                    Mutex::new(FullNodeOverviewScene::new(state))
                }).lock().update(gui, ctx, frame);
            }
        }
    }
}
