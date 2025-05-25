use dg_xch_clients::rpc::full_node::FullnodeClient;
use dg_xch_core::blockchain::blockchain_state::BlockchainState;
use eframe::egui::mutex::Mutex;
use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Debug, Hash)]
pub enum SelectedTab {
    Farmer,
    Wallet,
    FullNode,
    Config,
}
#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Debug, Hash)]
pub enum FullNodeTab {
    Overview,
    Coins,
    Blocks,
}
#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Debug, Hash)]
pub enum WalletTab {
    Overview,
    Mnemonic,
    Transactions,
}
pub struct State {
    pub selected_tab: SelectedTab,
    pub full_node_client: Arc<FullnodeClient>,
    pub shutdown_signal: Arc<AtomicBool>,
}
#[derive(Default)]
pub struct FullNodeState {
    pub blockchain_state: Mutex<Option<BlockchainState>>,
}

#[derive(Default)]
pub struct WalletState {
    pub confirmed_balance: u128,
    pub unconfirmed_balance: u128,
    pub balance_history: HashMap<u128, u128>,
}
