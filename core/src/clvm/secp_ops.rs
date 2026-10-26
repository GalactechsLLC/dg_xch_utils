use crate::clvm::arena::{Arena, NodePtr};
use crate::clvm::pure_ops::OpOut;
use crate::clvm::utils::{atom, check_cost, split};
use crate::errors::ClvmError;
use k256::ecdsa::signature::hazmat::PrehashVerifier;

/// Both CLVM secp operators verify a 32-byte digest and a compact (r, s) signature.
/// Use prehash verification: hashing the digest again changes the signed message.
pub fn op_secp_verify(
    arena: &Arena,
    args: NodePtr,
    max_cost: u64,
    r1: bool,
) -> Result<(u64, OpOut), ClvmError> {
    let (name, cost) = if r1 {
        ("secp256r1_verify", 1_850_000)
    } else {
        ("secp256k1_verify", 1_300_000)
    };
    check_cost(cost, max_cost)?;
    if !arena.arg_count_is(args, 3) {
        return Err(ClvmError::InvalidOperandArgs(name, 3));
    }
    let (key, rest) = split(arena, args)?;
    let (message, rest) = split(arena, rest)?;
    let (signature, _) = split(arena, rest)?;
    let key = atom(arena, key, name)?;
    let message = atom(arena, message, name)?;
    let signature = atom(arena, signature, name)?;
    if message.as_ref().len() != 32 {
        return Err(ClvmError::InvalidInput(format!(
            "{name}: message digest is not 32 bytes"
        )));
    }
    let invalid =
        || ClvmError::InvalidSignature(format!("{name}: invalid public key or signature"));
    if r1 {
        let key =
            p256::ecdsa::VerifyingKey::from_sec1_bytes(key.as_ref()).map_err(|_| invalid())?;
        let signature =
            p256::ecdsa::Signature::from_slice(signature.as_ref()).map_err(|_| invalid())?;
        key.verify_prehash(message.as_ref(), &signature)
            .map_err(|_| invalid())?;
    } else {
        let key =
            k256::ecdsa::VerifyingKey::from_sec1_bytes(key.as_ref()).map_err(|_| invalid())?;
        let signature =
            k256::ecdsa::Signature::from_slice(signature.as_ref()).map_err(|_| invalid())?;
        key.verify_prehash(message.as_ref(), &signature)
            .map_err(|_| invalid())?;
    }
    Ok((cost, OpOut::Same(NodePtr::NIL)))
}
