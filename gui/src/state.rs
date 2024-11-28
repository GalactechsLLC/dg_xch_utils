use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use eframe::egui::mutex::Mutex;
use dg_xch_clients::rpc::full_node::FullnodeClient;
use dg_xch_core::blockchain::blockchain_state::BlockchainState;
use dg_xch_core::blockchain::network_info::NetworkInfo;

#[derive(Default)]
pub struct FullNodeState {
    pub blockchain_state: Mutex<Option<BlockchainState>>,
    pub network_info: Mutex<Option<NetworkInfo>>,
}

#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Debug, Hash)]
pub enum SelectedTab {
    Farmer,
    Wallet,
    FullNode,
    Config
}

#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Debug, Hash)]
pub enum FullNodeTab {
    Overview,
    Coins,
    Blocks,
}

pub struct State {
    pub selected_tab: SelectedTab,
    pub full_node_client: Arc<FullnodeClient>,
    pub farmer_client: Arc<FullnodeClient>,
    pub wallet_client: Arc<FullnodeClient>,
    pub shutdown_signal: Arc<AtomicBool>
}