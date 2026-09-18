use crate::blockchain::coin::Coin;
use crate::blockchain::coin_spend::CoinSpend;
use crate::blockchain::condition_with_args::ConditionWithArgs;
#[cfg(feature = "bls")]
use crate::blockchain::condition_with_args::{Message, MessageArgs};
#[cfg(feature = "bls")]
use crate::blockchain::sized_bytes::Bytes48;
use crate::blockchain::sized_bytes::{Bytes32, Bytes96};
#[cfg(feature = "bls")]
use crate::blockchain::utils::{pkm_pairs_for_conditions, verify_agg_sig_unsafe_message};
#[cfg(feature = "bls")]
use crate::clvm::bls_bindings;
#[cfg(feature = "bls")]
use crate::clvm::bls_bindings::{aggregate_verify_signature, verify_signature};
#[cfg(feature = "bls")]
use crate::clvm::condition_utils::agg_sig_additional_data_for_opcode;
use crate::clvm::condition_utils::conditions_for_solution;
use crate::clvm::utils::INFINITE_COST;
#[cfg(feature = "bls")]
use crate::clvm::utils::{
    COST_CONDITIONS, DISABLE_SIGNATURE_VALIDATION, IGNORE_ASSERT_CONCURRENT_NULL, NO_UNKNOWN_OPS,
};
#[cfg(feature = "bls")]
use crate::consensus::constants::{ConsensusConstants, MAINNET};
#[cfg(feature = "bls")]
use crate::consensus::{AGG_SIG_COST, CREATE_COIN_COST};
#[cfg(feature = "bls")]
use crate::errors::ClvmError;
#[cfg(feature = "bls")]
use crate::formatting::u64_to_bytes;
#[cfg(feature = "bls")]
use crate::traits::SizedBytes;
use crate::utils::hash_256;
#[cfg(feature = "bls")]
use blst::min_pk::{AggregateSignature, PublicKey, SecretKey, Signature};
use dg_xch_macros::ChiaSerial;
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
#[cfg(feature = "bls")]
use log::{error, info};
#[cfg(feature = "bls")]
use num_traits::ToPrimitive;
use serde::{Deserialize, Serialize};
#[cfg(feature = "bls")]
use std::cmp::{max, min};
use std::collections::HashSet;
#[cfg(feature = "bls")]
use std::future::Future;
use std::io::Error;
#[cfg(feature = "bls")]
use std::io::ErrorKind;

#[cfg(feature = "bls")]
const ANNOUNCEMENT_LIMIT: u64 = 1024;

#[cfg(feature = "bls")]
#[derive(Default, Clone, Debug)]
struct ValidationState {
    pub coins_spent: HashSet<Coin>,
    pub coins_created: HashSet<Coin>,
    pub messages_sent: Vec<(u8, Message, MessageArgs, Coin)>,
    pub messages_received: Vec<(u8, Message, MessageArgs, Coin)>,
    pub puzzle_announcements: Vec<(Bytes32, Message)>,
    pub asserted_puzzle_announcements: Vec<Bytes32>,
    pub coin_announcements: Vec<(Bytes32, Message)>,
    pub asserted_coin_announcements: Vec<Bytes32>,
    pub asserted_concurrent_spend: Vec<Bytes32>,
    pub asserted_concurrent_puzzle: Vec<Bytes32>,
    pub agg_sig_me: Vec<(Bytes48, Message)>,
    pub agg_sig_parents: Vec<(Bytes48, Message)>,
    pub agg_sig_puzzles: Vec<(Bytes48, Message)>,
    pub agg_sig_amounts: Vec<(Bytes48, Message)>,
    pub agg_sig_puzzle_amounts: Vec<(Bytes48, Message)>,
    pub agg_sig_parent_amounts: Vec<(Bytes48, Message)>,
    pub agg_sig_parent_puzzles: Vec<(Bytes48, Message)>,
    pub agg_sig_unsafe: Vec<(Bytes48, Message)>,
    pub pkm_pairs: Vec<(Bytes48, Message)>,
    pub output_conditions: Vec<ConditionWithArgs>,
    pub total_announcements: u64,
    pub total_cost: u64,
    pub total_reserved_fee: u64,
    pub total_removed: u64,
    pub total_created: u64,
    pub seconds_relative: Option<u64>,
    pub seconds_absolute: u64,
    pub height_relative: Option<u32>,
    pub height_absolute: u32,
    pub before_seconds_relative: Option<u64>,
    pub before_seconds_absolute: Option<u64>,
    pub before_height_relative: Option<u32>,
    pub before_height_absolute: Option<u32>,
    pub birth_seconds: Option<u64>,
    pub birth_height: Option<u32>,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct SpendBundle {
    pub coin_spends: Vec<CoinSpend>,
    pub aggregated_signature: Bytes96,
}

impl Default for SpendBundle {
    fn default() -> Self {
        Self::empty()
    }
}

impl SpendBundle {
    pub fn name(&self) -> Result<Bytes32, Error> {
        Ok(hash_256(&self.to_bytes(ChiaProtocolVersion::default())?).into())
    }
    #[cfg(feature = "bls")]
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
        let mut sig = [0u8; 96];
        sig[0] = 0xc0; // compressed + infinity flag
        SpendBundle {
            coin_spends: vec![],
            aggregated_signature: sig.into(),
        }
    }

    pub fn output_conditions(&self) -> Result<Vec<ConditionWithArgs>, Error> {
        let mut conditions = vec![];
        for spend in &self.coin_spends {
            let reveal = spend.puzzle_reveal.to_program()?;
            let solution = spend.solution.to_program()?;
            conditions.extend(conditions_for_solution(&reveal, &solution, INFINITE_COST)?.0);
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

    #[cfg(feature = "bls")]
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

    #[cfg(feature = "bls")]
    pub async fn sign<F, Fut>(
        mut self,
        key_function: F,
        constants: Option<&ConsensusConstants>,
    ) -> Result<Self, ClvmError>
    where
        F: Fn(&Bytes48) -> Fut,
        Fut: Future<Output = Result<SecretKey, ClvmError>>,
    {
        let constants = constants.unwrap_or(&MAINNET);
        let mut signatures: Vec<Signature> = vec![];
        let mut pk_list: Vec<Bytes48> = vec![];
        let mut msg_list: Vec<Vec<u8>> = vec![];
        let max_cost = constants
            .max_block_cost_clvm
            .to_u64()
            .ok_or(ClvmError::AtomNotValidU64(format!(
                "Invalid Max Cost: {}",
                constants.max_block_cost_clvm
            )))?;
        for coin_spend in self.coin_spends.iter() {
            let reveal = coin_spend.puzzle_reveal.to_program()?;
            let solution = coin_spend.solution.to_program()?;
            //Get AGG_SIG conditions
            let conditions = conditions_for_solution(&reveal, &solution, max_cost)?.0;
            //Create signature
            for (code, pk_bytes, msg) in pkm_pairs_for_conditions(
                &conditions,
                coin_spend.coin,
                constants.agg_sig_me_additional_data.as_ref(),
            )? {
                let pk = PublicKey::from_bytes(pk_bytes.as_ref()).map_err(|e| {
                    error!("Failed to Parse PublicKey: {:?}", e);
                    ClvmError::InvalidPublicKey(pk_bytes)
                })?;
                let secret_key = (key_function)(&pk_bytes).await?;
                assert_eq!(&secret_key.sk_to_pk(), &pk);
                let signature = bls_bindings::sign(&secret_key, msg.as_ref());
                if !verify_signature(&pk, msg.as_ref(), &signature) {
                    Err(ClvmError::InvalidSignature(format!(
                        "PH({}) Failed to Validate Signature for Message: {} - {}",
                        pk_bytes, code, msg
                    )))?;
                }
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
                ClvmError::InvalidSignature(format!("Failed to aggregate signatures: {e:?}"))
            })?
            .to_signature();
        assert!(aggregate_verify_signature(&pk_list, &msg_list, &aggsig));
        self.aggregated_signature = aggsig.to_bytes().into();
        Ok(self)
    }

    #[cfg(feature = "bls")]
    pub async fn vault_sign<F, Fut>(
        mut self,
        sign_function: F,
        constants: Option<&ConsensusConstants>,
    ) -> Result<Self, ClvmError>
    where
        F: Fn(&Bytes48, &[u8]) -> Fut,
        Fut: Future<Output = Result<Signature, ClvmError>>,
    {
        let constants = constants.unwrap_or(&MAINNET);
        let mut signatures: Vec<Signature> = vec![];
        let mut pk_list: Vec<Bytes48> = vec![];
        let mut msg_list: Vec<Vec<u8>> = vec![];
        let max_cost = constants
            .max_block_cost_clvm
            .to_u64()
            .ok_or(ClvmError::AtomNotValidU64(format!(
                "Invalid Max Cost: {}",
                constants.max_block_cost_clvm
            )))?;
        for coin_spend in self.coin_spends.iter() {
            let reveal = coin_spend.puzzle_reveal.to_program()?;
            let solution = coin_spend.solution.to_program()?;
            //Get AGG_SIG conditions
            let conditions = conditions_for_solution(&reveal, &solution, max_cost)?.0;
            //Create signature
            for (code, pk_bytes, msg) in pkm_pairs_for_conditions(
                &conditions,
                coin_spend.coin,
                constants.agg_sig_me_additional_data.as_ref(),
            )? {
                let pk = PublicKey::from_bytes(pk_bytes.as_ref()).map_err(|e| {
                    error!("Failed to Parse PublicKey: {:?}", e);
                    ClvmError::InvalidPublicKey(pk_bytes)
                })?;
                let signature = (sign_function)(&pk_bytes, msg.as_ref()).await?;
                if !verify_signature(&pk, msg.as_ref(), &signature) {
                    Err(ClvmError::InvalidSignature(format!(
                        "PH({}) Failed to Validate Signature for Message: {} - {}",
                        pk_bytes, code, msg
                    )))?;
                }
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
                ClvmError::InvalidSignature(format!("Failed to aggregate signatures: {e:?}"))
            })?
            .to_signature();
        assert!(aggregate_verify_signature(&pk_list, &msg_list, &aggsig));
        self.aggregated_signature = aggsig.to_bytes().into();
        Ok(self)
    }
    #[cfg(feature = "bls")]
    pub fn validate(
        &self,
        max_cost: Option<u64>,
        flags: u32,
        consensus_constants: &ConsensusConstants,
        print: bool,
    ) -> Result<Vec<ConditionWithArgs>, ClvmError> {
        let mut max_cost = max_cost.unwrap_or(INFINITE_COST);
        let mut create_conditions = vec![];
        let mut state = ValidationState::default();
        let additional_data =
            Bytes32::parse(consensus_constants.agg_sig_me_additional_data.as_ref())?;
        for spend in &self.coin_spends {
            let reveal = spend.puzzle_reveal.to_program()?;
            let solution = spend.solution.to_program()?;
            if spend.coin.puzzle_hash != reveal.tree_hash() {
                Err(ClvmError::InvalidSpendbundle(
                    "Puzzle Hash does not match Puzzle Reveal for Spend".to_string(),
                ))?;
            }
            let (cost, output_conditions_program) =
                reveal.run(max_cost, NO_UNKNOWN_OPS | flags, &solution)?;
            state.total_cost += cost;
            state.total_removed += spend.coin.amount;
            if state.total_cost > max_cost {
                Err(ClvmError::CostExceeded(max_cost, state.total_cost))?;
            }
            if !state.coins_spent.insert(spend.coin) {
                Err(ClvmError::DoubleSpend(format!("{}", spend.coin.coin_id())))?;
            }
            let conditions_with_args: Vec<ConditionWithArgs> =
                output_conditions_program.sexp().try_into()?;
            if print {
                info!("Validating Spend of: {:?}", spend.coin);
                info!("Reveal: {reveal}");
                info!("Solution: {solution}");
            }
            for condition_with_args in conditions_with_args {
                if print {
                    info!("{condition_with_args}");
                }
                let agg_sig_additional_data = agg_sig_additional_data_for_opcode(
                    additional_data,
                    condition_with_args.op_code(),
                );
                //Check Costs
                match &condition_with_args {
                    ConditionWithArgs::Remark(_) | ConditionWithArgs::Unknown => {}
                    ConditionWithArgs::CreateCoin(puzzle_hash, amount, _) => {
                        max_cost = max_cost
                            .checked_sub(CREATE_COIN_COST)
                            .ok_or(ClvmError::CostExceeded(max_cost, state.total_cost))?;
                        let created_coin = Coin {
                            parent_coin_info: spend.coin.coin_id(),
                            puzzle_hash: *puzzle_hash,
                            amount: *amount,
                        };
                        state.total_created += created_coin.amount;
                        if !state.coins_created.insert(created_coin) {
                            Err(ClvmError::DuplicateCreate(format!(
                                "Duplicate CreateCoin Condition: {}",
                                created_coin.coin_id()
                            )))?;
                        }
                        create_conditions.push(condition_with_args.clone());
                    }
                    ConditionWithArgs::AggSigMe(public_key, message) => {
                        max_cost = max_cost
                            .checked_sub(AGG_SIG_COST)
                            .ok_or(ClvmError::CostExceeded(max_cost, state.total_cost))?;
                        state.agg_sig_me.push((*public_key, *message));
                        if (flags & DISABLE_SIGNATURE_VALIDATION) == 0 {
                            let mut msg = message.data().to_vec();
                            msg.extend(spend.coin.coin_id());
                            msg.extend(agg_sig_additional_data.bytes().as_ref());
                            state.pkm_pairs.push((*public_key, Message::new(msg)?));
                        }
                    }
                    ConditionWithArgs::AggSigParent(public_key, message) => {
                        max_cost = max_cost
                            .checked_sub(AGG_SIG_COST)
                            .ok_or(ClvmError::CostExceeded(max_cost, state.total_cost))?;
                        state.agg_sig_parents.push((*public_key, *message));
                        if (flags & DISABLE_SIGNATURE_VALIDATION) == 0 {
                            let mut msg = message.data().to_vec();
                            msg.extend(spend.coin.parent_coin_info);
                            msg.extend(agg_sig_additional_data.bytes().as_ref());
                            state.pkm_pairs.push((*public_key, Message::new(msg)?));
                        }
                    }
                    ConditionWithArgs::AggSigPuzzle(public_key, message) => {
                        max_cost = max_cost
                            .checked_sub(AGG_SIG_COST)
                            .ok_or(ClvmError::CostExceeded(max_cost, state.total_cost))?;
                        state.agg_sig_puzzles.push((*public_key, *message));
                        if (flags & DISABLE_SIGNATURE_VALIDATION) == 0 {
                            let mut msg = message.data().to_vec();
                            msg.extend(spend.coin.puzzle_hash);
                            msg.extend(agg_sig_additional_data.bytes().as_ref());
                            state.pkm_pairs.push((*public_key, Message::new(msg)?));
                        }
                    }
                    ConditionWithArgs::AggSigAmount(public_key, message) => {
                        max_cost = max_cost
                            .checked_sub(AGG_SIG_COST)
                            .ok_or(ClvmError::CostExceeded(max_cost, state.total_cost))?;
                        state.agg_sig_amounts.push((*public_key, *message));
                        if (flags & DISABLE_SIGNATURE_VALIDATION) == 0 {
                            let mut msg = message.data().to_vec();
                            msg.extend(u64_to_bytes(spend.coin.amount));
                            msg.extend(agg_sig_additional_data.bytes().as_ref());
                            state.pkm_pairs.push((*public_key, Message::new(msg)?));
                        }
                    }
                    ConditionWithArgs::AggSigPuzzleAmount(public_key, message) => {
                        max_cost = max_cost
                            .checked_sub(AGG_SIG_COST)
                            .ok_or(ClvmError::CostExceeded(max_cost, state.total_cost))?;
                        state.agg_sig_puzzle_amounts.push((*public_key, *message));
                        if (flags & DISABLE_SIGNATURE_VALIDATION) == 0 {
                            let mut msg = message.data().to_vec();
                            msg.extend(spend.coin.puzzle_hash);
                            msg.extend(u64_to_bytes(spend.coin.amount));
                            msg.extend(agg_sig_additional_data.bytes().as_ref());
                            state.pkm_pairs.push((*public_key, Message::new(msg)?));
                        }
                    }
                    ConditionWithArgs::AggSigParentAmount(public_key, message) => {
                        max_cost = max_cost
                            .checked_sub(AGG_SIG_COST)
                            .ok_or(ClvmError::CostExceeded(max_cost, state.total_cost))?;
                        state.agg_sig_parent_amounts.push((*public_key, *message));
                        if (flags & DISABLE_SIGNATURE_VALIDATION) == 0 {
                            let mut msg = message.data().to_vec();
                            msg.extend(spend.coin.parent_coin_info);
                            msg.extend(u64_to_bytes(spend.coin.amount));
                            msg.extend(agg_sig_additional_data.bytes().as_ref());
                            state.pkm_pairs.push((*public_key, Message::new(msg)?));
                        }
                    }
                    ConditionWithArgs::AggSigParentPuzzle(public_key, message) => {
                        max_cost = max_cost
                            .checked_sub(AGG_SIG_COST)
                            .ok_or(ClvmError::CostExceeded(max_cost, state.total_cost))?;
                        state.agg_sig_parent_puzzles.push((*public_key, *message));
                        if (flags & DISABLE_SIGNATURE_VALIDATION) == 0 {
                            let mut msg = message.data().to_vec();
                            msg.extend(spend.coin.parent_coin_info);
                            msg.extend(spend.coin.puzzle_hash);
                            msg.extend(agg_sig_additional_data.bytes().as_ref());
                            state.pkm_pairs.push((*public_key, Message::new(msg)?));
                        }
                    }
                    ConditionWithArgs::AggSigUnsafe(public_key, message) => {
                        max_cost = max_cost
                            .checked_sub(AGG_SIG_COST)
                            .ok_or(ClvmError::CostExceeded(max_cost, state.total_cost))?;
                        verify_agg_sig_unsafe_message(message, consensus_constants)?;
                        state.agg_sig_unsafe.push((*public_key, *message));
                        if (flags & DISABLE_SIGNATURE_VALIDATION) == 0 {
                            state.pkm_pairs.push((*public_key, *message));
                        }
                    }
                    ConditionWithArgs::AssertMyCoinId(my_coin_id) => {
                        if *my_coin_id != spend.coin.coin_id() {
                            Err(ClvmError::InvalidSpendbundle("Invalid Coin ID".to_string()))?;
                        }
                    }
                    ConditionWithArgs::AssertMyParentId(my_parent_id) => {
                        if *my_parent_id != spend.coin.parent_coin_info {
                            Err(ClvmError::InvalidSpendbundle(
                                "Invalid Parent Coin ID".to_string(),
                            ))?;
                        }
                    }
                    ConditionWithArgs::AssertMyPuzzlehash(my_puzzle_hash) => {
                        if *my_puzzle_hash != spend.coin.puzzle_hash {
                            Err(ClvmError::InvalidSpendbundle(
                                "Invalid Puzzle Hash".to_string(),
                            ))?;
                        }
                    }
                    ConditionWithArgs::AssertMyAmount(my_amount) => {
                        if *my_amount != spend.coin.amount {
                            Err(ClvmError::InvalidSpendbundle(
                                "Coin Amount Incorrect".to_string(),
                            ))?;
                        }
                    }
                    ConditionWithArgs::SendMessage(m_type, message, message_address) => {
                        state
                            .messages_sent
                            .push((*m_type, *message, *message_address, spend.coin));
                    }
                    ConditionWithArgs::ReceiveMessage(m_type, message, message_address) => {
                        if *message_address == MessageArgs::None {
                            if flags & IGNORE_ASSERT_CONCURRENT_NULL == 0 {
                                state.messages_received.push((
                                    *m_type,
                                    *message,
                                    *message_address,
                                    spend.coin,
                                ))
                            }
                        } else {
                            state.messages_received.push((
                                *m_type,
                                *message,
                                *message_address,
                                spend.coin,
                            ));
                        }
                    }
                    ConditionWithArgs::CreatePuzzleAnnouncement(message) => {
                        if flags & COST_CONDITIONS == 0 {
                            state.total_announcements += 1;
                            if state.total_announcements > ANNOUNCEMENT_LIMIT {
                                Err(ClvmError::TooManyAnnouncements)?;
                            }
                        }
                        state
                            .puzzle_announcements
                            .push((spend.coin.coin_id(), *message));
                    }
                    ConditionWithArgs::CreateCoinAnnouncement(message) => {
                        if flags & COST_CONDITIONS == 0 {
                            state.total_announcements += 1;
                            if state.total_announcements > ANNOUNCEMENT_LIMIT {
                                Err(ClvmError::TooManyAnnouncements)?;
                            }
                        }
                        state
                            .coin_announcements
                            .push((spend.coin.coin_id(), *message));
                    }
                    ConditionWithArgs::ReserveFee(reserve_fee) => {
                        state.total_reserved_fee = state
                            .total_reserved_fee
                            .checked_add(*reserve_fee)
                            .ok_or(ClvmError::Overflow("Overflow in Reserve Fee".to_string()))?
                    }
                    ConditionWithArgs::AssertCoinAnnouncement(puzzle_hash) => {
                        if flags & COST_CONDITIONS == 0 {
                            state.total_announcements += 1;
                            if state.total_announcements > ANNOUNCEMENT_LIMIT {
                                Err(ClvmError::TooManyAnnouncements)?;
                            }
                        }
                        state.asserted_coin_announcements.push(*puzzle_hash);
                    }
                    ConditionWithArgs::AssertPuzzleAnnouncement(puzzle_hash) => {
                        if flags & COST_CONDITIONS == 0 {
                            state.total_announcements += 1;
                            if state.total_announcements > ANNOUNCEMENT_LIMIT {
                                Err(ClvmError::TooManyAnnouncements)?;
                            }
                        }
                        state.asserted_puzzle_announcements.push(*puzzle_hash);
                    }
                    ConditionWithArgs::AssertConcurrentSpend(puzzle_hash) => {
                        if flags & COST_CONDITIONS == 0 {
                            state.total_announcements += 1;
                            if state.total_announcements > ANNOUNCEMENT_LIMIT {
                                Err(ClvmError::TooManyAnnouncements)?;
                            }
                        }
                        state.asserted_concurrent_spend.push(*puzzle_hash);
                    }
                    ConditionWithArgs::AssertConcurrentPuzzle(puzzle_hash) => {
                        if flags & COST_CONDITIONS == 0 {
                            state.total_announcements += 1;
                            if state.total_announcements > ANNOUNCEMENT_LIMIT {
                                Err(ClvmError::TooManyAnnouncements)?;
                            }
                        }
                        state.asserted_concurrent_puzzle.push(*puzzle_hash);
                    }
                    ConditionWithArgs::AssertMyBirthSeconds(seconds) => {
                        if state.birth_seconds.map(|v| v == *seconds) == Some(false) {
                            Err(ClvmError::InvalidSpendbundle(
                                "Cannot have 2 Different Birth Seconds".to_string(),
                            ))?;
                        }
                        state.birth_seconds = Some(*seconds);
                        //Assert not Ephemeral
                    }
                    ConditionWithArgs::AssertMyBirthHeight(height) => {
                        if state.birth_height.map(|v| v == *height) == Some(false) {
                            Err(ClvmError::InvalidSpendbundle(
                                "Cannot have 2 Different Birth Heights".to_string(),
                            ))?;
                        }
                        state.birth_height = Some(*height);
                        //Assert not Ephemeral
                    }
                    ConditionWithArgs::AssertEphemeral => {}
                    ConditionWithArgs::AssertSecondsRelative(seconds) => {
                        if let Some(current_value) = state.seconds_relative {
                            state.seconds_relative = Some(max(current_value, *seconds));
                        } else {
                            state.seconds_relative = Some(*seconds);
                        }
                        if let Some(before_seconds_relative) = state.before_seconds_relative
                            && before_seconds_relative <= *seconds
                        {
                            Err(ClvmError::InvalidSpendbundle(
                                "AssertBeforeSecondsRelative is <= AssertSecondsRelative"
                                    .to_string(),
                            ))?;
                        }
                        //Assert not Ephemeral
                    }
                    ConditionWithArgs::AssertSecondsAbsolute(seconds) => {
                        state.seconds_absolute = max(state.seconds_absolute, *seconds);
                    }
                    ConditionWithArgs::AssertHeightRelative(height) => {
                        if let Some(current_value) = state.height_relative {
                            state.height_relative = Some(max(current_value, *height));
                        } else {
                            state.height_relative = Some(*height);
                        }
                        if let Some(before_height_relative) = state.before_height_relative
                            && before_height_relative <= *height
                        {
                            Err(ClvmError::InvalidSpendbundle(
                                "AssertBeforeHeightRelative is <= AssertHeightRelative".to_string(),
                            ))?;
                        }
                        //Assert not Ephemeral
                    }
                    ConditionWithArgs::AssertHeightAbsolute(height) => {
                        state.height_absolute = max(state.height_absolute, *height);
                    }
                    ConditionWithArgs::AssertBeforeSecondsRelative(seconds) => {
                        if let Some(current_value) = state.before_seconds_relative {
                            state.before_seconds_relative = Some(max(current_value, *seconds));
                        } else {
                            state.before_seconds_relative = Some(*seconds);
                        }
                        if let Some(seconds_relative) = state.seconds_relative
                            && seconds_relative <= *seconds
                        {
                            Err(ClvmError::InvalidSpendbundle(
                                "AssertBeforeSecondsRelative is <= AssertSecondsRelative"
                                    .to_string(),
                            ))?;
                        }
                        //Assert not Ephemeral
                    }
                    ConditionWithArgs::AssertBeforeSecondsAbsolute(seconds) => {
                        if let Some(existing) = state.before_seconds_absolute {
                            state.before_seconds_absolute = Some(min(existing, *seconds));
                        } else {
                            state.before_seconds_absolute = Some(*seconds);
                        }
                    }
                    ConditionWithArgs::AssertBeforeHeightRelative(height) => {
                        if let Some(current_value) = state.before_height_relative {
                            state.before_height_relative = Some(max(current_value, *height));
                        } else {
                            state.before_height_relative = Some(*height);
                        }
                        if let Some(height_relative) = state.height_relative
                            && *height <= height_relative
                        {
                            Err(ClvmError::InvalidSpendbundle(
                                "AssertBeforeHeightRelative is <= AssertHeightRelative".to_string(),
                            ))?;
                        }
                        //Assert not Ephemeral
                    }
                    ConditionWithArgs::AssertBeforeHeightAbsolute(height) => {
                        if let Some(existing) = state.before_height_absolute {
                            state.before_height_absolute = Some(min(existing, *height));
                        } else {
                            state.before_height_absolute = Some(*height);
                        }
                    }
                    ConditionWithArgs::SoftFork(cost) => {
                        max_cost = max_cost
                            .checked_sub(*cost)
                            .ok_or(ClvmError::CostExceeded(max_cost, state.total_cost))?;
                        state.total_cost += cost;
                    }
                }
                state.output_conditions.push(condition_with_args);
            }
        }
        if (flags & DISABLE_SIGNATURE_VALIDATION) == 0 {
            let (keys, messages) = state.pkm_pairs.iter().fold(
                (vec![], vec![]),
                |(mut keys, mut messages), (key, msg)| {
                    keys.push(*key);
                    messages.push(msg.data());
                    (keys, messages)
                },
            );
            let signature = self.aggregated_signature.try_into()?;
            if !aggregate_verify_signature(&keys, &messages, &signature) {
                Err(ClvmError::InvalidSpendbundle(format!(
                    "Invalid signature on Spendbundle: {}",
                    self.aggregated_signature
                )))?;
            };
        }
        for coin_id in state.asserted_concurrent_spend {
            if coin_id == Bytes32::default()
                && flags & IGNORE_ASSERT_CONCURRENT_NULL == IGNORE_ASSERT_CONCURRENT_NULL
            {
                continue;
            }
            if !state.coins_spent.iter().any(|c| c.coin_id() == coin_id) {
                Err(ClvmError::InvalidSpendbundle(format!(
                    "Invalid Concurrent Spend: Missing Coin {coin_id}"
                )))?;
            }
        }
        for puzzle_hash in state.asserted_concurrent_puzzle {
            if !state
                .coins_spent
                .iter()
                .any(|c| c.puzzle_hash == puzzle_hash)
            {
                Err(ClvmError::InvalidSpendbundle(
                    "Invalid Concurrent Puzzle".to_string(),
                ))?;
            }
        }
        if !state.asserted_coin_announcements.is_empty() {
            let mut announcements = HashSet::<Bytes32>::new();
            for (coin_id, msg) in &state.coin_announcements {
                let mut buffer = Vec::with_capacity(32 + msg.data().len());
                buffer.extend_from_slice(coin_id.as_ref());
                buffer.extend_from_slice(msg.data());
                announcements.insert(hash_256(&buffer).into());
            }
            for announcement in &state.asserted_coin_announcements {
                if !announcements.contains(announcement) {
                    Err(ClvmError::InvalidSpendbundle(
                        "Failed to Assert Coin Announcement".to_string(),
                    ))?;
                }
            }
        }

        if state.messages_received.len() != state.messages_sent.len() {
            Err(ClvmError::InvalidSpendbundle(format!(
                "Sent Messages {} != Received Messages {}",
                state.messages_received.len(),
                state.messages_sent.len()
            )))?;
        }
        for (send_type, send_message, send_target, send_source) in &state.messages_sent {
            if !state
                .messages_received
                .iter()
                .filter(
                    |(receive_type, receive_message, receive_target, receive_source)| {
                        verify_send_recieve(
                            send_type,
                            receive_type,
                            send_message,
                            receive_message,
                            send_target,
                            receive_target,
                            send_source,
                            receive_source,
                        )
                    },
                )
                .count()
                == 1
            {
                Err(ClvmError::InvalidSpendbundle(
                    "Mismatch on Send and Receive messages".to_string(),
                ))?;
            }
        }
        if print {
            info!("Spendbundle Validated");
            info!("Total Cost: {}", state.total_cost);
            info!("Total Announcements: {}", state.total_announcements);
            info!("Total Reserved Fee: {}", state.total_reserved_fee);
            info!("Total Coins Spent: {}", state.coins_spent.len());
            info!("Total Coins Created: {}", state.coins_created.len());
            info!("Total Coins Announced: {}", state.coin_announcements.len());
            info!(
                "Total Puzzle Announcements: {}",
                state.puzzle_announcements.len()
            );
            info!("Total Messages Sent: {}", state.messages_sent.len());
            info!("Total Messages Received: {}", state.messages_received.len());
            info!("Total Agg Sig Pairs: {}", state.pkm_pairs.len());
            info!("Total Agg Sig Amounts: {}", state.agg_sig_amounts.len());
        }
        Ok(state.output_conditions)
    }
}

#[allow(clippy::too_many_arguments)]
#[cfg(feature = "bls")]
fn verify_send_recieve(
    send_type: &u8,
    receive_type: &u8,
    send_message: &Message,
    receive_message: &Message,
    send_target: &MessageArgs,
    receive_target: &MessageArgs,
    send_source: &Coin,
    receive_source: &Coin,
) -> bool {
    let res = receive_message == send_message
        && send_type == receive_type
        && {
            match send_target {
                MessageArgs::None => true,
                MessageArgs::CoinId(id) => receive_source.coin_id() == *id,
                MessageArgs::Puzzle(hash) => receive_source.puzzle_hash == *hash,
                MessageArgs::Parent(parent) => receive_source.parent_coin_info == *parent,
                MessageArgs::Amount(amount) => receive_source.amount == *amount,
                MessageArgs::ParentPuzzle {
                    parent,
                    puzzle_hash,
                } => {
                    receive_source.parent_coin_info == *parent
                        && receive_source.puzzle_hash == *puzzle_hash
                }
                MessageArgs::ParentAmount { parent, amount } => {
                    receive_source.parent_coin_info == *parent && receive_source.amount == *amount
                }
                MessageArgs::PuzzleAmount {
                    puzzle_hash,
                    amount,
                } => receive_source.puzzle_hash == *puzzle_hash && receive_source.amount == *amount,
            }
        }
        && {
            match receive_target {
                MessageArgs::None => true,
                MessageArgs::CoinId(id) => send_source.coin_id() == *id,
                MessageArgs::Puzzle(hash) => send_source.puzzle_hash == *hash,
                MessageArgs::Parent(parent) => send_source.parent_coin_info == *parent,
                MessageArgs::Amount(amount) => send_source.amount == *amount,
                MessageArgs::ParentPuzzle {
                    parent,
                    puzzle_hash,
                } => {
                    send_source.parent_coin_info == *parent
                        && send_source.puzzle_hash == *puzzle_hash
                }
                MessageArgs::ParentAmount { parent, amount } => {
                    send_source.parent_coin_info == *parent && send_source.amount == *amount
                }
                MessageArgs::PuzzleAmount {
                    puzzle_hash,
                    amount,
                } => send_source.puzzle_hash == *puzzle_hash && send_source.amount == *amount,
            }
        };
    if res {
        info!(
            "Verified Send -> Receive with (Send)-{send_source:?} to (Receive)-{receive_target:?}"
        );
        info!(
            "Verified Send <- Receive with (Send)-{send_target:?} to (Receive)-{receive_source:?}"
        );
    }
    res
}
