use crate::components::top_bar::TabBar;
use crate::config::Config;
use crate::scenes::config::ConfigScene;
use crate::scenes::farmer::FarmerScene;
use crate::scenes::fullnode::FullNodeScene;
use crate::scenes::wallet::WalletScene;
use crate::scenes::Scene;
use crate::state::{SelectedTab, State, WalletState};
use dg_xch_clients::rpc::full_node::FullnodeClient;
use dg_xch_clients::ClientSSLConfig;
use eframe::egui;
use eframe::egui::mutex::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, OnceLock};

static TAB_BAR: OnceLock<Mutex<TabBar>> = OnceLock::new();
static CONFIG_SCENE: OnceLock<Mutex<ConfigScene>> = OnceLock::new();
static FARMER_SCENE: OnceLock<Mutex<FarmerScene>> = OnceLock::new();
static WALLET_SCENE: OnceLock<Mutex<WalletScene>> = OnceLock::new();
static FULL_NODE_SCENE: OnceLock<Mutex<FullNodeScene>> = OnceLock::new();

pub struct DgXchGui {
    pub state: State,
    pub config: Config,
    pub wallet: Option<WalletState>,
    pub errors: Vec<String>,
}
impl DgXchGui {
    pub fn new(
        _cc: &eframe::CreationContext<'_>,
        config: Config,
        shutdown_signal: Arc<AtomicBool>,
    ) -> Self {
        Self {
            state: State {
                selected_tab: SelectedTab::FullNode,
                full_node_client: Arc::new(FullnodeClient::new(
                    &config.full_node_config.full_node_hostname,
                    config.full_node_config.full_node_rpc_port,
                    10,
                    config
                        .full_node_config
                        .full_node_ssl
                        .clone()
                        .map(|v| ClientSSLConfig {
                            ssl_crt_path: format!("{}/{}", v, "full_node/private_full_node.crt"),
                            ssl_key_path: format!("{}/{}", v, "full_node/private_full_node.crt"),
                            ssl_ca_crt_path: format!("{}/{}", v, "full_node/private_full_node.crt"),
                        }),
                    &None,
                )),
                farmer_client: Arc::new(FullnodeClient::new(
                    &config.farmer_config.full_node_hostname,
                    config.farmer_config.full_node_rpc_port,
                    10,
                    config
                        .farmer_config
                        .full_node_ssl
                        .clone()
                        .map(|v| ClientSSLConfig {
                            ssl_crt_path: format!("{}/{}", v, "full_node/private_full_node.crt"),
                            ssl_key_path: format!("{}/{}", v, "full_node/private_full_node.crt"),
                            ssl_ca_crt_path: format!("{}/{}", v, "full_node/private_full_node.crt"),
                        }),
                    &None,
                )),
                wallet_client: Arc::new(FullnodeClient::new(
                    &config.wallet_config.full_node_hostname,
                    config.wallet_config.full_node_rpc_port,
                    10,
                    config
                        .wallet_config
                        .full_node_ssl
                        .clone()
                        .map(|v| ClientSSLConfig {
                            ssl_crt_path: format!("{}/{}", v, "full_node/private_full_node.crt"),
                            ssl_key_path: format!("{}/{}", v, "full_node/private_full_node.crt"),
                            ssl_ca_crt_path: format!("{}/{}", v, "full_node/private_full_node.crt"),
                        }),
                    &None,
                )),
                shutdown_signal,
            },
            config,
            wallet: None,
            errors: vec![],
        }
    }
}

impl eframe::App for DgXchGui {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        TAB_BAR
            .get_or_init(|| Mutex::new(TabBar::new(self)))
            .lock()
            .update(self, ctx, frame);
        egui::CentralPanel::default().show(ctx, |_ui| match self.state.selected_tab {
            SelectedTab::Farmer => FARMER_SCENE
                .get_or_init(|| Mutex::new(FarmerScene::new()))
                .lock()
                .update(self, ctx, frame),
            SelectedTab::Wallet => WALLET_SCENE
                .get_or_init(|| Mutex::new(WalletScene::new(self)))
                .lock()
                .update(self, ctx, frame),
            SelectedTab::FullNode => FULL_NODE_SCENE
                .get_or_init(|| Mutex::new(FullNodeScene::new(self)))
                .lock()
                .update(self, ctx, frame),
            SelectedTab::Config => CONFIG_SCENE
                .get_or_init(|| Mutex::new(ConfigScene::new()))
                .lock()
                .update(self, ctx, frame),
        });
    }
}
