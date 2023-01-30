use crate::types::blockchain::sized_bytes::Bytes32;
use crate::types::blockchain::sized_bytes::SizedBytes;
use crate::types::blockchain::vdf_info::VdfInfo;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::error::Error;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ChallengeChainSubSlot {
    pub challenge_chain_end_of_slot_vdf: VdfInfo,
    pub new_sub_slot_iters: Option<u64>,
    pub new_difficulty: Option<u64>,
    pub infused_challenge_chain_sub_slot_hash: Option<Bytes32>,
    pub subepoch_summary_hash: Option<Bytes32>,
}
impl ChallengeChainSubSlot {
    pub fn hash(&self) -> Result<Vec<u8>, Box<dyn Error>> {
        let mut to_hash: Vec<u8> = Vec::new();
        to_hash.extend(&self.challenge_chain_end_of_slot_vdf.challenge.to_bytes());
        to_hash.extend(
            self.challenge_chain_end_of_slot_vdf
                .number_of_iterations
                .to_be_bytes(),
        );
        to_hash.extend(&self.challenge_chain_end_of_slot_vdf.output.data.to_bytes());
        match &self.infused_challenge_chain_sub_slot_hash {
            Some(sub_slot_hash) => {
                to_hash.push(1);
                to_hash.extend(sub_slot_hash.to_bytes());
            }
            None => {
                to_hash.push(0);
            }
        }
        match &self.subepoch_summary_hash {
            Some(summary_hash) => {
                to_hash.push(1);
                to_hash.extend(summary_hash.to_bytes());
            }
            None => {
                to_hash.push(0);
            }
        }
        match &self.new_sub_slot_iters {
            Some(slot_iters) => {
                to_hash.push(1);
                to_hash.extend(slot_iters.to_be_bytes());
            }
            None => {
                to_hash.push(0);
            }
        }
        match &self.new_difficulty {
            Some(difficulty) => {
                to_hash.push(1);
                to_hash.extend(difficulty.to_be_bytes());
            }
            None => {
                to_hash.push(0);
            }
        }
        let mut hasher: Sha256 = Sha256::new();
        hasher.update(to_hash);
        Ok(hasher.finalize().to_vec())
    }
}
