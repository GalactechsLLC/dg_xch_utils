use std::cmp::Ordering;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use crate::blockchain::coin::Coin;
use crate::blockchain::npc_result::NPCResult;
use crate::blockchain::spend_bundle::SpendBundle;
use serde::{Deserialize, Serialize};
use crate::blockchain::coin_spend::CoinSpend;
use crate::blockchain::sized_bytes::{Bytes32, SizedBytes};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct BundleCoinSpend {
    coin_spend: CoinSpend,
    eligible_for_dedup: bool,
    additions: Vec<Coin>,
    cost: Optional<u64>,
}

#[derive(PartialEq, Eq, Serialize, Deserialize, Debug, Clone)]
pub struct MempoolItem {
    pub spend_bundle: SpendBundle,
    pub fee: u64,
    pub npc_result: NPCResult,
    pub spend_bundle_name: Bytes32,
    pub height_added_to_mempool: u32,
    pub assert_height: Option<u32>,
    pub assert_before_height: Option<u32>,
    pub assert_before_seconds: Option<u64>,
    pub bundle_coin_spends: HashMap<Bytes32, BundleCoinSpend>,
}
impl MempoolItem {

    pub fn fee_per_cost(&self) -> f64 {
        self.fee / self.cost
    }
    pub fn name(&self) -> Bytes32 {
        self.spend_bundle_name.clone()
    }
    pub fn cost(&self) -> u64 {
        self.npc_result.cost
    }
    pub fn additions(self) -> Vec<Coin> {
        additions_for_npc(self.npc_result)
    }

    pub fn removals(self) -> Vec<Coin> {
        self.spend_bundle.removals()
    }
}

impl PartialOrd<Self> for MempoolItem {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        self.fee_per_cost().partial_cmp(*other.fee_per_cost())
    }
}

impl Ord for MempoolItem {
    fn cmp(&self, other: &Self) -> Ordering {
        self.fee_per_cost().total_cmp(*other.fee_per_cost())
    }
}
impl Hash for MempoolItem {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write(self.spend_bundle_name.as_slice())
    }
}
