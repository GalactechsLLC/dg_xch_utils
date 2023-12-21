use dg_xch_core::blockchain::block_record::BlockRecord;
use dg_xch_core::blockchain::blockchain_state::BlockchainState;
use dg_xch_core::blockchain::coin_record::{CoinRecord, HintedCoinRecord};
use dg_xch_core::blockchain::coin_spend::CoinSpend;
use dg_xch_core::blockchain::full_block::FullBlock;
use dg_xch_core::blockchain::mempool_item::MempoolItem;
use dg_xch_core::blockchain::signage_point::SignagePoint;
use dg_xch_core::blockchain::subslot_bundle::SubSlotBundle;
use dg_xch_core::blockchain::transaction_record::TransactionRecord;
use dg_xch_core::blockchain::tx_status::TXStatus;
use dg_xch_core::blockchain::unfinished_block::UnfinishedBlock;
use dg_xch_core::blockchain::wallet_balance::WalletBalance;
use dg_xch_core::blockchain::wallet_info::WalletInfo;
use dg_xch_core::protocols::full_node::BlockCountMetrics;
use dg_xch_core::protocols::full_node::FeeEstimate;

use dg_xch_core::blockchain::sized_bytes::Bytes32;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AdditionsAndRemovalsResp {
    pub additions: Vec<CoinRecord>,
    pub removals: Vec<CoinRecord>,
    pub success: bool,
}
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HintedAdditionsAndRemovalsResp {
    //non-standard
    pub additions: Vec<HintedCoinRecord>,
    pub removals: Vec<HintedCoinRecord>,
    pub success: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BlockchainStateResp {
    pub blockchain_state: BlockchainState,
    pub success: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BlockRecordResp {
    pub block_record: BlockRecord,
    pub success: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BlockRecordAryResp {
    pub block_records: Vec<BlockRecord>,
    pub success: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CoinRecordResp {
    pub coin_record: Option<CoinRecord>,
    pub success: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CoinRecordAryResp {
    pub coin_records: Vec<CoinRecord>,
    pub success: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CoinHintsResp {
    //non-standard
    pub coin_id_hints: HashMap<Bytes32, Bytes32>,
    pub success: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PaginatedCoinRecordAryResp {
    //non-standard
    pub coin_records: Vec<CoinRecord>,
    pub last_id: Option<Bytes32>,
    pub total_coin_count: Option<i32>,
    pub success: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CoinSpendResp {
    pub coin_solution: CoinSpend,
    pub success: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CoinSpendMapResp {
    //non-standard
    pub coin_solutions: HashMap<Bytes32, Option<CoinSpend>>,
    pub success: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct FeeEstimateResp {
    pub fee_estimate: FeeEstimate,
    pub success: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct FullBlockResp {
    pub block: FullBlock,
    pub success: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockCountMetricsResp {
    pub metrics: BlockCountMetrics,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct FullBlockAryResp {
    pub blocks: Vec<FullBlock>,
    pub success: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct InitialFreezePeriodResp {
    pub initial_freeze_end_timestamp: u64,
    pub success: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LoginResp {
    pub fingerprint: u32,
    pub success: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MempoolItemResp {
    pub mempool_item: MempoolItem,
    pub success: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MempoolItemAryResp {
    pub mempool_items: Vec<MempoolItem>,
    pub success: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MempoolItemsResp {
    pub mempool_items: HashMap<String, MempoolItem>,
    pub success: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MempoolTXResp {
    pub tx_ids: Vec<String>,
    pub success: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct NetworkInfoResp {
    pub network_name: String,
    pub network_prefix: String,
    pub success: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct NetSpaceResp {
    pub space: u64,
    pub success: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SignagePointOrEOSResp {
    pub signage_point: Option<SignagePoint>,
    pub eos: Option<SubSlotBundle>,
    pub time_received: f64,
    pub reverted: bool,
    pub success: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SignedTransactionRecordResp {
    pub signed_tx: TransactionRecord,
    pub success: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TXResp {
    pub status: TXStatus,
    pub success: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TransactionRecordResp {
    pub transaction: TransactionRecord,
    pub success: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct UnfinishedBlockAryResp {
    pub headers: Vec<UnfinishedBlock>,
    pub success: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WalletBalanceResp {
    pub wallets: Vec<WalletBalance>,
    pub success: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WalletInfoResp {
    pub wallets: Vec<WalletInfo>,
    pub success: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WalletSyncResp {
    pub genesis_initialized: bool,
    pub synced: bool,
    pub syncing: bool,
    pub success: bool,
}
