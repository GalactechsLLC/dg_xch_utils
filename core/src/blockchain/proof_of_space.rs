use crate::blockchain::sized_bytes::{Bytes32, Bytes48};
use crate::clvm::sexp::SExp;
use crate::consensus::constants::ConsensusConstants;
use crate::formatting::prep_hex_str;
use crate::traits::SizedBytes;
#[cfg(feature = "bls")]
use crate::utils::hash_256;
#[cfg(feature = "bls")]
use blst::min_pk::{AggregatePublicKey, PublicKey, SecretKey};
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use hex::{decode, encode};
use serde::de::Visitor;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use std::cmp::max;
use std::fmt;
use std::fmt::{Debug, Display, Formatter};
#[cfg(feature = "bls")]
use std::io::ErrorKind;
use std::io::{Cursor, Error};

pub const NUMBER_ZERO_BITS_PLOT_FILTER: i32 = 9;

#[derive(Clone, PartialEq, Eq)]
pub struct ProofBytes(Vec<u8>);

impl IntoIterator for ProofBytes {
    type Item = u8;
    type IntoIter = std::vec::IntoIter<Self::Item>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}
impl Display for ProofBytes {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "0x{}", encode(&self.0))
    }
}
impl Debug for ProofBytes {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "0x{}", encode(&self.0))
    }
}

impl ChiaSerialize for ProofBytes {
    fn to_bytes(&self, version: ChiaProtocolVersion) -> Result<Vec<u8>, Error>
    where
        Self: Sized,
    {
        ChiaSerialize::to_bytes(&self.0, version)
    }

    fn from_bytes(bytes: &mut Cursor<&[u8]>, version: ChiaProtocolVersion) -> Result<Self, Error>
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
        serializer.serialize_str(&format!("0x{}", encode(&self.0)))
    }
}

struct ProofBytesVisitor;

impl Visitor<'_> for ProofBytesVisitor {
    type Value = ProofBytes;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Expecting a hex String")
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

impl From<&ProofBytes> for SExp<'static> {
    fn from(bytes: &ProofBytes) -> SExp<'static> {
        SExp::from(bytes.0.clone())
    }
}

// Serialization is hand written: the wire format packs the proof version into bit 1 of the
// pool_contract_puzzle_hash Option prefix, which a per-field derive cannot express. A v1 proof
// writes prefix 0b00/0b01 and carries size; a v2 proof writes 0b10/0b11 and carries plot_index,
// meta_group and strength in its place.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct ProofOfSpace {
    pub challenge: Bytes32,
    pub pool_public_key: Option<Bytes48>,
    pub pool_contract_puzzle_hash: Option<Bytes32>,
    pub plot_public_key: Bytes48,
    /// 0 for a v1 proof, 1 for a v2 proof. The four v2 fields default on deserialize so JSON
    /// written before v2 proofs existed still reads as a v1 proof.
    #[serde(default)]
    pub version: u8,
    /// v2 only; zero on v1 proofs.
    #[serde(default)]
    pub plot_index: u16,
    /// v2 only; zero on v1 proofs.
    #[serde(default)]
    pub meta_group: u8,
    /// v2 only; zero on v1 proofs.
    #[serde(default)]
    pub strength: u8,
    /// v1 only; zero on v2 proofs, whose plot size is a network constant.
    pub size: u8,
    pub proof: ProofBytes,
}
impl ProofOfSpace {
    #[must_use]
    pub fn v1(
        challenge: Bytes32,
        pool_public_key: Option<Bytes48>,
        pool_contract_puzzle_hash: Option<Bytes32>,
        plot_public_key: Bytes48,
        size: u8,
        proof: ProofBytes,
    ) -> Self {
        Self {
            challenge,
            pool_public_key,
            pool_contract_puzzle_hash,
            plot_public_key,
            version: 0,
            plot_index: 0,
            meta_group: 0,
            strength: 0,
            size,
            proof,
        }
    }

    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn v2(
        challenge: Bytes32,
        pool_public_key: Option<Bytes48>,
        pool_contract_puzzle_hash: Option<Bytes32>,
        plot_public_key: Bytes48,
        plot_index: u16,
        meta_group: u8,
        strength: u8,
        proof: ProofBytes,
    ) -> Self {
        Self {
            challenge,
            pool_public_key,
            pool_contract_puzzle_hash,
            plot_public_key,
            version: 1,
            plot_index,
            meta_group,
            strength,
            size: 0,
            proof,
        }
    }

    #[must_use]
    pub fn get_plot_id(&self) -> Option<Bytes32> {
        if let (Some(_), Some(_)) = (&self.pool_public_key, &self.pool_contract_puzzle_hash) {
            //Invalid, Both cant be Some
            None
        } else if let (None, None) = (&self.pool_public_key, &self.pool_contract_puzzle_hash) {
            //Invalid, Both cant be None
            None
        } else if self.version == 1 {
            Some(calculate_plot_id_v2(
                self.strength,
                self.plot_public_key,
                self.pool_public_key,
                self.pool_contract_puzzle_hash,
                self.plot_index,
                self.meta_group,
            ))
        } else if let Some(contract) = self.pool_contract_puzzle_hash {
            Some(calculate_plot_id_puzzle_hash(
                contract,
                self.plot_public_key,
            ))
        } else {
            self.pool_public_key
                .map(|pub_key| calculate_plot_id_public_key(pub_key, self.plot_public_key))
        }
    }
}

impl ChiaSerialize for ProofOfSpace {
    fn to_bytes(&self, version: ChiaProtocolVersion) -> Result<Vec<u8>, Error> {
        let mut bytes = ChiaSerialize::to_bytes(&self.challenge, version)?;
        bytes.extend(ChiaSerialize::to_bytes(&self.pool_public_key, version)?);
        match self.version {
            0 => {
                bytes.extend(ChiaSerialize::to_bytes(
                    &self.pool_contract_puzzle_hash,
                    version,
                )?);
                bytes.extend(ChiaSerialize::to_bytes(&self.plot_public_key, version)?);
                bytes.extend(ChiaSerialize::to_bytes(&self.size, version)?);
            }
            1 => {
                if let Some(contract) = &self.pool_contract_puzzle_hash {
                    bytes.push(0b11);
                    bytes.extend(ChiaSerialize::to_bytes(contract, version)?);
                } else {
                    bytes.push(0b10);
                }
                bytes.extend(ChiaSerialize::to_bytes(&self.plot_public_key, version)?);
                bytes.extend(ChiaSerialize::to_bytes(&self.plot_index, version)?);
                bytes.extend(ChiaSerialize::to_bytes(&self.meta_group, version)?);
                bytes.extend(ChiaSerialize::to_bytes(&self.strength, version)?);
            }
            other => {
                return Err(Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("unknown proof of space version: {other}"),
                ));
            }
        }
        bytes.extend(ChiaSerialize::to_bytes(&self.proof, version)?);
        Ok(bytes)
    }

    fn from_bytes(bytes: &mut Cursor<&[u8]>, version: ChiaProtocolVersion) -> Result<Self, Error> {
        let challenge = ChiaSerialize::from_bytes(bytes, version)?;
        let pool_public_key: Option<Bytes48> = ChiaSerialize::from_bytes(bytes, version)?;
        let prefix: u8 = ChiaSerialize::from_bytes(bytes, version)?;
        // Only the Option flag (bit 0) and the proof version (bit 1) carry meaning; anything above
        // is malformed, exactly as a plain Option prefix above 1 always was.
        if prefix > 0b11 {
            return Err(Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("invalid proof of space prefix: {prefix}"),
            ));
        }
        let proof_version = prefix >> 1;
        let pool_contract_puzzle_hash: Option<Bytes32> = if prefix & 1 != 0 {
            Some(ChiaSerialize::from_bytes(bytes, version)?)
        } else {
            None
        };
        let plot_public_key = ChiaSerialize::from_bytes(bytes, version)?;
        if proof_version == 0 {
            let size = ChiaSerialize::from_bytes(bytes, version)?;
            let proof = ChiaSerialize::from_bytes(bytes, version)?;
            Ok(Self::v1(
                challenge,
                pool_public_key,
                pool_contract_puzzle_hash,
                plot_public_key,
                size,
                proof,
            ))
        } else {
            let plot_index = ChiaSerialize::from_bytes(bytes, version)?;
            let meta_group = ChiaSerialize::from_bytes(bytes, version)?;
            let strength = ChiaSerialize::from_bytes(bytes, version)?;
            let proof = ChiaSerialize::from_bytes(bytes, version)?;
            if pool_public_key.is_some() == pool_contract_puzzle_hash.is_some() {
                return Err(Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "a v2 proof needs exactly one of pool_public_key and pool_contract_puzzle_hash",
                ));
            }
            Ok(Self::v2(
                challenge,
                pool_public_key,
                pool_contract_puzzle_hash,
                plot_public_key,
                plot_index,
                meta_group,
                strength,
                proof,
            ))
        }
    }
}

impl From<&ProofOfSpace> for SExp<'static> {
    fn from(val: &ProofOfSpace) -> SExp<'static> {
        (&[
            SExp::from(&val.challenge),
            SExp::from(&val.pool_public_key),
            SExp::from(&val.pool_contract_puzzle_hash),
            SExp::from(&val.plot_public_key),
            SExp::from(&val.version),
            SExp::from(&val.plot_index),
            SExp::from(&val.meta_group),
            SExp::from(&val.strength),
            SExp::from(&val.size),
            SExp::from(&val.proof),
        ])
            .into()
    }
}
impl From<ProofOfSpace> for SExp<'static> {
    fn from(val: ProofOfSpace) -> SExp<'static> {
        (&val).into()
    }
}

/// `plot_group_id = sha256(strength || plot_pk || (pool_pk | contract_ph))`. Every plot in a group
/// shares this; the per plot id folds the index and meta group on top.
#[must_use]
pub fn calculate_plot_group_id_v2(
    strength: u8,
    plot_public_key: Bytes48,
    pool_public_key: Option<Bytes48>,
    pool_contract_puzzle_hash: Option<Bytes32>,
) -> Bytes32 {
    let mut hasher: Sha256 = Sha256::new();
    hasher.update([strength]);
    hasher.update(plot_public_key);
    if let Some(pool) = pool_public_key {
        hasher.update(pool);
    } else if let Some(contract) = pool_contract_puzzle_hash {
        hasher.update(contract);
    }
    let mut buf = [0u8; 32];
    hasher.finalize_into((&mut buf).into());
    buf.into()
}

/// `plot_id = sha256(plot_group_id || plot_index || meta_group)`, the index big endian.
#[must_use]
pub fn calculate_plot_id_v2(
    strength: u8,
    plot_public_key: Bytes48,
    pool_public_key: Option<Bytes48>,
    pool_contract_puzzle_hash: Option<Bytes32>,
    plot_index: u16,
    meta_group: u8,
) -> Bytes32 {
    let group = calculate_plot_group_id_v2(
        strength,
        plot_public_key,
        pool_public_key,
        pool_contract_puzzle_hash,
    );
    let mut hasher: Sha256 = Sha256::new();
    hasher.update(group);
    hasher.update(plot_index.to_be_bytes());
    hasher.update([meta_group]);
    let mut buf = [0u8; 32];
    hasher.finalize_into((&mut buf).into());
    buf.into()
}

#[must_use]
pub fn calculate_plot_id_public_key(pool_public_key: Bytes48, plot_public_key: Bytes48) -> Bytes32 {
    let mut to_hash: Vec<u8> = Vec::new();
    to_hash.extend(pool_public_key);
    to_hash.extend(plot_public_key);
    let mut hasher: Sha256 = Sha256::new();
    hasher.update(to_hash);
    let mut buf = [0u8; 32];
    hasher.finalize_into((&mut buf).into());
    buf.into()
}

#[must_use]
pub fn calculate_plot_id_puzzle_hash(
    pool_contract_puzzle_hash: Bytes32,
    plot_public_key: Bytes48,
) -> Bytes32 {
    let mut to_hash: Vec<u8> = Vec::new();
    to_hash.extend(pool_contract_puzzle_hash);
    to_hash.extend(plot_public_key);
    let mut hasher: Sha256 = Sha256::new();
    hasher.update(to_hash);
    let mut buf = [0u8; 32];
    hasher.finalize_into((&mut buf).into());
    buf.into()
}

/// The number of v1 phase-out epochs: always a power of two minus one, so it doubles as a bit mask
/// over the phase-out hash.
#[must_use]
pub fn num_phase_out_epochs(constants: &ConsensusConstants) -> u32 {
    (1u32 << constants.plot_v1_phase_out_epoch_bits) - 1
}

/// The height at which v1 proofs stop being valid: a block whose previous transaction block is at
/// or above this may not carry one.
#[must_use]
pub fn v1_cut_off_height(constants: &ConsensusConstants) -> u64 {
    u64::from(constants.hard_fork2_height)
        + u64::from(num_phase_out_epochs(constants)) * u64::from(constants.epoch_blocks)
}

/// Whether a v1 proof has been phased out.
///
/// Before hard fork 2 nothing is phased out. After it, each proof is retired at a randomly assigned
/// epoch: the phase-out byte of `hash(proof || tag)` is compared against a counter that ticks down
/// to the cut-off height, so the surviving fraction of v1 plots shrinks epoch by epoch until none
/// remain.
#[must_use]
pub fn is_v1_phased_out(
    proof: &[u8],
    prev_transaction_block_height: u32,
    constants: &ConsensusConstants,
) -> bool {
    if prev_transaction_block_height < constants.hard_fork2_height {
        return false;
    }
    let mask = num_phase_out_epochs(constants);
    debug_assert!(mask < 256, "phase-out mask must fit one byte");

    let cut_off = v1_cut_off_height(constants) as i64;
    let epoch_counter =
        (cut_off - i64::from(prev_transaction_block_height)) / i64::from(constants.epoch_blocks);
    if epoch_counter < 0 {
        return true;
    }

    let mut hasher = Sha256::new();
    hasher.update(proof);
    hasher.update(b"chia proof-of-space v1 phase-out");
    let mut digest = [0u8; 32];
    hasher.finalize_into((&mut digest).into());
    let proof_value = i64::from(digest[0] & mask as u8);
    proof_value >= epoch_counter
}

/// The v2 plot filter, on its own schedule: v2 plots start at five zero bits and relax by one at
/// each adjustment height, against v1's nine.
#[allow(clippy::cast_possible_wrap)]
#[must_use]
pub fn calculate_prefix_bits_v2(constants: &ConsensusConstants, height: u32) -> i8 {
    let mut prefix_bits = constants.number_zero_bits_plot_filter_v2 as i8;
    if height >= constants.plot_filter_v2_third_adjustment_height {
        prefix_bits -= 3;
    } else if height >= constants.plot_filter_v2_second_adjustment_height {
        prefix_bits -= 2;
    } else if height >= constants.plot_filter_v2_first_adjustment_height {
        prefix_bits -= 1;
    }
    max(0, prefix_bits)
}

#[allow(clippy::cast_possible_wrap)]
#[must_use]
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

#[allow(clippy::cast_sign_loss)]
#[must_use]
pub fn passes_plot_filter(
    prefix_bits: i8,
    plot_id: Bytes32,
    challenge_hash: Bytes32,
    signage_point: Bytes32,
) -> bool {
    if prefix_bits == 0 {
        true
    } else {
        let mut filter = [false; 256];
        let mut index = 0;
        for b in calculate_plot_filter_input(plot_id, challenge_hash, signage_point).bytes() {
            for i in (0..=7).rev() {
                filter[index] = ((b >> i) & 1) == 1;
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

#[must_use]
pub fn calculate_plot_filter_input(
    plot_id: Bytes32,
    challenge_hash: Bytes32,
    signage_point: Bytes32,
) -> Bytes32 {
    let mut hasher: Sha256 = Sha256::new();
    hasher.update(plot_id);
    hasher.update(challenge_hash);
    hasher.update(signage_point);
    let mut buf = [0u8; 32];
    hasher.finalize_into((&mut buf).into());
    buf.into()
}

#[must_use]
pub fn calculate_pos_challenge(
    plot_id: Bytes32,
    challenge_hash: Bytes32,
    signage_point: Bytes32,
) -> Bytes32 {
    let mut hasher: Sha256 = Sha256::new();
    hasher.update(calculate_plot_filter_input(
        plot_id,
        challenge_hash,
        signage_point,
    ));
    let mut buf = [0u8; 32];
    hasher.finalize_into((&mut buf).into());
    buf.into()
}

#[cfg(feature = "bls")]
pub fn generate_taproot_sk(
    local_pk: &PublicKey,
    farmer_pk: &PublicKey,
) -> Result<SecretKey, Error> {
    let mut taproot_message = vec![];
    let mut agg = AggregatePublicKey::from_public_key(local_pk);
    agg.add_public_key(farmer_pk, false)
        .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("{e:?}")))?;
    taproot_message.extend(agg.to_public_key().to_bytes());
    taproot_message.extend(local_pk.to_bytes());
    taproot_message.extend(farmer_pk.to_bytes());
    let taproot_hash = hash_256(&taproot_message);
    SecretKey::key_gen_v3(&taproot_hash, &[])
        .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("{e:?}")))
}

#[cfg(feature = "bls")]
pub fn generate_plot_public_key(
    local_pk: &PublicKey,
    farmer_pk: &PublicKey,
    include_taproot: bool,
) -> Result<PublicKey, Error> {
    let mut agg = AggregatePublicKey::from_public_key(local_pk);
    if include_taproot {
        let taproot_sk = generate_taproot_sk(local_pk, farmer_pk)?;
        agg.add_public_key(farmer_pk, false)
            .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("{e:?}")))?;
        agg.add_public_key(&taproot_sk.sk_to_pk(), false)
            .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("{e:?}")))?;
        Ok(agg.to_public_key())
    } else {
        agg.add_public_key(farmer_pk, false)
            .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("{e:?}")))?;
        Ok(agg.to_public_key())
    }
}

#[cfg(test)]
#[path = "../../tests/unit/blockchain/proof_of_space/tests.rs"]
mod tests;
