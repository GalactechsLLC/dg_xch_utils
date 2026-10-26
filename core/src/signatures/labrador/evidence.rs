//! Composition preserves wallet proofs and their ordered statements. It does not
//! claim recursive compression: validators check each proof independently.
use super::{LabradorProfile, verify_profile};
use crate::{
    blockchain::sized_bytes::Bytes32,
    signatures::{
        AuthorizationMode, AuthorizationRules, AuthorizationVerifier, AuthorizationWitness,
        SignatureRequirement,
        extension::FalconAuthorization,
        falcon::{FalconDirect, FalconSignature},
        falcon_block::MAX_TRANSACTION_SIGNATURES,
    },
};
use std::{
    collections::{HashMap, HashSet},
    io::Error,
};

const MAGIC: &[u8; 8] = b"DGXLSET2";
// Bound verification work as well as wire size; leave room for block headers.
pub const MAX_ZK_PROOFS: usize = 4;
const MAX_BYTES: usize = 1024 * 1024 - 8192;
type Key = (Bytes32, u32);

fn key(requirement: &SignatureRequirement) -> Result<Key, Error> {
    match requirement {
        SignatureRequirement::Falcon { occurrence, .. } => {
            Ok((occurrence.coin_id, occurrence.condition_index))
        }
        _ => Err(Error::other("non-Falcon proof requirement")),
    }
}

struct Group<'a> {
    indices: Vec<usize>,
    proof: bool,
    bytes: &'a [u8],
}

fn take<'a>(input: &mut &'a [u8], length: usize) -> Result<&'a [u8], Error> {
    if input.len() < length {
        return Err(Error::other("truncated wallet proof set"));
    }
    let (value, rest) = input.split_at(length);
    *input = rest;
    Ok(value)
}
fn u16be(input: &mut &[u8]) -> Result<usize, Error> {
    Ok(u16::from_be_bytes(take(input, 2)?.try_into().unwrap()) as usize)
}

fn groups(bytes: &[u8], count: usize) -> Result<Vec<Group<'_>>, Error> {
    if count == 0 || count > MAX_TRANSACTION_SIGNATURES || bytes.len() > MAX_BYTES {
        return Err(Error::other("wallet proof set limit"));
    }
    if !bytes.starts_with(MAGIC) {
        dg_xch_labrador::unpack_profile(dg_xch_labrador::Profile::Zk, bytes, count)?;
        return Ok(vec![Group {
            indices: (0..count).collect(),
            proof: true,
            bytes,
        }]);
    }
    let mut input = &bytes[8..];
    let n = u16be(&mut input)?;
    if n == 0 || n > MAX_ZK_PROOFS + 1 {
        return Err(Error::other("wallet proof group limit"));
    }
    let mut covered = HashSet::new();
    let mut result = Vec::new();
    let mut proofs = 0;
    let mut direct = false;
    let mut previous = None;
    for _ in 0..n {
        let kind = take(&mut input, 1)?[0];
        let length = u16be(&mut input)?;
        if kind > 1 || length == 0 || length > count {
            return Err(Error::other("invalid wallet proof group"));
        }
        let mut indices = Vec::with_capacity(length);
        for _ in 0..length {
            let index = u16be(&mut input)?;
            if index >= count || !covered.insert(index) {
                return Err(Error::other("duplicate or out-of-range proof coverage"));
            }
            indices.push(index);
        }
        let minimum = *indices.iter().min().unwrap();
        if previous.is_some_and(|p| p >= minimum) {
            return Err(Error::other("noncanonical proof group order"));
        }
        previous = Some(minimum);
        let size = u32::from_be_bytes(take(&mut input, 4)?.try_into().unwrap()) as usize;
        let bytes = take(&mut input, size)?;
        if kind == 1 {
            proofs += 1;
            dg_xch_labrador::unpack_profile(dg_xch_labrador::Profile::Zk, bytes, length)?;
        } else {
            if direct || size != 666 * length || indices.windows(2).any(|p| p[0] >= p[1]) {
                return Err(Error::other("noncanonical direct proof group"));
            }
            direct = true;
        }
        result.push(Group {
            indices,
            proof: kind == 1,
            bytes,
        });
    }
    if !input.is_empty() || covered.len() != count || proofs == 0 || proofs > MAX_ZK_PROOFS {
        return Err(Error::other(
            "incomplete or excessive wallet proof coverage",
        ));
    }
    if result.len() == 1 && result[0].indices.iter().copied().eq(0..count) {
        return Err(Error::other("redundant wallet proof set envelope"));
    }
    Ok(result)
}

/// Counts are structural only; callers must still verify the executed statement.
pub fn evidence_stats(count: usize, bytes: &[u8]) -> Result<(usize, usize, usize), Error> {
    let groups = groups(bytes, count)?;
    Ok(groups.iter().fold((0, 0, 0), |(p, z, d), g| {
        if g.proof {
            (p + 1, z + g.indices.len(), d)
        } else {
            (p, z, d + g.indices.len())
        }
    }))
}

pub fn verify_evidence(
    profile: LabradorProfile,
    genesis: Bytes32,
    requirements: &[SignatureRequirement],
    bytes: &[u8],
) -> Result<(), Error> {
    if profile == LabradorProfile::Packed11V2 {
        return verify_profile(profile, genesis, requirements, bytes);
    }
    let mut seen = HashSet::new();
    for requirement in requirements {
        if !seen.insert(key(requirement)?) {
            return Err(Error::other("duplicate proof occurrence"));
        }
    }
    // Fully parse and check exact coverage before starting any native workers.
    for group in groups(bytes, requirements.len())? {
        let selected: Vec<_> = group
            .indices
            .iter()
            .map(|&i| requirements[i].clone())
            .collect();
        if group.proof {
            verify_profile(profile, genesis, &selected, group.bytes)?;
        } else {
            let signatures = group.bytes.as_chunks::<666>().0;
            FalconDirect
                .verify(
                    &AuthorizationRules::for_mode(AuthorizationMode::FalconDirect)
                        .map_err(Error::other)?,
                    &selected,
                    &AuthorizationWitness::FalconDirect(signatures),
                )
                .map_err(Error::other)?;
        }
    }
    Ok(())
}

/// Rebinds existing evidence to the final execution order without the hidden
/// signatures. `finish` checks that every requirement survives unchanged once.
#[derive(Default)]
pub struct Evidence {
    direct: HashMap<Key, (SignatureRequirement, FalconSignature)>,
    proofs: Vec<(Vec<SignatureRequirement>, Vec<u8>)>,
    seen: HashSet<Key>,
}

impl Evidence {
    pub fn proof_count(&self) -> usize {
        self.proofs.len()
    }

    pub fn add(
        &mut self,
        requirements: &[SignatureRequirement],
        witness: AuthorizationWitness<'_>,
    ) -> Result<(), Error> {
        for requirement in requirements {
            if !self.seen.insert(key(requirement)?) {
                return Err(Error::other("duplicate evidence occurrence"));
            }
        }
        match witness {
            AuthorizationWitness::FalconDirect(signatures)
                if signatures.len() == requirements.len() =>
            {
                for (requirement, signature) in requirements.iter().zip(signatures) {
                    self.direct
                        .insert(key(requirement)?, (requirement.clone(), *signature));
                }
            }
            AuthorizationWitness::FalconLabrador(bytes) => {
                for group in groups(bytes, requirements.len())? {
                    if group.proof {
                        self.proofs.push((
                            group
                                .indices
                                .iter()
                                .map(|&i| requirements[i].clone())
                                .collect(),
                            group.bytes.to_vec(),
                        ));
                    } else {
                        for (&index, signature) in
                            group.indices.iter().zip(group.bytes.as_chunks::<666>().0)
                        {
                            let requirement = &requirements[index];
                            self.direct
                                .insert(key(requirement)?, (requirement.clone(), *signature));
                        }
                    }
                }
            }
            _ => return Err(Error::other("unsupported wallet evidence")),
        }
        if self.proofs.len() > MAX_ZK_PROOFS {
            return Err(Error::other("too many wallet proofs"));
        }
        Ok(())
    }

    pub fn finish(
        self,
        genesis: Bytes32,
        requirements: &[SignatureRequirement],
    ) -> Result<FalconAuthorization, Error> {
        let mut remaining: HashMap<_, _> = requirements
            .iter()
            .enumerate()
            .map(|(i, r)| Ok((key(r)?, (i, r))))
            .collect::<Result<_, Error>>()?;
        if remaining.len() != requirements.len() || remaining.len() != self.seen.len() {
            return Err(Error::other("evidence coverage changed during assembly"));
        }
        let mut groups: Vec<(Vec<usize>, bool, Vec<u8>)> = Vec::new();
        for (selected, bytes) in self.proofs {
            let indices = selected
                .iter()
                .map(|r| {
                    let (i, actual) = remaining
                        .remove(&key(r)?)
                        .ok_or_else(|| Error::other("missing proof occurrence"))?;
                    if r != actual {
                        return Err(Error::other("proof statement changed during assembly"));
                    }
                    Ok(i)
                })
                .collect::<Result<Vec<_>, Error>>()?;
            groups.push((indices, true, bytes));
        }
        let mut direct = Vec::new();
        for (k, (r, signature)) in self.direct {
            let (i, actual) = remaining
                .remove(&k)
                .ok_or_else(|| Error::other("missing direct occurrence"))?;
            if &r != actual {
                return Err(Error::other("direct statement changed during assembly"));
            }
            direct.push((i, signature));
        }
        if !remaining.is_empty() {
            return Err(Error::other("uncovered authorization"));
        }
        direct.sort_by_key(|(i, _)| *i);
        let mut auth = FalconAuthorization {
            genesis,
            header: vec![],
            transactions: vec![],
            labrador: None,
        };
        if groups.is_empty() {
            auth.transactions = direct.into_iter().map(|(_, s)| s).collect();
            return Ok(auth);
        }
        if !direct.is_empty() {
            groups.push((
                direct.iter().map(|(i, _)| *i).collect(),
                false,
                direct.into_iter().flat_map(|(_, s)| s).collect(),
            ));
        }
        groups.sort_by_key(|(indices, _, _)| *indices.iter().min().unwrap());
        if groups.len() == 1 && groups[0].0.iter().copied().eq(0..requirements.len()) {
            auth.labrador = Some(groups.remove(0).2);
        } else {
            let mut bytes = MAGIC.to_vec();
            bytes.extend_from_slice(&(groups.len() as u16).to_be_bytes());
            for (indices, proof, data) in groups {
                bytes.push(u8::from(proof));
                bytes.extend_from_slice(&(indices.len() as u16).to_be_bytes());
                for i in indices {
                    bytes.extend_from_slice(&(i as u16).to_be_bytes());
                }
                bytes.extend_from_slice(&(data.len() as u32).to_be_bytes());
                bytes.extend_from_slice(&data);
            }
            auth.labrador = Some(bytes);
        }
        // Includes the total byte/work limit before a producer commits the block.
        evidence_stats(requirements.len(), auth.labrador.as_ref().unwrap())?;
        Ok(auth)
    }
}
