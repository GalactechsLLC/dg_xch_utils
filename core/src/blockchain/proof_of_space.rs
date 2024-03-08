use crate::blockchain::sized_bytes::{prep_hex_str, Bytes32, Bytes48, SizedBytes};
use crate::consensus::constants::ConsensusConstants;
use blst::min_pk::{AggregatePublicKey, PublicKey, SecretKey};
use dg_xch_macros::ChiaSerial;
use dg_xch_serialize::{hash_256, ChiaProtocolVersion, ChiaSerialize};
use hex::{decode, encode};
use serde::de::Visitor;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use std::cmp::max;
use std::fmt;
use std::fmt::{Debug, Display, Formatter};
use std::io::{Cursor, Error, ErrorKind};

pub const NUMBER_ZERO_BITS_PLOT_FILTER: i32 = 9;

#[derive(Clone, PartialEq, Eq)]
pub struct ProofBytes(Vec<u8>);

impl ProofBytes {
    pub fn iter(&self) -> std::slice::Iter<'_, u8> {
        self.0.iter()
    }
}
impl Display for ProofBytes {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&encode(&self.0))
    }
}
impl Debug for ProofBytes {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&encode(&self.0))
    }
}

impl ChiaSerialize for ProofBytes {
    fn to_bytes(&self, version: ChiaProtocolVersion) -> Vec<u8>
    where
        Self: Sized,
    {
        ChiaSerialize::to_bytes(&self.0, version)
    }

    fn from_bytes<T: AsRef<[u8]>>(
        bytes: &mut Cursor<T>,
        version: ChiaProtocolVersion,
    ) -> Result<Self, Error>
    where
        Self: Sized,
    {
        Ok(Self(ChiaSerialize::from_bytes(bytes, version)?))
    }
}

impl Serialize for ProofBytes {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&encode(&self.0))
    }
}

struct ProofBytesVisitor;

impl<'de> Visitor<'de> for ProofBytesVisitor {
    type Value = ProofBytes;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("Expecting a hex String")
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(ProofBytes(
            decode(prep_hex_str(value)).map_err(|e| serde::de::Error::custom(e.to_string()))?,
        ))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(ProofBytes(
            decode(prep_hex_str(&value)).map_err(|e| serde::de::Error::custom(e.to_string()))?,
        ))
    }
}

impl<'a> Deserialize<'a> for ProofBytes {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'a>,
    {
        match deserializer.deserialize_string(ProofBytesVisitor) {
            Ok(hex) => Ok(hex),
            Err(er) => Err(er),
        }
    }
}

impl AsRef<[u8]> for ProofBytes {
    fn as_ref(&self) -> &[u8] {
        self.0.as_ref()
    }
}

impl From<Vec<u8>> for ProofBytes {
    fn from(bytes: Vec<u8>) -> ProofBytes {
        ProofBytes(bytes)
    }
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct ProofOfSpace {
    pub challenge: Bytes32,
    pub pool_public_key: Option<Bytes48>,
    pub pool_contract_puzzle_hash: Option<Bytes32>,
    pub plot_public_key: Bytes48,
    pub size: u8,
    pub proof: ProofBytes,
}
impl ProofOfSpace {
    pub fn get_plot_id(&self) -> Option<Bytes32> {
        if let (Some(_), Some(_)) = (&self.pool_public_key, &self.pool_contract_puzzle_hash) {
            //Invalid, Both cant be Some
            None
        } else if let (None, None) = (&self.pool_public_key, &self.pool_contract_puzzle_hash) {
            //Invalid, Both cant be None
            None
        } else if let Some(contract) = &self.pool_contract_puzzle_hash {
            Some(calculate_plot_id_puzzle_hash(
                contract,
                &self.plot_public_key,
            ))
        } else {
            self.pool_public_key
                .as_ref()
                .map(|pub_key| calculate_plot_id_public_key(pub_key, &self.plot_public_key))
        }
    }
}

pub fn calculate_plot_id_public_key(
    pool_public_key: &Bytes48,
    plot_public_key: &Bytes48,
) -> Bytes32 {
    let mut to_hash: Vec<u8> = Vec::new();
    to_hash.extend(pool_public_key.to_sized_bytes());
    to_hash.extend(plot_public_key.to_sized_bytes());
    let mut hasher: Sha256 = Sha256::new();
    hasher.update(to_hash);
    Bytes32::new(&hasher.finalize())
}

pub fn calculate_plot_id_puzzle_hash(
    pool_contract_puzzle_hash: &Bytes32,
    plot_public_key: &Bytes48,
) -> Bytes32 {
    let mut to_hash: Vec<u8> = Vec::new();
    to_hash.extend(pool_contract_puzzle_hash.to_sized_bytes());
    to_hash.extend(plot_public_key.to_sized_bytes());
    let mut hasher: Sha256 = Sha256::new();
    hasher.update(to_hash);
    Bytes32::new(&hasher.finalize())
}

pub fn calculate_prefix_bits(constants: &ConsensusConstants, height: u32) -> i8 {
    let mut prefix_bits = constants.number_zero_bits_plot_filter as i8;
    if height >= constants.plot_filter_32_height {
        prefix_bits -= 4;
    } else if height >= constants.plot_filter_64_height {
        prefix_bits -= 3;
    } else if height >= constants.plot_filter_128_height {
        prefix_bits -= 2;
    } else if height >= constants.hard_fork_height {
        prefix_bits -= 1;
    }
    max(0, prefix_bits)
}

pub fn passes_plot_filter(
    prefix_bits: i8,
    plot_id: &Bytes32,
    challenge_hash: &Bytes32,
    signage_point: &Bytes32,
) -> bool {
    if prefix_bits == 0 {
        true
    } else {
        let mut filter = [false; 256];
        let mut index = 0;
        for b in calculate_plot_filter_input(plot_id, challenge_hash, signage_point).as_slice() {
            for i in (0..=7).rev() {
                filter[index] = (b >> i & 1) == 1;
                index += 1;
            }
        }
        for is_one in filter.iter().take(prefix_bits as usize) {
            if *is_one {
                return false;
            }
        }
        true
    }
}

pub fn calculate_plot_filter_input(
    plot_id: &Bytes32,
    challenge_hash: &Bytes32,
    signage_point: &Bytes32,
) -> Bytes32 {
    let mut hasher: Sha256 = Sha256::new();
    hasher.update(plot_id);
    hasher.update(challenge_hash);
    hasher.update(signage_point);
    Bytes32::new(&hasher.finalize())
}

pub fn calculate_pos_challenge(
    plot_id: &Bytes32,
    challenge_hash: &Bytes32,
    signage_point: &Bytes32,
) -> Bytes32 {
    let mut hasher: Sha256 = Sha256::new();
    hasher.update(calculate_plot_filter_input(
        plot_id,
        challenge_hash,
        signage_point,
    ));
    Bytes32::new(&hasher.finalize())
}

pub fn generate_taproot_sk(
    local_pk: &PublicKey,
    farmer_pk: &PublicKey,
) -> Result<SecretKey, Error> {
    let mut taproot_message = vec![];
    let mut agg = AggregatePublicKey::from_public_key(local_pk);
    agg.add_public_key(farmer_pk, false)
        .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("{:?}", e)))?;
    taproot_message.extend(agg.to_public_key().to_bytes());
    taproot_message.extend(local_pk.to_bytes());
    taproot_message.extend(farmer_pk.to_bytes());
    let taproot_hash = hash_256(&taproot_message);
    SecretKey::key_gen_v3(&taproot_hash, &[])
        .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("{:?}", e)))
}

pub fn generate_plot_public_key(
    local_pk: &PublicKey,
    farmer_pk: &PublicKey,
    include_taproot: bool,
) -> Result<PublicKey, Error> {
    let mut agg = AggregatePublicKey::from_public_key(local_pk);
    if include_taproot {
        let taproot_sk = generate_taproot_sk(local_pk, farmer_pk)?;
        agg.add_public_key(farmer_pk, false)
            .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("{:?}", e)))?;
        agg.add_public_key(&taproot_sk.sk_to_pk(), false)
            .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("{:?}", e)))?;
        Ok(agg.to_public_key())
    } else {
        agg.add_public_key(farmer_pk, false)
            .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("{:?}", e)))?;
        Ok(agg.to_public_key())
    }
}
