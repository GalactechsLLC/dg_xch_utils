use crate::types::blockchain::proof_of_space::ProofOfSpace;
use crate::types::blockchain::sized_bytes::{Bytes32, Bytes48, Bytes96, SizedBytes};
use crate::types::ChiaSerialize;
use serde::{Deserialize, Serialize};
use std::fmt::{Display, Formatter};
use std::io::{Error, ErrorKind};

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct PoolDifficulty {
    pub difficulty: u64,
    pub sub_slot_iters: u64,
    pub pool_contract_puzzle_hash: Bytes32,
}
impl ChiaSerialize for PoolDifficulty {
    fn to_bytes(&self) -> Vec<u8>
    where
        Self: Sized,
    {
        let mut bytes: Vec<u8> = Vec::new();
        bytes.extend(self.difficulty.to_be_bytes());
        bytes.extend(self.sub_slot_iters.to_be_bytes());
        bytes.extend(self.pool_contract_puzzle_hash.to_sized_bytes());
        bytes
    }

    fn from_bytes(bytes: &[u8]) -> Result<Self, Error>
    where
        Self: Sized,
    {
        let mut u64_ary: [u8; 8] = [0; 8];
        let (difficulty, rest) = bytes.split_at(8);
        u64_ary.copy_from_slice(&difficulty[0..8]);
        let difficulty = u64::from_be_bytes(u64_ary);
        let (sub_slot_iters, rest) = rest.split_at(8);
        u64_ary.copy_from_slice(&sub_slot_iters[0..8]);
        let sub_slot_iters = u64::from_be_bytes(u64_ary);
        let (pool_contract_puzzle_hash, _) = rest.split_at(32);
        Ok(Self {
            difficulty,
            sub_slot_iters,
            pool_contract_puzzle_hash: pool_contract_puzzle_hash.into(),
        })
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct HarvesterHandshake {
    pub farmer_public_keys: Vec<Bytes48>,
    pub pool_public_keys: Vec<Bytes48>,
}
impl ChiaSerialize for HarvesterHandshake {
    fn to_bytes(&self) -> Vec<u8> {
        let mut bytes: Vec<u8> = Vec::new();
        bytes.extend((self.farmer_public_keys.len() as u32).to_be_bytes());
        for b in &self.farmer_public_keys {
            bytes.extend(b.to_sized_bytes());
        }
        bytes.extend((self.pool_public_keys.len() as u32).to_be_bytes());
        for b in &self.pool_public_keys {
            bytes.extend(b.to_sized_bytes());
        }
        bytes
    }

    fn from_bytes(bytes: &[u8]) -> Result<Self, Error>
    where
        Self: Sized,
    {
        let mut farmer_public_keys = vec![];
        let mut pool_public_keys = vec![];
        let mut u32_len_ary: [u8; 4] = [0; 4];
        //Read the Farmer Keys
        let (data_len, mut rest) = bytes.split_at(4);
        u32_len_ary.copy_from_slice(&data_len[0..4]);
        let len = u32::from_be_bytes(u32_len_ary) as usize;
        for _ in 0..len {
            let (k, r) = rest.split_at(48);
            farmer_public_keys.push(k.into());
            rest = r;
        }
        //Read the Public Keys
        let (data_len, mut rest) = rest.split_at(4);
        u32_len_ary.copy_from_slice(&data_len[0..4]);
        let len = u32::from_be_bytes(u32_len_ary) as usize;
        for _ in 0..len {
            let (k, r) = rest.split_at(48);
            pool_public_keys.push(k.into());
            rest = r;
        }
        Ok(Self {
            farmer_public_keys,
            pool_public_keys,
        })
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct NewSignagePointHarvester {
    pub challenge_hash: Bytes32,
    pub difficulty: u64,
    pub sub_slot_iters: u64,
    pub signage_point_index: u8,
    pub sp_hash: Bytes32,
    pub pool_difficulties: Vec<PoolDifficulty>,
}
impl Display for NewSignagePointHarvester {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "NewSignagePointHarvester {{")?;
        writeln!(f, "\tchallenge_hash: {:?},", self.challenge_hash)?;
        writeln!(f, "\tdifficulty: {:?},", self.difficulty)?;
        writeln!(f, "\tsub_slot_iters: {:?},", self.sub_slot_iters)?;
        writeln!(f, "\tsignage_point_index: {:?},", self.signage_point_index)?;
        writeln!(f, "\tsp_hash: {:?},", self.sp_hash)?;
        writeln!(f, "\tpool_difficulties: {:?},", self.pool_difficulties)?;
        writeln!(f, "}}")
    }
}
impl ChiaSerialize for NewSignagePointHarvester {
    fn to_bytes(&self) -> Vec<u8> {
        let mut bytes: Vec<u8> = Vec::new();
        bytes.extend(self.challenge_hash.to_sized_bytes());
        bytes.extend(self.difficulty.to_be_bytes());
        bytes.extend(self.sub_slot_iters.to_be_bytes());
        bytes.push(self.signage_point_index);
        bytes.extend(self.sp_hash.to_sized_bytes());
        bytes.extend((self.pool_difficulties.len() as u32).to_be_bytes());
        for d in &self.pool_difficulties {
            bytes.extend(d.to_bytes());
        }
        bytes.push(self.signage_point_index);
        bytes
    }

    fn from_bytes(bytes: &[u8]) -> Result<Self, Error>
    where
        Self: Sized,
    {
        let mut u64_ary: [u8; 8] = [0; 8];
        let mut u32_len_ary: [u8; 4] = [0; 4];
        let (challenge_hash, rest) = bytes.split_at(32);
        let (difficulty, rest) = rest.split_at(8);
        u64_ary.copy_from_slice(&difficulty[0..8]);
        let difficulty = u64::from_be_bytes(u64_ary);
        let (sub_slot_iters, rest) = rest.split_at(8);
        u64_ary.copy_from_slice(&sub_slot_iters[0..8]);
        let sub_slot_iters = u64::from_be_bytes(u64_ary);
        let (signage_point_index, rest) = rest.split_at(1);
        let (sp_hash, rest) = rest.split_at(32);
        let (pool_dif_len, mut rest) = rest.split_at(4);
        u32_len_ary.copy_from_slice(&pool_dif_len[0..4]);
        let pool_dif_len = u32::from_be_bytes(u32_len_ary) as usize;
        let mut pool_difficulties = vec![];
        for _ in 0..pool_dif_len {
            let (pool_dif, r) = rest.split_at(48); //LENGTH OF Pool Difficulty //TODO Make this more dynamic
            pool_difficulties.push(PoolDifficulty::from_bytes(pool_dif)?);
            rest = r
        }
        Ok(Self {
            challenge_hash: challenge_hash.into(),
            difficulty,
            sub_slot_iters,
            signage_point_index: signage_point_index[0],
            sp_hash: sp_hash.into(),
            pool_difficulties,
        })
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct NewProofOfSpace {
    pub challenge_hash: Bytes32,
    pub sp_hash: Bytes32,
    pub plot_identifier: String,
    pub proof: ProofOfSpace,
    pub signage_point_index: u8,
}
impl ChiaSerialize for NewProofOfSpace {
    fn to_bytes(&self) -> Vec<u8> {
        let mut bytes: Vec<u8> = Vec::new();
        bytes.extend(self.challenge_hash.to_sized_bytes());
        bytes.extend(self.sp_hash.to_sized_bytes());
        bytes.extend((self.plot_identifier.len() as u32).to_be_bytes());
        bytes.extend(self.plot_identifier.as_bytes());
        bytes.extend(self.proof.to_bytes());
        bytes.push(self.signage_point_index);
        bytes
    }

    fn from_bytes(bytes: &[u8]) -> Result<Self, Error>
    where
        Self: Sized,
    {
        let mut u32_len_ary: [u8; 4] = [0; 4];
        let (challenge_hash, rest) = bytes.split_at(32);
        let (sp_hash, rest) = rest.split_at(32);
        let (plot_identifier_len, rest) = rest.split_at(4);
        u32_len_ary.copy_from_slice(&plot_identifier_len[0..4]);
        let len = u32::from_be_bytes(u32_len_ary) as usize;
        let (plot_identifier, rest) = rest.split_at(len);
        let (proof_of_space, last) = rest.split_at(rest.len() - 1);
        Ok(Self {
            challenge_hash: Bytes32::new(challenge_hash.to_vec()),
            sp_hash: Bytes32::new(sp_hash.to_vec()),
            plot_identifier: String::from_utf8(plot_identifier.to_vec())
                .map_err(|e| Error::new(ErrorKind::Other, format!("{:?}", e)))?,
            proof: ProofOfSpace::from_bytes(proof_of_space)?,
            signage_point_index: last[0],
        })
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RequestSignatures {
    pub plot_identifier: String,
    pub challenge_hash: Bytes32,
    pub sp_hash: Bytes32,
    pub messages: Vec<Bytes32>,
}
impl ChiaSerialize for RequestSignatures {
    fn to_bytes(&self) -> Vec<u8> {
        let mut bytes: Vec<u8> = Vec::new();
        bytes.extend((self.plot_identifier.len() as u32).to_be_bytes());
        bytes.extend(self.plot_identifier.as_bytes());
        bytes.extend(self.challenge_hash.to_sized_bytes());
        bytes.extend(self.sp_hash.to_sized_bytes());
        bytes.extend((self.messages.len() as u32).to_be_bytes());
        for msg in &self.messages {
            bytes.extend(msg.to_sized_bytes());
        }
        bytes
    }

    fn from_bytes(bytes: &[u8]) -> Result<Self, Error>
    where
        Self: Sized,
    {
        let mut u32_len_ary: [u8; 4] = [0; 4];
        let (plot_identifier_len, rest) = bytes.split_at(4);
        u32_len_ary.copy_from_slice(&plot_identifier_len[0..4]);
        let plot_identifier_len = u32::from_be_bytes(u32_len_ary) as usize;
        let (plot_identifier, rest) = rest.split_at(plot_identifier_len);
        let (challenge_hash, rest) = rest.split_at(32);
        let (sp_hash, rest) = rest.split_at(32);
        let (messages_len, mut rest) = rest.split_at(4);
        u32_len_ary.copy_from_slice(&messages_len[0..4]);
        let messages_len = u32::from_be_bytes(u32_len_ary) as usize;
        let mut messages = vec![];
        for _ in 0..messages_len {
            let (msg, r) = rest.split_at(32);
            messages.push(msg.into());
            rest = r
        }
        Ok(Self {
            plot_identifier: String::from_utf8(plot_identifier.to_vec())
                .map_err(|e| Error::new(ErrorKind::InvalidInput, e))?,
            challenge_hash: challenge_hash.into(),
            sp_hash: sp_hash.into(),
            messages,
        })
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RespondSignatures {
    pub plot_identifier: String,
    pub challenge_hash: Bytes32,
    pub sp_hash: Bytes32,
    pub local_pk: Bytes48,
    pub farmer_pk: Bytes48,
    pub message_signatures: Vec<(Bytes32, Bytes96)>,
}
impl ChiaSerialize for RespondSignatures {
    fn to_bytes(&self) -> Vec<u8> {
        let mut bytes: Vec<u8> = Vec::new();
        bytes.extend((self.plot_identifier.len() as u32).to_be_bytes());
        bytes.extend(self.plot_identifier.as_bytes());
        bytes.extend(self.challenge_hash.to_sized_bytes());
        bytes.extend(self.sp_hash.to_sized_bytes());
        bytes.extend(self.local_pk.to_sized_bytes());
        bytes.extend(self.farmer_pk.to_sized_bytes());
        bytes.extend((self.message_signatures.len() as u32).to_be_bytes());
        for (msg, sig) in &self.message_signatures {
            bytes.extend(msg.to_sized_bytes());
            bytes.extend(sig.to_sized_bytes());
        }
        bytes
    }

    fn from_bytes(bytes: &[u8]) -> Result<Self, Error>
    where
        Self: Sized,
    {
        let mut u32_len_ary: [u8; 4] = [0; 4];
        let (plot_identifier_len, rest) = bytes.split_at(4);
        u32_len_ary.copy_from_slice(&plot_identifier_len[0..4]);
        let plot_identifier_len = u32::from_be_bytes(u32_len_ary) as usize;
        let (plot_identifier, rest) = rest.split_at(plot_identifier_len);
        let (challenge_hash, rest) = rest.split_at(32);
        let (sp_hash, rest) = rest.split_at(32);
        let (local_pk, rest) = rest.split_at(48);
        let (farmer_pk, rest) = rest.split_at(48);
        let (message_signatures_len, mut rest) = rest.split_at(4);
        u32_len_ary.copy_from_slice(&message_signatures_len[0..4]);
        let message_signatures_len = u32::from_be_bytes(u32_len_ary) as usize;
        let mut message_signatures = vec![];
        for _ in 0..message_signatures_len {
            let (msg, r) = rest.split_at(32);
            let (sig, r) = r.split_at(96);
            message_signatures.push((msg.into(), sig.into()));
            rest = r
        }
        Ok(Self {
            plot_identifier: String::from_utf8(plot_identifier.to_vec())
                .map_err(|e| Error::new(ErrorKind::InvalidInput, e))?,
            challenge_hash: challenge_hash.into(),
            sp_hash: sp_hash.into(),
            local_pk: local_pk.into(),
            farmer_pk: farmer_pk.into(),
            message_signatures,
        })
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct Plot {
    pub filename: String,
    pub size: u8,
    pub plot_id: Bytes32,
    pub pool_public_key: Option<Bytes48>,
    pub pool_contract_puzzle_hash: Option<Bytes32>,
    pub plot_public_key: Bytes48,
    pub file_size: u64,
    pub time_modified: u64,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RequestPlots {}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RespondPlots {
    pub plots: Vec<Plot>,
    pub failed_to_open_filenames: Vec<String>,
    pub no_key_filenames: Vec<String>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct PlotSyncIdentifier {
    pub timestamp: u64,
    pub sync_id: u64,
    pub message_id: u64,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct PlotSyncStart {
    pub identifier: PlotSyncIdentifier,
    pub initial: bool,
    pub last_sync_id: u64,
    pub plot_file_count: u32,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct PlotSyncPathList {
    pub identifier: PlotSyncIdentifier,
    pub data: Vec<String>,
    //final
    pub is_final: bool,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct PlotSyncPlotList {
    pub identifier: PlotSyncIdentifier,
    pub data: Vec<Plot>,
    //final
    pub is_final: bool,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct PlotSyncDone {
    pub identifier: PlotSyncIdentifier,
    pub duration: u64,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct PlotSyncError {
    pub code: i16,
    pub message: String,
    pub expected_identifier: Option<PlotSyncIdentifier>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct PlotSyncResponse {
    pub identifier: PlotSyncIdentifier,
    pub message_type: i16,
    pub error: Option<PlotSyncError>,
}
