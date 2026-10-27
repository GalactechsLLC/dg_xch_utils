//! Versioned, bounded authorization trailers for the native block and farming wire types.
//! An absent trailer writes no bytes, preserving every existing BLS encoding. The
//! trailer is used only at message ends and after blocks in block lists (the next
//! block starts with its bounded sub-slot count, never this magic).
use super::SignatureScheme;
use super::falcon::{FalconSignature, FnDsa512V2};
use super::falcon_block::{
    FalconHeaderAuthorization, MAX_TRANSACTION_SIGNATURES, header_message, public_key_id,
    signature_reference,
};
use crate::blockchain::{
    full_block::FullBlock,
    sized_bytes::{Bytes32, Bytes48, Bytes96},
    unfinished_block::UnfinishedBlock,
};
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use std::io::{Cursor, Error, Read};

const MAGIC_DIRECT: &[u8; 8] = b"DGXAUT21";
const MAGIC_COMPACT: &[u8; 8] = b"DGXAUT22";
const MAGIC_AGGREGATE: &[u8; 8] = b"DGXAUT23";
const MAX_BYTES: usize = 1024 * 1024;
const MAX_HEADER_SIGNATURES: usize = 5;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FalconAuthorization {
    pub genesis: Bytes32,
    pub header: Vec<FalconHeaderAuthorization>,
    #[serde(with = "signatures")]
    pub transactions: Vec<FalconSignature>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub labrador: Option<Vec<u8>>,
}

#[derive(Clone, Default, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct AuthorizationExtension(pub Option<Box<FalconAuthorization>>);

/// Immutable evidence that every header signature was verified. It owns its
/// witness so mutating the original cannot invalidate the cached bindings.
/// Transaction signatures still require verification against execution output.
#[derive(Clone)]
pub struct VerifiedFalconAuthorization {
    authorization: FalconAuthorization,
    headers: Vec<VerifiedHeader>,
}

#[derive(Clone)]
struct VerifiedHeader {
    key: Bytes48,
    reference: Bytes96,
}

impl VerifiedFalconAuthorization {
    pub fn authorization(&self) -> &FalconAuthorization {
        &self.authorization
    }

    pub fn verify_header(&self, key: &Bytes48, message: &[u8], reference: &Bytes96) -> bool {
        self.headers
            .iter()
            .zip(&self.authorization.header)
            .any(|(binding, proof)| {
                binding.key == *key && binding.reference == *reference && proof.message == message
            })
    }
}

impl AuthorizationExtension {
    pub fn is_none(&self) -> bool {
        self.0.is_none()
    }
    pub fn as_ref(&self) -> Option<&FalconAuthorization> {
        self.0.as_deref()
    }
    pub fn as_mut(&mut self) -> Option<&mut FalconAuthorization> {
        self.0.as_deref_mut()
    }
}
impl From<FalconAuthorization> for AuthorizationExtension {
    fn from(value: FalconAuthorization) -> Self {
        Self(Some(Box::new(value)))
    }
}

impl FalconAuthorization {
    pub fn transaction_witness(&self) -> Result<super::AuthorizationWitness<'_>, Error> {
        match &self.labrador {
            Some(proof)
                if self.transactions.is_empty() && (54..=MAX_BYTES).contains(&proof.len()) =>
            {
                Ok(super::AuthorizationWitness::FalconLabrador(proof))
            }
            Some(_) => Err(Error::other(
                "mixed or malformed Falcon transaction evidence",
            )),
            None => Ok(super::AuthorizationWitness::FalconDirect(
                &self.transactions,
            )),
        }
    }
    pub fn verify_messages(
        &self,
        genesis: Bytes32,
        messages: &[(Bytes48, Vec<u8>, Bytes96)],
    ) -> bool {
        if !self.transactions.is_empty() || self.labrador.is_some() {
            return false;
        }
        let Ok(verified) = self.verified(genesis) else {
            return false;
        };
        let required: std::collections::BTreeSet<_> =
            messages.iter().map(|(_, _, s)| s.const_bytes()).collect();
        required.into_iter().collect::<Vec<_>>()
            == self
                .header
                .iter()
                .map(|p| p.reference(genesis).const_bytes())
                .collect::<Vec<_>>()
            && messages
                .iter()
                .all(|(key, message, signature)| verified.verify_header(key, message, signature))
    }
    pub fn retain_references(&mut self, refs: &[Bytes96]) {
        self.header
            .retain(|proof| refs.contains(&proof.reference(self.genesis)));
        self.canonicalize();
    }
    pub fn verify(&self, genesis: Bytes32) -> Result<(), Error> {
        self.verify_headers(genesis).map(|_| ())
    }
    pub fn verified(&self, genesis: Bytes32) -> Result<VerifiedFalconAuthorization, Error> {
        Ok(VerifiedFalconAuthorization {
            headers: self.verify_headers(genesis)?,
            authorization: self.clone(),
        })
    }
    fn verify_headers(&self, genesis: Bytes32) -> Result<Vec<VerifiedHeader>, Error> {
        self.transaction_witness()?;
        if self.genesis != genesis
            || self.header.len() > MAX_HEADER_SIGNATURES
            || self.transactions.len() > MAX_TRANSACTION_SIGNATURES
        {
            return Err(Error::other("wrong Falcon chain or witness limits"));
        }
        let mut previous = None;
        let mut headers = Vec::with_capacity(self.header.len());
        for proof in &self.header {
            if proof.message.len() > 1024 {
                return Err(Error::other("Falcon header message limit"));
            }
            // Derive each key commitment and reference once, then bind the cache
            // to the exact key/message whose signature is checked here.
            let key = public_key_id(&proof.public_key);
            let reference = signature_reference(genesis, key, &proof.message);
            if previous.is_some_and(|p| p >= reference.const_bytes())
                || !FnDsa512V2.verify(
                    &proof.public_key,
                    &header_message(genesis, &proof.message),
                    &proof.signature,
                )
            {
                return Err(Error::other(
                    "invalid or noncanonical Falcon header authorization",
                ));
            }
            previous = Some(reference.const_bytes());
            headers.push(VerifiedHeader { key, reference });
        }
        Ok(headers)
    }
    pub fn verify_header(&self, key: &Bytes48, message: &[u8], reference: &Bytes96) -> bool {
        self.header
            .iter()
            .any(|p| p.verify(self.genesis, key, message, reference))
    }
    pub fn canonicalize(&mut self) {
        self.header
            .sort_by_key(|p| p.reference(self.genesis).const_bytes());
        self.header
            .dedup_by_key(|p| p.reference(self.genesis).const_bytes());
    }
    pub fn check_full(
        &self,
        block: &FullBlock,
        genesis: Bytes32,
    ) -> Result<VerifiedFalconAuthorization, Error> {
        let verified = self.verified(genesis)?;
        if block.height() == 0 && block.foliage.foliage_block_data.pool_signature.is_some() {
            return Err(Error::other("unexpected genesis pool authorization"));
        }
        self.check_generator(block.transactions_generator.is_some())?;
        self.check_coverage(
            &block.foliage,
            [
                block.reward_chain_block.challenge_chain_sp_signature,
                block.reward_chain_block.reward_chain_sp_signature,
            ],
        )?;
        Ok(verified)
    }
    pub fn check_unfinished(
        &self,
        block: &UnfinishedBlock,
        genesis: Bytes32,
    ) -> Result<VerifiedFalconAuthorization, Error> {
        let verified = self.verified(genesis)?;
        self.check_generator(block.transactions_generator.is_some())?;
        self.check_coverage(
            &block.foliage,
            [
                block.reward_chain_block.challenge_chain_sp_signature,
                block.reward_chain_block.reward_chain_sp_signature,
            ],
        )?;
        Ok(verified)
    }
    fn check_generator(&self, present: bool) -> Result<(), Error> {
        if !present && (!self.transactions.is_empty() || self.labrador.is_some()) {
            return Err(Error::other(
                "transaction authorization without a generator",
            ));
        }
        Ok(())
    }
    fn check_coverage(
        &self,
        foliage: &crate::blockchain::foliage::Foliage,
        sp: [Bytes96; 2],
    ) -> Result<(), Error> {
        let mut refs = std::collections::BTreeSet::from([
            sp[0].const_bytes(),
            sp[1].const_bytes(),
            foliage.foliage_block_data_signature.const_bytes(),
        ]);
        for reference in [
            foliage.foliage_transaction_block_signature,
            foliage.foliage_block_data.pool_signature,
        ]
        .into_iter()
        .flatten()
        {
            refs.insert(reference.const_bytes());
        }
        if refs.into_iter().collect::<Vec<_>>()
            != self
                .header
                .iter()
                .map(|p| p.reference(self.genesis).const_bytes())
                .collect::<Vec<_>>()
            || (foliage.foliage_transaction_block_hash.is_none()
                && (!self.transactions.is_empty() || self.labrador.is_some()))
        {
            return Err(Error::other(
                "Falcon witness does not cover this block exactly",
            ));
        }
        Ok(())
    }
}

impl ChiaSerialize for AuthorizationExtension {
    fn to_bytes(&self, version: ChiaProtocolVersion) -> Result<Vec<u8>, Error> {
        let Some(auth) = self.as_ref() else {
            return Ok(vec![]);
        };
        auth.transaction_witness()?;
        if auth.header.len() > MAX_HEADER_SIGNATURES
            || auth.transactions.len() > MAX_TRANSACTION_SIGNATURES
        {
            return Err(Error::other("Falcon witness limits"));
        }
        let mut keys: Vec<_> = auth.header.iter().map(|proof| &proof.public_key).collect();
        keys.sort_unstable();
        keys.dedup();
        // Compact header-key encoding is used only when it saves space.
        // All three layouts are explicitly tagged for the FN-DSA v2 profile.
        let aggregate = auth.labrador.is_some();
        let compact = aggregate || keys.len() < auth.header.len();
        let mut payload = auth.genesis.to_bytes(version)?;
        if compact {
            payload.push(keys.len() as u8);
            for key in &keys {
                payload.extend_from_slice(*key);
            }
        }
        payload.push(auth.header.len() as u8);
        for proof in &auth.header {
            if proof.message.len() > 1024 {
                return Err(Error::other("Falcon header message limit"));
            }
            if compact {
                payload.push(
                    keys.binary_search(&&proof.public_key)
                        .map_err(|_| Error::other("missing Falcon header key"))?
                        as u8,
                );
            } else {
                payload.extend_from_slice(&proof.public_key);
            }
            (proof.message.len() as u16).append_bytes(&mut payload, version)?;
            payload.extend_from_slice(&proof.message);
            payload.extend_from_slice(&proof.signature);
        }
        if let Some(proof) = &auth.labrador {
            (proof.len() as u32).append_bytes(&mut payload, version)?;
            payload.extend_from_slice(proof);
        } else {
            (auth.transactions.len() as u32).append_bytes(&mut payload, version)?;
            for signature in &auth.transactions {
                payload.extend_from_slice(signature);
            }
        }
        if payload.len() > MAX_BYTES {
            return Err(Error::other("Falcon authorization too large"));
        }
        let mut bytes = if aggregate {
            MAGIC_AGGREGATE
        } else if compact {
            MAGIC_COMPACT
        } else {
            MAGIC_DIRECT
        }
        .to_vec();
        (payload.len() as u32).append_bytes(&mut bytes, version)?;
        bytes.extend(payload);
        Ok(bytes)
    }
    fn from_bytes(bytes: &mut Cursor<&[u8]>, version: ChiaProtocolVersion) -> Result<Self, Error> {
        let start = usize::try_from(bytes.position()).map_err(Error::other)?;
        let input = *bytes.get_ref();
        let magic = input.get(start..start.saturating_add(8));
        let aggregate = magic == Some(MAGIC_AGGREGATE);
        let compact = if aggregate || magic == Some(MAGIC_COMPACT) {
            true
        } else if magic == Some(MAGIC_DIRECT) {
            false
        } else if matches!(
            input.get(start..start.saturating_add(7)),
            Some(b"DGXAUTH" | b"DGXAUT2")
        ) {
            return Err(Error::other("unsupported Falcon authorization version"));
        } else {
            return Ok(Self::default());
        };
        bytes.set_position(bytes.position() + 8);
        let length = u32::from_bytes(bytes, version)? as usize;
        if length > MAX_BYTES {
            return Err(Error::other("Falcon authorization too large"));
        }
        let offset = bytes.position() as usize;
        let raw = input
            .get(
                offset
                    ..offset
                        .checked_add(length)
                        .ok_or_else(|| Error::other("length overflow"))?,
            )
            .ok_or_else(|| Error::other("truncated Falcon authorization"))?;
        let mut cursor = Cursor::new(raw);
        let genesis = Bytes32::from_bytes(&mut cursor, version)?;
        let mut keys = Vec::new();
        if compact {
            let count = u8::from_bytes(&mut cursor, version)? as usize;
            if count > MAX_HEADER_SIGNATURES
                || (!aggregate && (count == 0 || count == MAX_HEADER_SIGNATURES))
            {
                return Err(Error::other("invalid Falcon key table length"));
            }
            for _ in 0..count {
                let mut key = [0; 897];
                cursor.read_exact(&mut key)?;
                if keys.last().is_some_and(|previous| previous >= &key) {
                    return Err(Error::other("noncanonical Falcon key table"));
                }
                keys.push(key);
            }
        }
        let count = u8::from_bytes(&mut cursor, version)? as usize;
        if count > MAX_HEADER_SIGNATURES
            || (compact && (keys.len() > count || (!aggregate && keys.len() == count)))
        {
            return Err(Error::other("too many Falcon header signatures"));
        }
        let mut used_keys = [false; MAX_HEADER_SIGNATURES];
        let mut header = Vec::with_capacity(count);
        for _ in 0..count {
            let mut public_key = [0; 897];
            if compact {
                let index = u8::from_bytes(&mut cursor, version)? as usize;
                public_key = *keys
                    .get(index)
                    .ok_or_else(|| Error::other("invalid Falcon key table index"))?;
                used_keys[index] = true;
            } else {
                cursor.read_exact(&mut public_key)?;
            }
            let length = u16::from_bytes(&mut cursor, version)? as usize;
            if length > 1024 {
                return Err(Error::other("Falcon header message limit"));
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
        if compact && used_keys[..keys.len()].contains(&false) {
            return Err(Error::other("unused Falcon key table entry"));
        }
        let count = u32::from_bytes(&mut cursor, version)? as usize;
        if (!aggregate && count > MAX_TRANSACTION_SIGNATURES)
            || (aggregate && !(54..=MAX_BYTES).contains(&count))
        {
            return Err(Error::other("too many Falcon transaction signatures"));
        }
        let mut transactions = Vec::new();
        let labrador = if aggregate {
            if count > raw.len().saturating_sub(cursor.position() as usize) {
                return Err(Error::other("truncated LaBRADOR proof"));
            }
            let mut proof = vec![0; count];
            cursor.read_exact(&mut proof)?;
            Some(proof)
        } else {
            if count > raw.len().saturating_sub(cursor.position() as usize) / 666 {
                return Err(Error::other("truncated Falcon signatures"));
            }
            transactions.reserve(count);
            for _ in 0..count {
                let mut signature = [0; 666];
                cursor.read_exact(&mut signature)?;
                transactions.push(signature);
            }
            None
        };
        if cursor.position() as usize != raw.len() {
            return Err(Error::other("trailing Falcon authorization bytes"));
        }
        bytes.set_position((offset + length) as u64);
        Ok(FalconAuthorization {
            genesis,
            header,
            transactions,
            labrador,
        }
        .into())
    }
}

pub mod array {
    use crate::blockchain::sized_bytes::SizedBytesImpl;
    use serde::{Deserialize, Serialize};
    pub fn serialize<S: serde::Serializer, const N: usize>(
        value: &[u8; N],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        SizedBytesImpl::<N>::const_new(*value).serialize(serializer)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>, const N: usize>(
        deserializer: D,
    ) -> Result<[u8; N], D::Error> {
        Ok(SizedBytesImpl::<N>::deserialize(deserializer)?.const_bytes())
    }
}
mod signatures {
    use crate::blockchain::sized_bytes::SizedBytesImpl;
    use serde::{Deserialize, Serialize};
    pub fn serialize<S: serde::Serializer>(
        value: &[[u8; 666]],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        value
            .iter()
            .copied()
            .map(SizedBytesImpl::<666>::const_new)
            .collect::<Vec<_>>()
            .serialize(serializer)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<[u8; 666]>, D::Error> {
        Ok(Vec::<SizedBytesImpl<666>>::deserialize(deserializer)?
            .into_iter()
            .map(|s| s.const_bytes())
            .collect())
    }
}
