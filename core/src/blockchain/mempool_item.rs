use crate::blockchain::coin::Coin;
use crate::blockchain::coin_spend::CoinSpend;
use crate::blockchain::npc_result::NPCResult;
use crate::blockchain::sized_bytes::{Bytes32, SizedBytes};
use crate::blockchain::spend_bundle::SpendBundle;
use crate::clvm::utils::additions_for_npc;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

#[derive(PartialEq, Eq, Serialize, Deserialize, Debug, Clone)]
pub struct BundleCoinSpend {
    pub coin_spend: CoinSpend,
    pub eligible_for_dedup: bool,
    pub additions: Vec<Coin>,
    pub cost: Option<u64>,
}

#[derive(PartialEq, Eq, Serialize, Deserialize, Debug, Clone, Default)]
pub struct MempoolItem {
    pub spend_bundle: SpendBundle,
    pub fee: u64,
    pub cost: u64,
    pub npc_result: NPCResult,
    pub spend_bundle_name: Bytes32,
    #[serde(skip)]
    pub height_added_to_mempool: u32,
    #[serde(skip)]
    pub assert_height: Option<u32>,
    #[serde(skip)]
    pub assert_before_height: Option<u32>,
    #[serde(skip)]
    pub assert_before_seconds: Option<u64>,
    #[serde(skip)]
    pub bundle_coin_spends: HashMap<Bytes32, BundleCoinSpend>,
    #[serde(default)]
    pub additions: Vec<Coin>,
    #[serde(default)]
    pub removals: Vec<Coin>,
}
impl MempoolItem {
    #[allow(clippy::cast_precision_loss)]
    #[must_use]
    pub fn fee_per_cost(&self) -> f64 {
        (self.fee / self.cost()) as f64
    }
    #[must_use]
    pub fn name(&self) -> Bytes32 {
        self.spend_bundle_name
    }
    #[must_use]
    pub fn cost(&self) -> u64 {
        self.npc_result
            .conds
            .as_ref()
            .map(|c| c.cost)
            .unwrap_or_default()
    }
    #[must_use]
    pub fn additions(self) -> Vec<Coin> {
        additions_for_npc(self.npc_result)
    }

    #[must_use]
    pub fn removals(self) -> Vec<Coin> {
        self.spend_bundle.removals()
    }
}

impl PartialOrd<Self> for MempoolItem {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for MempoolItem {
    fn cmp(&self, other: &Self) -> Ordering {
        self.fee_per_cost().total_cmp(&other.fee_per_cost())
    }
}
impl Hash for MempoolItem {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write(self.spend_bundle_name.as_slice());
    }
}
