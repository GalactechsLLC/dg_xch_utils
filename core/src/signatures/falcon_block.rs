//! Experimental development-chain wire envelope. The shared block layout carries
//! key commitments and deterministic signature references; real Falcon evidence is
//! detached here. References commit to the author and message, never random salts.
//! A bare projected FullBlock is NOT a Falcon block and must not be accepted.

use super::falcon::{FalconPublicKey, FalconSignature, FalconSigner, FnDsa512V2};
use super::{SignatureScheme, SignatureSigner};
use crate::blockchain::full_block::FullBlock;
use crate::blockchain::sized_bytes::{Bytes32, Bytes48, Bytes96};
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use sha2::{Digest, Sha384};
use std::collections::BTreeSet;
use std::io::{Cursor, Error, ErrorKind, Read};

pub const MAX_ENVELOPE_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_TRANSACTION_SIGNATURES: usize = 1024;
const MAGIC: &[u8; 8] = b"DGXFAL02";

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FalconHeaderAuthorization {
    #[serde(with = "super::extension::array")]
    pub public_key: FalconPublicKey,
    pub message: Vec<u8>,
    #[serde(with = "super::extension::array")]
    pub signature: FalconSignature,
}

pub fn public_key_id(public_key: &FalconPublicKey) -> Bytes48 {
    let mut hash = Sha384::new();
    hash.update(b"dg_xch/falcon/plot-key/v2\0");
    hash.update(public_key);
    Bytes48::const_new(hash.finalize().into())
}

pub fn header_message(genesis: Bytes32, message: &[u8]) -> Vec<u8> {
    let mut result = b"dg_xch/falcon/header/v2\0".to_vec();
    result.extend_from_slice(genesis.as_ref());
    result.extend_from_slice(&(message.len() as u64).to_be_bytes());
    result.extend_from_slice(message);
    result
}

pub fn signature_reference(genesis: Bytes32, key: Bytes48, message: &[u8]) -> Bytes96 {
    let mut hash = Sha384::new();
    hash.update(b"dg_xch/falcon/header-reference/v2\0");
    hash.update(key);
    hash.update(header_message(genesis, message));
    let mut reference = [0; 96];
    reference[1..49].copy_from_slice(&hash.finalize());
    // A zero first byte is deliberately not a valid compressed BLS signature.
    Bytes96::const_new(reference)
}

impl FalconHeaderAuthorization {
    pub fn sign(signer: &FalconSigner, genesis: Bytes32, message: &[u8]) -> Result<Self, Error> {
        if message.len() > 1024 {
            return Err(invalid("header message too large"));
        }
        Ok(Self {
            public_key: signer.public_key(),
            message: message.to_vec(),
            signature: signer
                .sign_message(&header_message(genesis, message))
                .map_err(Error::other)?,
        })
    }

    pub fn reference(&self, genesis: Bytes32) -> Bytes96 {
        signature_reference(genesis, public_key_id(&self.public_key), &self.message)
    }

    pub fn verify(
        &self,
        genesis: Bytes32,
        key: &Bytes48,
        message: &[u8],
        reference: &Bytes96,
    ) -> bool {
        public_key_id(&self.public_key) == *key
            && self.message == message
            && self.reference(genesis) == *reference
            && FnDsa512V2.verify(
                &self.public_key,
                &header_message(genesis, message),
                &self.signature,
            )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FalconBlock {
    pub genesis: Bytes32,
    pub block: FullBlock,
    /// Sorted by reference; identical header requirements share one proof.
    pub header: Vec<FalconHeaderAuthorization>,
    /// One signature per execution occurrence, with no deduplication.
    pub transactions: Vec<FalconSignature>,
}

impl FalconBlock {
    pub fn check_shape(&self, genesis: Bytes32) -> Result<(), Error> {
        if self.genesis != genesis
            || self.header.len() > 5
            || self.transactions.len() > MAX_TRANSACTION_SIGNATURES
        {
            return Err(invalid("wrong Falcon chain or witness limits"));
        }
        let b = &self.block;
        let mut required = BTreeSet::from([
            b.reward_chain_block
                .challenge_chain_sp_signature
                .const_bytes(),
            b.reward_chain_block.reward_chain_sp_signature.const_bytes(),
            b.foliage.foliage_block_data_signature.const_bytes(),
        ]);
        if let Some(s) = b.foliage.foliage_transaction_block_signature {
            required.insert(s.const_bytes());
        }
        if b.height() == 0 && b.foliage.foliage_block_data.pool_signature.is_some() {
            return Err(invalid("unexpected genesis pool authorization"));
        }
        if let Some(s) = b.foliage.foliage_block_data.pool_signature {
            required.insert(s.const_bytes());
        }
        let actual: Vec<_> = self
            .header
            .iter()
            .map(|a| a.reference(genesis).const_bytes())
            .collect();
        if actual != required.into_iter().collect::<Vec<_>>()
            || self.header.iter().any(|a| a.message.len() > 1024)
        {
            return Err(invalid("Falcon header witness coverage or order"));
        }
        if b.foliage_transaction_block.is_none() && !self.transactions.is_empty() {
            return Err(invalid("transaction witnesses on a non-transaction block"));
        }
        Ok(())
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        self.check_shape(self.genesis)?;
        let version = ChiaProtocolVersion::default();
        let block = self.block.to_bytes(version)?;
        let mut result = MAGIC.to_vec();
        self.genesis.append_bytes(&mut result, version)?;
        (block.len() as u32).append_bytes(&mut result, version)?;
        result.extend_from_slice(&block);
        result.push(self.header.len() as u8);
        for proof in &self.header {
            result.extend_from_slice(&proof.public_key);
            (proof.message.len() as u16).append_bytes(&mut result, version)?;
            result.extend_from_slice(&proof.message);
            result.extend_from_slice(&proof.signature);
        }
        (self.transactions.len() as u32).append_bytes(&mut result, version)?;
        for signature in &self.transactions {
            result.extend_from_slice(signature);
        }
        if result.len() > MAX_ENVELOPE_BYTES {
            return Err(invalid("Falcon block too large"));
        }
        Ok(result)
    }

    pub fn from_bytes(input: &[u8], genesis: Bytes32) -> Result<Self, Error> {
        if input.len() > MAX_ENVELOPE_BYTES || input.get(..8) != Some(MAGIC) {
            return Err(invalid("invalid Falcon block envelope"));
        }
        let version = ChiaProtocolVersion::default();
        let mut cursor = Cursor::new(&input[8..]);
        let network = Bytes32::from_bytes(&mut cursor, version)?;
        if network != genesis {
            return Err(invalid("wrong Falcon genesis"));
        }
        let length = u32::from_bytes(&mut cursor, version)? as usize;
        let start = 8 + cursor.position() as usize;
        let raw = input
            .get(
                start
                    ..start
                        .checked_add(length)
                        .ok_or_else(|| invalid("block length overflow"))?,
            )
            .ok_or_else(|| invalid("truncated block"))?;
        let mut block_cursor = Cursor::new(raw);
        let block = FullBlock::from_bytes(&mut block_cursor, version)?;
        if block_cursor.position() as usize != length {
            return Err(invalid("trailing projected block bytes"));
        }
        cursor.set_position(cursor.position() + length as u64);
        let count = u8::from_bytes(&mut cursor, version)? as usize;
        if count > 5 {
            return Err(invalid("too many header signatures"));
        }
        let mut header = Vec::with_capacity(count);
        for _ in 0..count {
            let mut public_key = [0; 897];
            cursor.read_exact(&mut public_key)?;
            let length = u16::from_bytes(&mut cursor, version)? as usize;
            if length > 1024 {
                return Err(invalid("header message too long"));
            }
            let mut message = vec![0; length];
            cursor.read_exact(&mut message)?;
            let mut signature = [0; 666];
            cursor.read_exact(&mut signature)?;
            header.push(FalconHeaderAuthorization {
                public_key,
                message,
                signature,
            });
        }
        let count = u32::from_bytes(&mut cursor, version)? as usize;
        if count > MAX_TRANSACTION_SIGNATURES {
            return Err(invalid("too many transaction signatures"));
        }
        let mut transactions = Vec::with_capacity(count);
        for _ in 0..count {
            let mut signature = [0; 666];
            cursor.read_exact(&mut signature)?;
            transactions.push(signature);
        }
        if cursor.position() as usize + 8 != input.len() {
            return Err(invalid("trailing Falcon envelope bytes"));
        }
        let result = Self {
            genesis: network,
            block,
            header,
            transactions,
        };
        result.check_shape(genesis)?;
        Ok(result)
    }
}

fn invalid(message: &str) -> Error {
    Error::new(ErrorKind::InvalidData, message)
}
