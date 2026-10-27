use crate::clvm::arena::{Arena, NodePtr};
use crate::clvm::pure_ops::OpOut;
use crate::clvm::utils::{atom, check_cost, split};
use crate::errors::ClvmError;
use crate::signatures::SignatureScheme;
use crate::signatures::falcon::FnDsa512V2;

/// DGX's versioned five-byte namespace, separate from Chia's short opcodes.
pub const FALCON_VERIFY_OPCODE: &[u8] = b"DGX\x01\x01";
/// Consensus enables this extension only on Falcon networks.
pub const ENABLE_FALCON_OPS: u32 = 0x2000_0000;
pub const FALCON_VERIFY_COST: u64 = 4_000_000;

/// Verify an FN-DSA-512 v2 signature over a 32-byte message digest.
/// Like the secp operators, success returns nil and failure aborts evaluation.
/// The signature uses the same domain context as native Falcon authorization.
pub fn op_falcon_verify(
    arena: &Arena,
    args: NodePtr,
    max_cost: u64,
) -> Result<(u64, OpOut), ClvmError> {
    const NAME: &str = "falcon_verify";
    check_cost(FALCON_VERIFY_COST, max_cost)?;
    if !arena.arg_count_is(args, 3) {
        return Err(ClvmError::InvalidOperandArgs(NAME, 3));
    }
    let (key, rest) = split(arena, args)?;
    let (message, rest) = split(arena, rest)?;
    let (signature, _) = split(arena, rest)?;
    let key = atom(arena, key, NAME)?;
    let message = atom(arena, message, NAME)?;
    let signature = atom(arena, signature, NAME)?;
    let invalid =
        || ClvmError::InvalidSignature("falcon_verify: invalid key, digest or signature".into());
    if message.as_ref().len() != 32
        || !FnDsa512V2.verify(
            key.as_ref().try_into().map_err(|_| invalid())?,
            message.as_ref(),
            signature.as_ref().try_into().map_err(|_| invalid())?,
        )
    {
        return Err(invalid());
    }
    Ok((FALCON_VERIFY_COST, OpOut::Same(NodePtr::NIL)))
}
