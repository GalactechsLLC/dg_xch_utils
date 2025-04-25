use crate::blockchain::coin::Coin;
use crate::blockchain::coin_spend::CoinSpend;
use crate::blockchain::condition_opcode::ConditionOpcode;
use crate::blockchain::condition_with_args::ConditionWithArgs;
use crate::blockchain::sized_bytes::{Bytes32, Bytes48, Bytes96};
use crate::blockchain::utils::pkm_pairs_for_conditions_dict;
use crate::clvm::bls_bindings;
use crate::clvm::bls_bindings::{aggregate_verify_signature, verify_signature};
use crate::clvm::condition_utils::conditions_dict_for_solution;
use crate::clvm::program::Program;
use crate::clvm::utils::INFINITE_COST;
use crate::consensus::constants::{ConsensusConstants, MAINNET};
use crate::consensus::{AGG_SIG_COST, CREATE_COIN_COST};
use crate::traits::SizedBytes;
use crate::utils::hash_256;
use blst::min_pk::{AggregateSignature, PublicKey, SecretKey, Signature};
use dg_xch_macros::ChiaSerial;
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use num_traits::ToPrimitive;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::future::Future;
use std::hash::RandomState;
use std::io::{Error, ErrorKind};

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug, Default)]
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
        self.aggregated_signature = if sigs.len() == 1 {
            sig.to_bytes().into()
        } else {
            AggregateSignature::aggregate(&sigs.iter().collect::<Vec<&Signature>>(), true)
                .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("{e:?}")))?
                .to_signature()
                .into()
        };
        Ok(self)
    }

    pub async fn sign<F, Fut>(
        mut self,
        key_function: F,
        constants: Option<&ConsensusConstants>,
    ) -> Result<Self, Error>
    where
        F: Fn(&Bytes48) -> Fut,
        Fut: Future<Output = Result<SecretKey, Error>>,
    {
        let constants = constants.unwrap_or(&*MAINNET);
        let mut signatures: Vec<Signature> = vec![];
        let mut pk_list: Vec<Bytes48> = vec![];
        let mut msg_list: Vec<Vec<u8>> = vec![];
        let max_cost = constants
            .max_block_cost_clvm
            .to_u64()
            .ok_or(Error::new(ErrorKind::InvalidInput, "Invalid Max Cost"))?;
        for coin_spend in self.coin_spends.iter() {
            //Get AGG_SIG conditions
            let conditions_dict = conditions_dict_for_solution::<RandomState>(
                &coin_spend.puzzle_reveal,
                &coin_spend.solution,
                max_cost,
            )?
            .0;
            //Create signature
            for (pk_bytes, msg) in pkm_pairs_for_conditions_dict(
                &conditions_dict,
                coin_spend.coin,
                &constants.agg_sig_me_additional_data,
            )? {
                let pk = PublicKey::from_bytes(pk_bytes.as_ref()).map_err(|e| {
                    Error::new(
                        ErrorKind::Other,
                        format!(
                            "Failed to parse Public key: {}, {:?}",
                            hex::encode(pk_bytes),
                            e
                        ),
                    )
                })?;
                let secret_key = (key_function)(&pk_bytes).await?;
                assert_eq!(&secret_key.sk_to_pk(), &pk);
                let signature = bls_bindings::sign(&secret_key, msg.as_ref());
                assert!(verify_signature(&pk, msg.as_ref(), &signature));
                pk_list.push(pk_bytes);
                msg_list.push(msg.as_ref().to_vec());
                signatures.push(signature);
            }
        }
        //Aggregate signatures
        let sig_refs: Vec<&Signature> = signatures.iter().collect();
        let msg_list: Vec<&[u8]> = msg_list.iter().map(Vec::as_slice).collect();
        let aggsig = AggregateSignature::aggregate(&sig_refs, true)
            .map_err(|e| {
                Error::new(
                    ErrorKind::Other,
                    format!("Failed to aggregate signatures: {e:?}"),
                )
            })?
            .to_signature();
        assert!(aggregate_verify_signature(&pk_list, &msg_list, &aggsig));
        self.aggregated_signature = aggsig.to_bytes().into();
        Ok(self)
    }
    pub fn validate(
        &self,
        max_cost: Option<u64>,
        print: bool,
    ) -> Result<Vec<ConditionWithArgs>, Error> {
        let mut max_cost = max_cost.unwrap_or(INFINITE_COST);
        let mut _total_cost = 0;
        let mut spent_coins = HashSet::<Bytes32>::new();
        let mut coins_to_create = vec![];
        let mut create_conditions = vec![];
        let mut coins_to_spend = vec![];
        let mut output_conditions = vec![];
        for spend in &self.coin_spends {
            let (cost, output_conditions_program) =
                spend
                    .puzzle_reveal
                    .run(INFINITE_COST, 2, &spend.solution.to_program())?;
            _total_cost += cost;
            let coin_id = spend.coin.coin_id();
            if !spent_coins.insert(coin_id) {
                //Double Spend
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    format!("Duplicate Spend: {}", coin_id),
                ));
            }
            let conditions_with_args: Vec<ConditionWithArgs> =
                (&output_conditions_program.sexp).try_into()?;
            for condition_with_args in &conditions_with_args {
                if print {
                    log::info!("{condition_with_args}");
                }
                //Check Costs
                match (*condition_with_args).op_code() {
                    ConditionOpcode::CreateCoin => {
                        if max_cost < CREATE_COIN_COST {
                            return Err(Error::new(ErrorKind::InvalidInput, "Max Cost Exceeded"));
                        }
                        max_cost -= CREATE_COIN_COST;
                        coins_to_create.push(spend.coin);
                        create_conditions.push(*condition_with_args);
                    }
                    ConditionOpcode::AggSigParent
                    | ConditionOpcode::AggSigPuzzle
                    | ConditionOpcode::AggSigAmount
                    | ConditionOpcode::AggSigPuzzleAmount
                    | ConditionOpcode::AggSigParentAmount
                    | ConditionOpcode::AggSigParentPuzzle
                    | ConditionOpcode::AggSigUnsafe
                    | ConditionOpcode::AggSigMe => {
                        if max_cost < AGG_SIG_COST {
                            return Err(Error::new(ErrorKind::InvalidInput, "Max Cost Exceeded"));
                        }
                        max_cost -= AGG_SIG_COST;
                    }
                    _ => {}
                }
            }
            output_conditions.extend(conditions_with_args);
            coins_to_spend.push(spend.coin);
        }
        Ok(output_conditions)
    }
}
