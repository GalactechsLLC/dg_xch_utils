use crate::clvm::arena::{Arena, Atom, NodePtr};
use crate::clvm::sexp_ext::SExpNumber;
use crate::errors::ClvmError;
use crate::formatting::i32_from_slice;
use std::io::{Error, ErrorKind};

pub const NO_NEG_DIV: u32 = 0x0001;
pub const NO_UNKNOWN_OPS: u32 = 0x0000_0002;
pub const LIMIT_HEAP: u32 = 0x0000_0004;
pub const DISABLE_SIGNATURE_VALIDATION: u32 = 0x0001_0000;
pub const NO_UNKNOWN_CONDITIONS: u32 = 0x0002_0000;
pub const COND_ARGS_NIL: u32 = 0x0004_0000;
pub const STRICT_ARGS_COUNT: u32 = 0x0008_0000;
pub const COST_CONDITIONS: u32 = 0x0080_0000;
pub const IGNORE_ASSERT_CONCURRENT_NULL: u32 = 0x0800_0000;
pub const ENABLE_KECCAK_OPS_OUTSIDE_FORK: u32 = 0x0000_0100;
// Selects the revised division-family cost formulas.
pub const NEW_COST_MODEL: u32 = 0x0000_2000;
// Disables modpow and caps modulus operands before the bounded cost model activates.
pub const DISABLE_OP: u32 = 0x0000_0200;
// Enables operand-size limits for division-family operators.
pub const LIMITS: u32 = 0x0000_0040;
// Rejects redundant leading zeroes in fixed-width unsigned integer atoms.
pub const CANONICAL_INTS: u32 = 0x0400_0000;
// Allows BLS negate operators to process invalid points after the activating fork.
pub const RELAXED_BLS: u32 = 0x0000_4000;
pub const MEMPOOL_MODE: u32 = NO_UNKNOWN_OPS | LIMIT_HEAP;
pub const INFINITE_COST: u64 = 0x7FFF_FFFF_FFFF_FFFF;

/// Returns whether a CLVM buffer uses minimal atom lengths, contains no back-references, and has
/// no trailing data.
#[must_use]
pub fn is_clvm_canonical(clvm: &[u8]) -> bool {
    if clvm.is_empty() {
        return false;
    }
    let mut offset: usize = 0;
    let mut tokens_left: u64 = 1;
    loop {
        let Some(&b) = clvm.get(offset) else {
            return false;
        };
        // pair
        if b == 0xFF {
            tokens_left += 1;
            offset += 1;
            continue;
        }
        // back-references may be encoded many ways; never canonical here
        if b == 0xFE {
            return false;
        }
        // small atom or NIL
        if b <= 0x80 {
            tokens_left -= 1;
            offset += 1;
        } else {
            let (prefix_len, mask, min_value): (usize, u8, u64) = if b & 0b1100_0000 == 0b1000_0000
            {
                (0, 0b0011_1111, 1)
            } else if b & 0b1110_0000 == 0b1100_0000 {
                (1, 0b0001_1111, 1 << 6)
            } else if b & 0b1111_0000 == 0b1110_0000 {
                (2, 0b0000_1111, 1 << 13)
            } else if b & 0b1111_1000 == 0b1111_0000 {
                (3, 0b0000_0111, 1 << 20)
            } else if b & 0b1111_1100 == 0b1111_1000 {
                (4, 0b0000_0011, 1 << 27)
            } else {
                // 0b1111110x — 0xFE/0xFF were handled above
                (5, 0b0000_0001, 1 << 34)
            };
            let mut atom_len = u64::from(b & mask);
            for i in 0..prefix_len {
                let Some(&next) = clvm.get(offset + 1 + i) else {
                    return false;
                };
                atom_len = (atom_len << 8) | u64::from(next);
            }
            if atom_len < min_value {
                return false;
            }
            tokens_left -= 1;
            let Some(next_offset) = usize::try_from(atom_len)
                .ok()
                .and_then(|len| offset.checked_add(1 + prefix_len + len))
            else {
                return false;
            };
            offset = next_offset;
        }
        if tokens_left == 0 {
            break;
        }
    }
    // trailing garbage is not canonical
    offset == clvm.len()
}

pub fn check_cost(cost: u64, max_cost: u64) -> Result<(), Error> {
    if cost > max_cost {
        Err(Error::new(
            ErrorKind::InvalidData,
            format!("cost {cost} exceeded {max_cost}"),
        ))
    } else {
        Ok(())
    }
}

pub fn check_arg_count(
    arena: &Arena,
    args: NodePtr,
    expected: usize,
    name: &'static str,
) -> Result<(), ClvmError> {
    if arena.arg_count(args, expected) == expected {
        Ok(())
    } else {
        Err(ClvmError::InvalidOperandArgs(name, expected))
    }
}

/// `(first, rest)` of a pair — the arena analog of `SExp::split`.
pub fn split(arena: &Arena, node: NodePtr) -> Result<(NodePtr, NodePtr), ClvmError> {
    arena
        .next(node)
        .ok_or_else(|| ClvmError::ExpectedPairGotAtom(arena.display(node)))
}

pub fn int_atom<'a>(arena: &'a Arena, node: NodePtr, op_name: &str) -> Result<Atom<'a>, Error> {
    arena.atom(node).ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidInput,
            format!("{op_name} requires int args: Got {}", arena.display(node)),
        )
    })
}

pub fn atom<'a>(arena: &'a Arena, node: NodePtr, op_name: &str) -> Result<Atom<'a>, Error> {
    arena
        .atom(node)
        .ok_or_else(|| Error::new(ErrorKind::InvalidInput, format!("{op_name} on list")))
}

/// Signed decode + encoded length — the arena analog of `SExpNumberWithLen::try_from(&SExp)`.
pub fn number_with_len(arena: &Arena, node: NodePtr) -> Result<(SExpNumber, usize), ClvmError> {
    match (arena.number(node), arena.atom_len(node)) {
        (Some(n), Some(len)) => Ok((n, len)),
        _ => Err(ClvmError::ExpectedAtomGotPair(arena.display(node))),
    }
}

#[allow(clippy::type_complexity)]
pub fn two_ints(
    arena: &Arena,
    args: NodePtr,
    op_name: &'static str,
) -> Result<((SExpNumber, usize), (SExpNumber, usize)), ClvmError> {
    check_arg_count(arena, args, 2, op_name)?;
    let (first, rest) = split(arena, args)?;
    let second = split(arena, rest)?.0;
    Ok((
        number_with_len(arena, first)?,
        number_with_len(arena, second)?,
    ))
}

pub fn i32_atom(arena: &Arena, node: NodePtr, op_name: &str) -> Result<i32, Error> {
    let Some(buf) = arena.atom(node) else {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!("{op_name} requires int32 args"),
        ));
    };
    match i32_from_slice(buf.as_ref()) {
        Some(v) => Ok(v),
        _ => Err(Error::new(
            ErrorKind::InvalidData,
            format!("{op_name} requires int32 args (with no leading zeros)"),
        )),
    }
}
