use crate::blockchain::coin::Coin;
use crate::blockchain::spend_bundle_conditions::SpendBundleConditions;
use serde::{Deserialize, Serialize};

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug, Default)]
pub struct NPCResult {
    pub error: Option<u16>,
    pub conds: Option<SpendBundleConditions>,
}
impl NPCResult {
    #[must_use]
    pub fn additions(self) -> Vec<Coin> {
        let mut additions: Vec<Coin> = vec![];
        if let Some(conds) = self.conds {
            for spend in conds.spends {
                for coin in spend.create_coin {
                    additions.push(Coin {
                        parent_coin_info: spend.coin_id,
                        puzzle_hash: coin.puzzle_hash,
                        amount: coin.amount,
                    });
                }
            }
        }
        additions
    }
}
