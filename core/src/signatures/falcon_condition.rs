//! Execution-derived direct-Falcon occurrences, preserving the original positions.
use super::{SignatureOccurrence, SignatureRequirement};
use crate::blockchain::{
    coin::Coin,
    condition_opcode::ConditionOpcode,
    condition_with_args::{ConditionWithArgs, Message},
    sized_bytes::{Bytes32, Bytes48, Bytes96, SizedBytesImpl},
    unsized_bytes::UnsizedBytes,
    utils::{pkm_pairs_for_conditions, verify_agg_sig_unsafe_message},
};
use crate::clvm::arena::{Arena, NodePtr};
use crate::consensus::constants::ConsensusConstants;
use crate::errors::ClvmError;
use sha2::{Digest, Sha384};

pub const FALCON_CONDITION_COST: u64 = 4_000_000;
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FalconCondition {
    pub coin_id: Bytes32,
    pub condition_index: u32,
    pub public_key: SizedBytesImpl<897>,
    pub message: UnsizedBytes,
}
impl FalconCondition {
    pub fn requirement(&self) -> SignatureRequirement {
        SignatureRequirement::Falcon {
            occurrence: SignatureOccurrence {
                coin_id: self.coin_id,
                condition_index: self.condition_index,
            },
            public_key: Box::new(self.public_key.const_bytes()),
            message: self.message.as_slice().to_vec(),
        }
    }
}

pub fn from_arena(
    arena: &Arena,
    condition: NodePtr,
    coin: Coin,
    index: usize,
    constants: &ConsensusConstants,
) -> Result<Option<FalconCondition>, ClvmError> {
    let Some((op, mut rest)) = arena.next(condition) else {
        return Ok(None);
    };
    let Some(op) = arena.atom(op) else {
        return Ok(None);
    };
    let op = op.as_ref();
    if op.len() != 1 || !(43..=50).contains(&op[0]) {
        return Ok(None);
    }
    let opcode = op[0];
    let mut fields = Vec::with_capacity(3);
    for _ in 0..3 {
        let Some((field, tail)) = arena.next(rest) else {
            return Err(invalid(
                "Falcon condition requires key, message and direct flag",
            ));
        };
        let atom = arena
            .atom(field)
            .ok_or_else(|| invalid("Falcon condition argument is not an atom"))?;
        if atom.as_ref().len() > 1024 {
            return Err(invalid("Falcon condition argument too large"));
        }
        fields.push(atom.as_ref().to_vec());
        rest = tail;
    }
    if !arena.atom(rest).is_some_and(|a| a.as_ref().is_empty()) || !fields[2].is_empty() {
        return Err(invalid(
            "Falcon direct condition needs an exact four-element list and an empty flag",
        ));
    }
    let public_key: [u8; 897] = fields[0]
        .as_slice()
        .try_into()
        .map_err(|_| invalid("Falcon public key must have 897 bytes"))?;
    if public_key[0] != 0x09 {
        return Err(invalid("invalid Falcon public key profile"));
    }
    let message = Message::new(std::mem::take(&mut fields[1]))?;
    let key = Bytes48::default();
    let parsed = match ConditionOpcode::from(opcode) {
        ConditionOpcode::AggSigParent => ConditionWithArgs::AggSigParent(key, message),
        ConditionOpcode::AggSigPuzzle => ConditionWithArgs::AggSigPuzzle(key, message),
        ConditionOpcode::AggSigAmount => ConditionWithArgs::AggSigAmount(key, message),
        ConditionOpcode::AggSigPuzzleAmount => ConditionWithArgs::AggSigPuzzleAmount(key, message),
        ConditionOpcode::AggSigParentAmount => ConditionWithArgs::AggSigParentAmount(key, message),
        ConditionOpcode::AggSigParentPuzzle => ConditionWithArgs::AggSigParentPuzzle(key, message),
        ConditionOpcode::AggSigMe => ConditionWithArgs::AggSigMe(key, message),
        ConditionOpcode::AggSigUnsafe => {
            verify_agg_sig_unsafe_message(&message, constants)?;
            ConditionWithArgs::AggSigUnsafe(key, message)
        }
        _ => unreachable!(),
    };
    let derived = pkm_pairs_for_conditions(
        &[parsed],
        coin,
        constants.agg_sig_me_additional_data.as_ref(),
    )?;
    Ok(Some(FalconCondition {
        coin_id: coin.name(),
        condition_index: u32::try_from(index).map_err(|_| invalid("condition index overflow"))?,
        public_key: SizedBytesImpl::const_new(public_key),
        message: UnsizedBytes::new(derived[0].2.data().to_vec()),
    }))
}

/// The empty reference preserves the common producer's canonical empty witness.
/// Nonempty references commit to every occurrence in execution order, including duplicates.
pub fn transaction_reference(genesis: Bytes32, requirements: &[FalconCondition]) -> Bytes96 {
    if requirements.is_empty() {
        let mut bytes = [0; 96];
        bytes[0] = 0xc0;
        return Bytes96::const_new(bytes);
    }
    let mut hash = Sha384::new();
    hash.update(b"dg_xch/falcon/transactions/v2\0");
    hash.update(genesis);
    hash.update((requirements.len() as u64).to_be_bytes());
    for requirement in requirements {
        hash.update(requirement.coin_id);
        hash.update(requirement.condition_index.to_be_bytes());
        hash.update(requirement.public_key);
        hash.update((requirement.message.as_slice().len() as u64).to_be_bytes());
        hash.update(requirement.message.as_slice());
    }
    let mut bytes = [0; 96];
    bytes[1..49].copy_from_slice(&hash.finalize());
    Bytes96::const_new(bytes)
}

fn invalid(message: &str) -> ClvmError {
    ClvmError::InvalidSpendbundle(message.into())
}
