use dg_xch_core::blockchain::coin::Coin;
use dg_xch_core::blockchain::coin_record::CoinRecord;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::blockchain::spend_bundle::SpendBundle;
use dg_xch_core::consensus::block_generator::{
    CoinSpendContext, ConditionValidationContext, additions_for_conditions,
    conditions_from_spend_bundle, validate_block_aggregate_signature, validate_block_conditions,
};
use dg_xch_core::consensus::constants::{ConsensusConstants, TESTNET_11};
use dg_xch_core::utils::hash_256;
use std::collections::{HashMap, HashSet};
use std::io::Error;

/// An in-memory coin ledger using DG's consensus condition and signature validation.
#[derive(Clone)]
pub struct CoinsetSimulator {
    pub constants: ConsensusConstants,
    pub height: u32,
    pub timestamp: u64,
    coins: HashMap<Bytes32, CoinRecord>,
    next_coin: u64,
}

impl Default for CoinsetSimulator {
    fn default() -> Self {
        Self::new()
    }
}

impl CoinsetSimulator {
    pub fn new() -> Self {
        Self {
            constants: ConsensusConstants {
                simulated: true,
                ..TESTNET_11
            },
            height: 1,
            timestamp: 1,
            coins: HashMap::new(),
            next_coin: 0,
        }
    }

    pub fn new_coin(&mut self, puzzle_hash: Bytes32, amount: u64) -> Coin {
        let coin = Coin {
            parent_coin_info: hash_256(self.next_coin.to_be_bytes()).into(),
            puzzle_hash,
            amount,
        };
        self.next_coin += 1;
        self.insert_coin(coin);
        coin
    }

    pub fn insert_coin(&mut self, coin: Coin) {
        self.coins.entry(coin.name()).or_insert(CoinRecord {
            coin,
            confirmed_block_index: self.height,
            spent_block_index: 0,
            coinbase: false,
            timestamp: self.timestamp,
            spent: false,
        });
    }

    pub fn coin_record(&self, id: Bytes32) -> Option<&CoinRecord> {
        self.coins.get(&id)
    }

    pub fn new_transaction(&mut self, bundle: SpendBundle) -> Result<Vec<Coin>, Error> {
        let conditions = conditions_from_spend_bundle(&bundle, self.height, &self.constants)
            .map_err(|e| Error::other(format!("{e:?}")))?;
        let fee = conditions
            .removal_amount
            .checked_sub(conditions.addition_amount)
            .ok_or_else(|| Error::other("spend creates more value than it removes"))?;
        if fee < u128::from(conditions.reserve_fee) {
            return Err(Error::other("reserved fee exceeds paid fee"));
        }
        let additions = additions_for_conditions(&conditions, &[]);
        let created: HashMap<_, _> = additions.iter().map(|coin| (coin.name(), *coin)).collect();
        let mut context = ConditionValidationContext {
            block_height: self.height,
            previous_transaction_block_timestamp: Some(self.timestamp),
            ..Default::default()
        };
        let mut removals = HashSet::new();
        for spend in &bundle.coin_spends {
            let id = spend.coin.name();
            if !removals.insert(id) {
                return Err(Error::other("duplicate removal"));
            }
            let (height, timestamp) = if let Some(record) = self.coins.get(&id) {
                if record.spent || record.coin != spend.coin {
                    return Err(Error::other("coin already spent or mismatched"));
                }
                (record.confirmed_block_index, record.timestamp)
            } else if created.get(&id) == Some(&spend.coin) {
                (self.height, self.timestamp)
            } else {
                return Err(Error::other("unknown removal"));
            };
            context.coin_context.insert(
                id,
                CoinSpendContext {
                    birth_height: Some(height),
                    birth_seconds: Some(timestamp),
                    spent_height: Some(self.height),
                    spent_seconds: Some(self.timestamp),
                },
            );
        }
        if additions
            .iter()
            .any(|coin| self.coins.contains_key(&coin.name()))
        {
            return Err(Error::other("coin already exists"));
        }
        validate_block_conditions(&conditions, &context)
            .map_err(|e| Error::other(format!("{e:?}")))?;
        validate_block_aggregate_signature(
            &conditions,
            &bundle.aggregated_signature,
            &self.constants,
        )
        .map_err(|e| Error::other(format!("{e:?}")))?;
        for coin in &additions {
            self.insert_coin(*coin);
        }
        for id in removals {
            let record = self
                .coins
                .get_mut(&id)
                .ok_or_else(|| Error::other("missing validated coin"))?;
            record.spent = true;
            record.spent_block_index = self.height;
        }
        Ok(additions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dg_xch_core::blockchain::coin_spend::CoinSpend;
    use dg_xch_core::clvm::assemble::assemble_text;
    use dg_xch_core::clvm::program::Program;

    #[test]
    fn rejected_spends_leave_the_ledger_unchanged() {
        let mut simulator = CoinsetSimulator::new();
        let puzzle = Program::to(1);
        let coin = simulator.new_coin(puzzle.tree_hash(), 10);
        let bundle = |solution: &str| SpendBundle {
            coin_spends: vec![CoinSpend {
                coin,
                puzzle_reveal: puzzle.serialized().unwrap(),
                solution: assemble_text(solution).unwrap().serialized().unwrap(),
            }],
            ..SpendBundle::default()
        };
        for bad in [
            "((51 1 11))",
            "((52 11))",
            "((61 0x00))",
            "((83 2))",
            "((51 1 5) (51 1 5))",
        ] {
            assert!(simulator.new_transaction(bundle(bad)).is_err(), "{bad}");
            assert!(!simulator.coin_record(coin.name()).unwrap().spent);
        }
        let valid = bundle("((51 1 9) (52 1))");
        let additions = simulator.new_transaction(valid.clone()).unwrap();
        assert_eq!(additions.len(), 1);
        assert!(simulator.coin_record(coin.name()).unwrap().spent);
        assert!(simulator.new_transaction(valid).is_err());
        let mut unknown = bundle("()");
        unknown.coin_spends[0].coin.parent_coin_info = [99; 32].into();
        assert!(simulator.new_transaction(unknown).is_err());
    }
}
