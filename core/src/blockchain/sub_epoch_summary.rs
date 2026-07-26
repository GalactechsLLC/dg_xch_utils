use crate::blockchain::sized_bytes::Bytes32;
use dg_xch_macros::ChiaSerial;
use serde::{Deserialize, Serialize};

// NOTE (verified against a real mainnet weight proof + the reference's chia_rs golden hashes):
// current mainnet's on-chain sub-epoch-summary hash is over EXACTLY these five fields. A newer chia_rs
// adds a 6th `challenge_merkle_root: Option<Bytes32>` (and `SubEpochData` gains one too), but that field
// is NOT active on mainnet yet — the fetched proof carries the 4-field `SubEpochData`, and golden[0] =
// sha256(5-field bytes). Adding the 6th field here shifts every SES hash by a trailing 0x00 and breaks
// the phase-2 anchor check. Do NOT add it until the activating hard fork lands on the target chain, and
// then only gated on activation height. See the `dg_xch_weight_proof` crate's phase-2 test.
#[derive(ChiaSerial, Copy, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct SubEpochSummary {
    pub prev_subepoch_summary_hash: Bytes32,
    pub reward_chain_hash: Bytes32,
    pub num_blocks_overflow: u8,
    pub new_difficulty: Option<u64>,
    pub new_sub_slot_iters: Option<u64>,
}
