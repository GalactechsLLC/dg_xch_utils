use crate::blockchain::coin::Coin;
use crate::blockchain::coin_spend::CoinSpend;
use crate::blockchain::condition_opcode::ConditionOpcode;
use crate::blockchain::condition_with_args::ConditionWithArgs;
use crate::blockchain::sized_bytes::{Bytes32, Bytes96};
use crate::clvm::program::Program;
use crate::clvm::sexp::AtomBuf;
use crate::clvm::utils::INFINITE_COST;
use crate::traits::SizedBytes;
use crate::utils::hash_256;
use blst::min_pk::{AggregateSignature, Signature};
use dg_xch_macros::ChiaSerial;
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::io::{Error, ErrorKind};

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct SpendBundle {
    pub coin_spends: Vec<CoinSpend>,
    pub aggregated_signature: Bytes96,
}
impl SpendBundle {
    #[must_use]
    pub fn name(&self) -> Bytes32 {
        hash_256(self.to_bytes(ChiaProtocolVersion::default())).into()
    }
    pub fn aggregate(bundles: Vec<SpendBundle>) -> Result<Self, Error> {
        let mut coin_spends = vec![];
        let mut signatures = vec![];
        for bundle in bundles {
            coin_spends.extend(bundle.coin_spends);
            signatures.push(bundle.aggregated_signature.try_into()?);
        }
        let aggregated_signature = if signatures.is_empty() {
            Bytes96::default()
        } else {
            AggregateSignature::aggregate(&signatures.iter().collect::<Vec<&Signature>>(), true)
                .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("{e:?}")))?
                .to_signature()
                .into()
        };
        Ok(SpendBundle {
            coin_spends,
            aggregated_signature,
        })
    }

    #[must_use]
    pub fn empty() -> Self {
        SpendBundle {
            coin_spends: vec![],
            aggregated_signature: Bytes96::default(),
        }
    }

    pub fn output_conditions(&self) -> Result<Vec<Program>, Error> {
        let mut conditions = vec![];
        for spend in &self.coin_spends {
            let (_, output) = spend
                .puzzle_reveal
                .run_with_cost(u64::MAX, &spend.solution.to_program())?;
            conditions.extend(output.as_list());
        }
        Ok(conditions)
    }

    pub fn additions(&self) -> Result<Vec<Coin>, Error> {
        self.coin_spends.iter().try_fold(vec![], |mut prev, cur| {
            prev.extend(cur.additions()?);
            Ok(prev)
        })
    }

    #[must_use]
    pub fn removals(&self) -> Vec<Coin> {
        self.coin_spends.iter().map(|c| &c.coin).copied().collect()
    }

    #[must_use]
    pub fn coins(&self) -> Vec<Coin> {
        self.removals()
    }

    pub fn net_additions(&self) -> Result<Vec<Coin>, Error> {
        let removals: HashSet<Bytes32> = self.removals().into_iter().map(|c| c.name()).collect();
        Ok(self
            .additions()?
            .into_iter()
            .filter(|a| !removals.contains(&a.name()))
            .collect())
    }

    pub fn add_signature(mut self, sig: Signature) -> Result<Self, Error> {
        let mut sigs: Vec<Signature> = vec![sig];
        if !self.aggregated_signature.is_null() {
            sigs.push((&self.aggregated_signature).try_into()?);
        }
        self.aggregated_signature = if sigs.is_empty() {
            Bytes96::default()
        } else {
            AggregateSignature::aggregate(&sigs.iter().collect::<Vec<&Signature>>(), true)
                .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("{e:?}")))?
                .to_signature()
                .into()
        };
        Ok(self)
    }

    pub fn sign<T: Fn(&CoinSpend) -> Result<Signature, Error>>(
        mut self,
        sig_function: T,
    ) -> Result<Self, Error> {
        let mut sigs: Vec<Signature> = vec![];
        for spend in &self.coin_spends {
            sigs.push((sig_function)(spend)?);
        }
        self.aggregated_signature =
            AggregateSignature::aggregate(&sigs.iter().collect::<Vec<&Signature>>(), true)
                .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("{e:?}")))?
                .to_signature()
                .into();
        Ok(self)
    }
    pub fn validate(&self) -> Result<bool, Error> {
        //Validate Signature

        //Validate Spends
        let mut coins_to_create = vec![];
        let mut coins_to_spend = vec![];
        let mut origin_id = None;
        for spend in &self.coin_spends {
            let (_cost, output_conditions) =
                spend
                    .puzzle_reveal
                    .run(INFINITE_COST, 2, &spend.solution.to_program())?;
            let conditions_with_args: Vec<ConditionWithArgs> =
                (&output_conditions.sexp).try_into()?;
            let mut create_conditions = vec![];
            for condition_with_args in conditions_with_args {
                if condition_with_args.opcode == ConditionOpcode::CreateCoin {
                    coins_to_create.push(Coin {
                        parent_coin_info: spend.coin.coin_id(),
                        puzzle_hash: AtomBuf::from(condition_with_args.vars[0].clone())
                            .as_bytes32()?,
                        amount: AtomBuf::from(condition_with_args.vars[1].clone()).as_u64()?,
                    });
                    create_conditions.push(condition_with_args);
                }
            }
            coins_to_spend.push(spend.coin);
            if !create_conditions.is_empty() {
                match origin_id {
                    Some(_) => Err(Error::new(
                        ErrorKind::InvalidInput,
                        "Cannot have multiple Origin coins",
                    ))?,
                    None => origin_id = Some(spend.coin.coin_id()),
                }
            }
        }

        //Validate Coins Created

        //Validate Conditions

        todo!()
    }
}
