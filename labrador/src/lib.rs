//! Bounded subprocess protocol. Native proof code is linked only into the worker.
use std::{
    io::{Error, Read, Write},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

pub const MAX_BATCH: usize = 256;
pub const MAX_PROOF: usize = 1024 * 1024;
pub const MAX_MESSAGE: usize = 4096;
pub const MAGIC: &[u8; 8] = b"DGXLAB02";
// Source pin, ring, relation, coefficient encoding and worker math profile are
// consensus choices. A changed relation must receive a new ID and chain identity.
pub const PROFILE: &[u8; 32] = b"DGX-LAB-Q38-PACKED11-RAW-FAP2-v2";
pub const ZK_PROFILE: &[u8; 32] = b"DGX-LAB-Q38-PACKED11-ZKP-FAP2-v2";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    Raw,
    Zk,
}
impl Profile {
    pub const fn id(self) -> &'static [u8; 32] {
        match self {
            Self::Raw => PROFILE,
            Self::Zk => ZK_PROFILE,
        }
    }
    pub const fn zk(self) -> bool {
        matches!(self, Self::Zk)
    }
}
pub const REQUEST: &[u8; 8] = b"DGXLREQ2";
pub const MAX_REQUEST: usize = 2 * 1024 * 1024;

pub struct Input<'a> {
    pub public_key: &'a [u8; 897],
    pub message: &'a [u8],
}

/// Salt extraction also enforces the envelope's canonical length and profile.
pub fn unpack(proof: &[u8], count: usize) -> Result<(&[u8], &[u8]), Error> {
    unpack_profile(Profile::Raw, proof, count)
}

pub fn unpack_profile(
    profile: Profile,
    proof: &[u8],
    count: usize,
) -> Result<(&[u8], &[u8]), Error> {
    let offset = 42 + 40 * count;
    if count == 0
        || count > MAX_BATCH
        || proof.len() > MAX_PROOF
        || proof.len() < offset + 12
        || &proof[..8] != MAGIC
        || &proof[8..40] != profile.id()
        || usize::from(u16::from_be_bytes(proof[40..42].try_into().unwrap())) != count
        || &proof[offset..offset + 4] != b"FAP2"
        || proof[offset + 4] != 38
        || proof[offset + 5] != u8::from(profile.zk())
        || proof[offset + 7] != 0
        || u32::from_le_bytes(proof[offset + 8..offset + 12].try_into().unwrap()) as usize
            != proof.len() - offset - 12
    {
        return Err(Error::other("invalid LaBRADOR envelope"));
    }
    Ok((&proof[42..offset], &proof[offset..]))
}

fn request(
    profile: Profile,
    context: &[u8; 32],
    inputs: &[Input<'_>],
    prove: bool,
) -> Result<Vec<u8>, Error> {
    if inputs.is_empty() || inputs.len() > MAX_BATCH {
        return Err(Error::other("LaBRADOR batch limit"));
    }
    let mut data = Vec::new();
    data.extend_from_slice(REQUEST);
    data.push(2 * u8::from(profile.zk()) + u8::from(prove));
    data.extend_from_slice(&(inputs.len() as u16).to_be_bytes());
    data.extend_from_slice(context);
    for input in inputs {
        if input.public_key[0] != 9 || input.message.len() > MAX_MESSAGE {
            return Err(Error::other("LaBRADOR input limit"));
        }
        data.extend_from_slice(input.public_key);
        data.extend_from_slice(&(input.message.len() as u16).to_be_bytes());
        data.extend_from_slice(input.message);
    }
    Ok(data)
}

pub fn prove(
    context: &[u8; 32],
    inputs: &[Input<'_>],
    signatures: &[[u8; 666]],
) -> Result<Vec<u8>, Error> {
    prove_profile(Profile::Raw, context, inputs, signatures)
}

pub fn prove_profile(
    profile: Profile,
    context: &[u8; 32],
    inputs: &[Input<'_>],
    signatures: &[[u8; 666]],
) -> Result<Vec<u8>, Error> {
    if inputs.len() != signatures.len() {
        return Err(Error::other("LaBRADOR coverage mismatch"));
    }
    let mut data = request(profile, context, inputs, true)?;
    for signature in signatures {
        if signature[0] != 0x39 {
            return Err(Error::other("wrong Falcon signature profile"));
        }
        data.extend_from_slice(signature);
    }
    let proof = run(data)?;
    unpack_profile(profile, &proof, inputs.len())?;
    Ok(proof)
}

pub fn verify(context: &[u8; 32], inputs: &[Input<'_>], proof: &[u8]) -> Result<(), Error> {
    verify_profile(Profile::Raw, context, inputs, proof)
}

pub fn verify_profile(
    profile: Profile,
    context: &[u8; 32],
    inputs: &[Input<'_>],
    proof: &[u8],
) -> Result<(), Error> {
    unpack_profile(profile, proof, inputs.len())?;
    let mut data = request(profile, context, inputs, false)?;
    data.extend_from_slice(&(proof.len() as u32).to_be_bytes());
    data.extend_from_slice(proof);
    if run(data)? != b"VERIFIED" {
        return Err(Error::other("invalid LaBRADOR proof"));
    }
    Ok(())
}

fn run(data: Vec<u8>) -> Result<Vec<u8>, Error> {
    if data.len() > MAX_REQUEST {
        return Err(Error::other("LaBRADOR request limit"));
    }
    let executable = std::env::var_os("DGX_LABRADOR_WORKER")
        .map(std::path::PathBuf::from)
        .unwrap_or(std::env::current_exe()?.with_file_name("dg_xch_labrador_worker"));
    let mut child = Command::new(executable)
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut input = child.stdin.take().unwrap();
    let output = child.stdout.take().unwrap();
    let start = Instant::now();
    std::thread::scope(|scope| {
        let writer = scope.spawn(move || input.write_all(&data));
        let reader = scope.spawn(move || {
            let mut bytes = Vec::new();
            output
                .take((MAX_PROOF + 1) as u64)
                .read_to_end(&mut bytes)?;
            Ok::<_, Error>(bytes)
        });
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Ok(status),
                Ok(None) if start.elapsed() < Duration::from_secs(180) => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                result => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break Err(result
                        .err()
                        .unwrap_or_else(|| Error::other("LaBRADOR worker timeout")));
                }
            }
        };
        let wrote = writer
            .join()
            .map_err(|_| Error::other("LaBRADOR input thread"))?;
        let output = reader
            .join()
            .map_err(|_| Error::other("LaBRADOR output thread"))??;
        let status = status?;
        if !status.success() || output.len() > MAX_PROOF {
            return Err(Error::other(format!(
                "LaBRADOR worker failed ({status}, elapsed {:?}, output {} bytes)",
                start.elapsed(),
                output.len(),
            )));
        }
        wrote?;
        Ok(output)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_rejects_legacy_and_cross_profile_proofs() {
        for profile in [Profile::Raw, Profile::Zk] {
            let mut envelope = vec![0; 42 + 40 + 12];
            envelope[..8].copy_from_slice(MAGIC);
            envelope[8..40].copy_from_slice(profile.id());
            envelope[40..42].copy_from_slice(&1u16.to_be_bytes());
            envelope[82..86].copy_from_slice(b"FAP2");
            envelope[86] = 38;
            envelope[87] = u8::from(profile.zk());
            assert!(unpack_profile(profile, &envelope, 1).is_ok());
            let other = if profile.zk() {
                Profile::Raw
            } else {
                Profile::Zk
            };
            assert!(unpack_profile(other, &envelope, 1).is_err());
            envelope[..8].copy_from_slice(b"DGXLAB01");
            assert!(unpack_profile(profile, &envelope, 1).is_err());
            envelope[..8].copy_from_slice(MAGIC);
            envelope[39] = b'1';
            assert!(unpack_profile(profile, &envelope, 1).is_err());
        }
    }
}
