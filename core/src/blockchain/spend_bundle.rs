use crate::blockchain::coin::Coin;
use crate::blockchain::coin_spend::CoinSpend;
use crate::blockchain::condition_with_args::{ConditionWithArgs, Message};
use crate::blockchain::sized_bytes::{Bytes32, Bytes48, Bytes96};
use crate::blockchain::unsized_bytes::UnsizedBytes;
use crate::blockchain::utils::{pkm_pairs_for_conditions, verify_agg_sig_unsafe_message};
use crate::clvm::bls_bindings;
use crate::clvm::bls_bindings::{aggregate_verify_signature, verify_signature};
use crate::clvm::condition_utils::{agg_sig_additional_data_for_opcode, conditions_for_solution};
use crate::clvm::program::Program;
use crate::clvm::utils::{
    COST_CONDITIONS, DISABLE_SIGNATURE_VALIDATION, IGNORE_ASSERT_CONCURRENT_NULL, INFINITE_COST,
    NO_UNKNOWN_OPS,
};
use crate::consensus::constants::{ConsensusConstants, MAINNET};
use crate::consensus::{AGG_SIG_COST, CREATE_COIN_COST};
use crate::formatting::u64_to_bytes;
use crate::traits::SizedBytes;
use crate::utils::hash_256;
use blst::min_pk::{AggregateSignature, PublicKey, SecretKey, Signature};
use dg_xch_macros::ChiaSerial;
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use log::info;
use num_traits::ToPrimitive;
use serde::{Deserialize, Serialize};
use std::cmp::{max, min};
use std::collections::HashSet;
use std::future::Future;
use std::io::{Error, ErrorKind};

const ANNOUNCEMENT_LIMIT: u64 = 1024;

#[derive(Default, Clone, Debug)]
struct ValidationState {
    pub coins_spent: HashSet<Coin>,
    pub coins_created: HashSet<Coin>,
    pub messages_sent: Vec<(u8, Bytes32, Message, Bytes32)>,
    pub messages_received: Vec<(u8, Bytes32, Message, Bytes32)>,
    pub puzzle_announcements: Vec<(Bytes32, Message)>,
    pub asserted_puzzle_announcements: Vec<Bytes32>,
    pub coin_announcements: Vec<(Bytes32, Message)>,
    pub asserted_coin_announcements: Vec<Bytes32>,
    pub asserted_concurrent_spend: Vec<Bytes32>,
    pub asserted_concurrent_puzzle: Vec<Bytes32>,
    // pub asserted_not_ephemeral: Vec<Bytes32>,
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
            let conditions =
                conditions_for_solution(&coin_spend.puzzle_reveal, &coin_spend.solution, max_cost)?
                    .0;
            //Create signature
            for (code, pk_bytes, msg) in pkm_pairs_for_conditions(
                &conditions,
                coin_spend.coin,
                &constants.agg_sig_me_additional_data,
            )? {
                let pk = PublicKey::from_bytes(pk_bytes.as_ref()).map_err(|e| {
                    Error::other(format!(
                        "Failed to parse Public key: {}, {:?}",
                        hex::encode(pk_bytes),
                        e
                    ))
                })?;
                let secret_key = (key_function)(&pk_bytes).await?;
                assert_eq!(&secret_key.sk_to_pk(), &pk);
                let signature = bls_bindings::sign(&secret_key, msg.as_ref());
                if !verify_signature(&pk, msg.as_ref(), &signature) {
                    return Err(Error::other(format!(
                        "PH({}) Failed to Validate Signature for Message: {} - {}",
                        pk_bytes,
                        code,
                        UnsizedBytes::new(msg.as_ref())
                    )));
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
            .map_err(|e| Error::other(format!("Failed to aggregate signatures: {e:?}")))?
            .to_signature();
        assert!(aggregate_verify_signature(&pk_list, &msg_list, &aggsig));
        self.aggregated_signature = aggsig.to_bytes().into();
        info!("Signed Bundle");
        Ok(self)
    }
    pub fn validate(
        &self,
        max_cost: Option<u64>,
        flags: u32,
        consensus_constants: &ConsensusConstants,
        print: bool,
    ) -> Result<Vec<ConditionWithArgs>, Error> {
        info!(
            "Using Constants: {} - {}",
            consensus_constants.simulated,
            Bytes32::parse(&consensus_constants.agg_sig_me_additional_data)?
        );
        let mut max_cost = max_cost.unwrap_or(INFINITE_COST);
        let mut create_conditions = vec![];
        let mut state = ValidationState::default();
        let additional_data = Bytes32::parse(&consensus_constants.agg_sig_me_additional_data)?;
        for spend in &self.coin_spends {
            if spend.coin.puzzle_hash != spend.puzzle_reveal.to_program().tree_hash() {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "Puzzle Hash does not match Puzzle Reveal for Spend",
                ));
            }
            let (cost, output_conditions_program) = spend.puzzle_reveal.run(
                max_cost,
                NO_UNKNOWN_OPS | flags,
                &spend.solution.to_program(),
            )?;
            state.total_cost += cost;
            state.total_removed += spend.coin.amount;
            if state.total_cost > max_cost {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    format!("Max Cost Exceded {} > {max_cost}", state.total_cost),
                ));
            }
            if !state.coins_spent.insert(spend.coin) {
                //Double Spend
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    format!("Duplicate Spend: {}", spend.coin.coin_id()),
                ));
            }
            let conditions_with_args: Vec<ConditionWithArgs> =
                (&output_conditions_program.sexp).try_into()?;
            for condition_with_args in &conditions_with_args {
                if print {
                    info!("{condition_with_args}");
                }
                let agg_sig_additional_data = agg_sig_additional_data_for_opcode(
                    additional_data,
                    condition_with_args.op_code(),
                );
                //Check Costs
                match condition_with_args {
                    ConditionWithArgs::Remark(_) | ConditionWithArgs::Unknown => {}
                    ConditionWithArgs::CreateCoin(puzzle_hash, amount, _) => {
                        if max_cost < CREATE_COIN_COST {
                            return Err(Error::new(ErrorKind::InvalidInput, "Max Cost Exceeded"));
                        }
                        max_cost = max_cost
                            .checked_sub(CREATE_COIN_COST)
                            .ok_or(Error::new(ErrorKind::InvalidInput, "Max Cost Exceeded"))?;
                        let created_coin = Coin {
                            parent_coin_info: spend.coin.coin_id(),
                            puzzle_hash: *puzzle_hash,
                            amount: *amount,
                        };
                        state.total_created += created_coin.amount;
                        if !state.coins_created.insert(created_coin) {
                            return Err(Error::new(
                                ErrorKind::InvalidInput,
                                format!(
                                    "Duplicate CreateCoin Condition: {}",
                                    created_coin.coin_id()
                                ),
                            ));
                        }
                        create_conditions.push(*condition_with_args);
                    }
                    ConditionWithArgs::AggSigMe(public_key, message) => {
                        if max_cost < AGG_SIG_COST {
                            return Err(Error::new(ErrorKind::InvalidInput, "Max Cost Exceeded"));
                        }
                        max_cost = max_cost
                            .checked_sub(AGG_SIG_COST)
                            .ok_or(Error::new(ErrorKind::InvalidInput, "Max Cost Exceeded"))?;
                        state.agg_sig_me.push((*public_key, *message));
                        if (flags & DISABLE_SIGNATURE_VALIDATION) == 0 {
                            let mut msg = message.data().to_vec();
                            msg.extend(spend.coin.coin_id());
                            msg.extend(agg_sig_additional_data.bytes().as_ref());
                            state.pkm_pairs.push((*public_key, Message::new(msg)?));
                        }
                    }
                    ConditionWithArgs::AggSigParent(public_key, message) => {
                        if max_cost < AGG_SIG_COST {
                            return Err(Error::new(ErrorKind::InvalidInput, "Max Cost Exceeded"));
                        }
                        max_cost = max_cost
                            .checked_sub(AGG_SIG_COST)
                            .ok_or(Error::new(ErrorKind::InvalidInput, "Max Cost Exceeded"))?;
                        state.agg_sig_parents.push((*public_key, *message));
                        if (flags & DISABLE_SIGNATURE_VALIDATION) == 0 {
                            let mut msg = message.data().to_vec();
                            msg.extend(spend.coin.parent_coin_info);
                            msg.extend(agg_sig_additional_data.bytes().as_ref());
                            state.pkm_pairs.push((*public_key, Message::new(msg)?));
                        }
                    }
                    ConditionWithArgs::AggSigPuzzle(public_key, message) => {
                        if max_cost < AGG_SIG_COST {
                            return Err(Error::new(ErrorKind::InvalidInput, "Max Cost Exceeded"));
                        }
                        max_cost = max_cost
                            .checked_sub(AGG_SIG_COST)
                            .ok_or(Error::new(ErrorKind::InvalidInput, "Max Cost Exceeded"))?;
                        state.agg_sig_puzzles.push((*public_key, *message));
                        if (flags & DISABLE_SIGNATURE_VALIDATION) == 0 {
                            let mut msg = message.data().to_vec();
                            msg.extend(spend.coin.puzzle_hash);
                            msg.extend(agg_sig_additional_data.bytes().as_ref());
                            state.pkm_pairs.push((*public_key, Message::new(msg)?));
                        }
                    }
                    ConditionWithArgs::AggSigAmount(public_key, message) => {
                        if max_cost < AGG_SIG_COST {
                            return Err(Error::new(ErrorKind::InvalidInput, "Max Cost Exceeded"));
                        }
                        max_cost = max_cost
                            .checked_sub(AGG_SIG_COST)
                            .ok_or(Error::new(ErrorKind::InvalidInput, "Max Cost Exceeded"))?;
                        state.agg_sig_amounts.push((*public_key, *message));
                        if (flags & DISABLE_SIGNATURE_VALIDATION) == 0 {
                            let mut msg = message.data().to_vec();
                            msg.extend(u64_to_bytes(spend.coin.amount));
                            msg.extend(agg_sig_additional_data.bytes().as_ref());
                            state.pkm_pairs.push((*public_key, Message::new(msg)?));
                        }
                    }
                    ConditionWithArgs::AggSigPuzzleAmount(public_key, message) => {
                        if max_cost < AGG_SIG_COST {
                            return Err(Error::new(ErrorKind::InvalidInput, "Max Cost Exceeded"));
                        }
                        max_cost = max_cost
                            .checked_sub(AGG_SIG_COST)
                            .ok_or(Error::new(ErrorKind::InvalidInput, "Max Cost Exceeded"))?;
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
                        if max_cost < AGG_SIG_COST {
                            return Err(Error::new(ErrorKind::InvalidInput, "Max Cost Exceeded"));
                        }
                        max_cost = max_cost
                            .checked_sub(AGG_SIG_COST)
                            .ok_or(Error::new(ErrorKind::InvalidInput, "Max Cost Exceeded"))?;
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
                        if max_cost < AGG_SIG_COST {
                            return Err(Error::new(ErrorKind::InvalidInput, "Max Cost Exceeded"));
                        }
                        max_cost = max_cost
                            .checked_sub(AGG_SIG_COST)
                            .ok_or(Error::new(ErrorKind::InvalidInput, "Max Cost Exceeded"))?;
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
                        if max_cost < AGG_SIG_COST {
                            return Err(Error::new(ErrorKind::InvalidInput, "Max Cost Exceeded"));
                        }
                        max_cost = max_cost
                            .checked_sub(AGG_SIG_COST)
                            .ok_or(Error::new(ErrorKind::InvalidInput, "Max Cost Exceeded"))?;
                        verify_agg_sig_unsafe_message(message, consensus_constants)?;
                        state.agg_sig_unsafe.push((*public_key, *message));
                        if (flags & DISABLE_SIGNATURE_VALIDATION) == 0 {
                            state.pkm_pairs.push((*public_key, *message));
                        }
                    }
                    ConditionWithArgs::AssertMyCoinId(my_coin_id) => {
                        if *my_coin_id != spend.coin.coin_id() {
                            return Err(Error::new(ErrorKind::InvalidInput, "Invalid Coin ID"));
                        }
                    }
                    ConditionWithArgs::AssertMyParentId(my_parent_id) => {
                        if *my_parent_id != spend.coin.parent_coin_info {
                            return Err(Error::new(
                                ErrorKind::InvalidInput,
                                "Invalid Parent Coin ID",
                            ));
                        }
                    }
                    ConditionWithArgs::AssertMyPuzzlehash(my_puzzle_hash) => {
                        if *my_puzzle_hash != spend.coin.puzzle_hash {
                            return Err(Error::new(ErrorKind::InvalidInput, "Invalid Puzzle Hash"));
                        }
                    }
                    ConditionWithArgs::AssertMyAmount(my_amount) => {
                        if *my_amount != spend.coin.amount {
                            return Err(Error::new(
                                ErrorKind::InvalidInput,
                                "Coin Amount Incorrect",
                            ));
                        }
                    }
                    ConditionWithArgs::SendMessage(m_type, message_address, message) => {
                        state.messages_sent.push((
                            *m_type,
                            *message_address,
                            *message,
                            spend.coin.coin_id(),
                        ));
                    }
                    ConditionWithArgs::ReceiveMessage(m_type, message_address, message) => {
                        if *message_address == Bytes32::default() {
                            if flags & IGNORE_ASSERT_CONCURRENT_NULL == 0 {
                                state.messages_received.push((
                                    *m_type,
                                    *message_address,
                                    *message,
                                    spend.coin.coin_id(),
                                ))
                            }
                        } else {
                            state.messages_received.push((
                                *m_type,
                                *message_address,
                                *message,
                                spend.coin.coin_id(),
                            ));
                        }
                    }
                    ConditionWithArgs::CreatePuzzleAnnouncement(message) => {
                        if flags & COST_CONDITIONS == 0 {
                            state.total_announcements += 1;
                            if state.total_announcements > ANNOUNCEMENT_LIMIT {
                                return Err(Error::new(
                                    ErrorKind::InvalidInput,
                                    "Total Announcements exceeded",
                                ));
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
                                return Err(Error::new(
                                    ErrorKind::InvalidInput,
                                    "Total Announcements exceeded",
                                ));
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
                            .ok_or(Error::new(
                                ErrorKind::InvalidInput,
                                "Overflow in Reserve Fee",
                            ))?
                    }
                    ConditionWithArgs::AssertCoinAnnouncement(puzzle_hash) => {
                        if flags & COST_CONDITIONS == 0 {
                            state.total_announcements += 1;
                            if state.total_announcements > ANNOUNCEMENT_LIMIT {
                                return Err(Error::new(
                                    ErrorKind::InvalidInput,
                                    "Total Announcements exceeded",
                                ));
                            }
                        }
                        state.asserted_coin_announcements.push(*puzzle_hash);
                    }
                    ConditionWithArgs::AssertPuzzleAnnouncement(puzzle_hash) => {
                        if flags & COST_CONDITIONS == 0 {
                            state.total_announcements += 1;
                            if state.total_announcements > ANNOUNCEMENT_LIMIT {
                                return Err(Error::new(
                                    ErrorKind::InvalidInput,
                                    "Total Announcements exceeded",
                                ));
                            }
                        }
                        state.asserted_puzzle_announcements.push(*puzzle_hash);
                    }
                    ConditionWithArgs::AssertConcurrentSpend(puzzle_hash) => {
                        if flags & COST_CONDITIONS == 0 {
                            state.total_announcements += 1;
                            if state.total_announcements > ANNOUNCEMENT_LIMIT {
                                return Err(Error::new(
                                    ErrorKind::InvalidInput,
                                    "Total Announcements exceeded",
                                ));
                            }
                        }
                        state.asserted_concurrent_spend.push(*puzzle_hash);
                    }
                    ConditionWithArgs::AssertConcurrentPuzzle(puzzle_hash) => {
                        if flags & COST_CONDITIONS == 0 {
                            state.total_announcements += 1;
                            if state.total_announcements > ANNOUNCEMENT_LIMIT {
                                return Err(Error::new(
                                    ErrorKind::InvalidInput,
                                    "Total Announcements exceeded",
                                ));
                            }
                        }
                        state.asserted_concurrent_puzzle.push(*puzzle_hash);
                    }
                    ConditionWithArgs::AssertMyBirthSeconds(seconds) => {
                        if state.birth_seconds.map(|v| v == *seconds) == Some(false) {
                            return Err(Error::new(
                                ErrorKind::InvalidInput,
                                "Cannot have 2 Different Birth Seconds",
                            ));
                        }
                        state.birth_seconds = Some(*seconds);
                        //Assert not Ephemeral
                    }
                    ConditionWithArgs::AssertMyBirthHeight(height) => {
                        if state.birth_height.map(|v| v == *height) == Some(false) {
                            return Err(Error::new(
                                ErrorKind::InvalidInput,
                                "Cannot have 2 Different Birth Heights",
                            ));
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
                        if let Some(before_seconds_relative) = state.before_seconds_relative {
                            if before_seconds_relative <= *seconds {
                                return Err(Error::new(
                                    ErrorKind::InvalidInput,
                                    "AssertBeforeSecondsRelative is <= AssertSecondsRelative",
                                ));
                            }
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
                        if let Some(before_height_relative) = state.before_height_relative {
                            if before_height_relative <= *height {
                                return Err(Error::new(
                                    ErrorKind::InvalidInput,
                                    "AssertBeforeHeightRelative is <= AssertHeightRelative",
                                ));
                            }
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
                        if let Some(seconds_relative) = state.seconds_relative {
                            if seconds_relative <= *seconds {
                                return Err(Error::new(
                                    ErrorKind::InvalidInput,
                                    "AssertBeforeSecondsRelative is <= AssertSecondsRelative",
                                ));
                            }
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
                        if let Some(height_relative) = state.height_relative {
                            if *height <= height_relative {
                                return Err(Error::new(
                                    ErrorKind::InvalidInput,
                                    "AssertBeforeHeightRelative is <= AssertHeightRelative",
                                ));
                            }
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
                        if max_cost < *cost {
                            return Err(Error::new(ErrorKind::InvalidInput, "Max Cost Exceeded"));
                        }
                        max_cost = max_cost
                            .checked_sub(*cost)
                            .ok_or(Error::new(ErrorKind::InvalidInput, "Max Cost Exceeded"))?;
                        state.total_cost += cost;
                    }
                }
            }
            state.output_conditions.extend(conditions_with_args);
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
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    format!(
                        "Invalid signature on Spendbundle: {}",
                        self.aggregated_signature
                    ),
                ));
            };
        }
        for coin_id in state.asserted_concurrent_spend {
            if coin_id == Bytes32::default()
                && flags & IGNORE_ASSERT_CONCURRENT_NULL == IGNORE_ASSERT_CONCURRENT_NULL
            {
                continue;
            }
            if !state.coins_spent.iter().any(|c| c.coin_id() == coin_id) {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    format!("Invalid Concurrent Spend: Missing Coin {coin_id}"),
                ));
            }
        }
        for puzzle_hash in state.asserted_concurrent_puzzle {
            if !state
                .coins_spent
                .iter()
                .any(|c| c.puzzle_hash == puzzle_hash)
            {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "Invalid Concurrent Puzzle",
                ));
            }
        }
        if !state.asserted_coin_announcements.is_empty() {
            let mut announcements = HashSet::<Bytes32>::new();
            for (coin_id, msg) in state.coin_announcements {
                let mut buffer = Vec::with_capacity(32 + msg.data().len());
                buffer.extend_from_slice(coin_id.as_ref());
                buffer.extend_from_slice(msg.data());
                announcements.insert(hash_256(&buffer).into());
            }
            for announcement in state.asserted_coin_announcements {
                if !announcements.contains(&announcement) {
                    return Err(Error::new(
                        ErrorKind::InvalidInput,
                        "Failed to Assert Coin Announcement",
                    ));
                }
            }
        }

        if state.messages_received.len() != state.messages_sent.len() {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                format!(
                    "Sent Messages {} != Received Messages {}",
                    state.messages_received.len(),
                    state.messages_sent.len()
                ),
            ));
        }
        for (send_type, send_target, send_message, send_source) in &state.messages_sent {
            if !state
                .messages_received
                .iter()
                .filter(
                    |(receive_type, receive_target, receive_message, receive_source)| {
                        receive_target == send_source
                            && receive_source == send_target
                            && receive_message == send_message
                            && send_type == receive_type
                    },
                )
                .count()
                == 1
            {
                return Err(Error::new(
                    ErrorKind::NotFound,
                    "Mismatch on Send and Receive messages",
                ));
            }
        }
        Ok(state.output_conditions)
    }
}
