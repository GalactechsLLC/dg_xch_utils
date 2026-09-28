use crate::clvm::arena::{Arena, NodeKind, NodePtr};
use crate::clvm::dialect::Dialect;
use crate::clvm::pure_ops::OpOut;
use crate::clvm::utils::{atom, check_arg_count, split};
use crate::errors::ClvmError;

const FIRST_COST: u64 = 30;
const IF_COST: u64 = 33;
// Cons cost lowered from 245. It only allocates a pair, which is small
const CONS_COST: u64 = 50;
// Rest cost lowered from 77 since it doesn't allocate anything and it should be
// the same as first
const REST_COST: u64 = 30;
const LISTP_COST: u64 = 19;
const EQ_BASE_COST: u64 = 117;
const EQ_COST_PER_BYTE: u64 = 1;

pub fn op_if<D: Dialect>(
    arena: &Arena,
    args: NodePtr,
    _max_cost: u64,
    _dialect: &D,
) -> Result<(u64, OpOut), ClvmError> {
    check_arg_count(arena, args, 3, "i")?;
    let (cond, mut chosen_node) = split(arena, args)?;
    if arena.nullp(cond) {
        chosen_node = split(arena, chosen_node)?.1;
    }
    Ok((IF_COST, OpOut::Same(split(arena, chosen_node)?.0)))
}

pub fn op_cons<D: Dialect>(
    arena: &Arena,
    args: NodePtr,
    _max_cost: u64,
    _dialect: &D,
) -> Result<(u64, OpOut), ClvmError> {
    check_arg_count(arena, args, 2, "c")?;
    let (first, rest) = split(arena, args)?;
    let second = split(arena, rest)?.0;
    Ok((CONS_COST, OpOut::Pair(first, second)))
}

pub fn op_first<D: Dialect>(
    arena: &Arena,
    args: NodePtr,
    _max_cost: u64,
    _dialect: &D,
) -> Result<(u64, OpOut), ClvmError> {
    check_arg_count(arena, args, 1, "f")?;
    let inner = split(arena, args)?.0;
    Ok((FIRST_COST, OpOut::Same(split(arena, inner)?.0)))
}

pub fn op_rest<D: Dialect>(
    arena: &Arena,
    args: NodePtr,
    _max_cost: u64,
    _dialect: &D,
) -> Result<(u64, OpOut), ClvmError> {
    check_arg_count(arena, args, 1, "r")?;
    let inner = split(arena, args)?.0;
    Ok((REST_COST, OpOut::Same(split(arena, inner)?.1)))
}

pub fn op_listp<D: Dialect>(
    arena: &Arena,
    args: NodePtr,
    _max_cost: u64,
    _dialect: &D,
) -> Result<(u64, OpOut), ClvmError> {
    check_arg_count(arena, args, 1, "l")?;
    let inner = split(arena, args)?.0;
    match arena.node_kind(inner) {
        NodeKind::Pair(_, _) => Ok((LISTP_COST, OpOut::Same(NodePtr::ONE))),
        NodeKind::Atom => Ok((LISTP_COST, OpOut::Same(NodePtr::NIL))),
    }
}

pub fn op_raise<D: Dialect>(
    arena: &Arena,
    args: NodePtr,
    _max_cost: u64,
    _dialect: &D,
) -> Result<(u64, OpOut), ClvmError> {
    match arena.node_kind(args) {
        NodeKind::Atom => Err(ClvmError::Raise(arena.debug_fmt(args))),
        NodeKind::Pair(first, rest) => {
            if arena.nullp(rest) {
                if arena.atom(first).is_none() {
                    return Err(ClvmError::ExpectedAtomGotPair(arena.display(first)));
                }
                Err(ClvmError::Raise(arena.debug_fmt(first)))
            } else {
                Err(ClvmError::Raise(arena.debug_fmt(rest)))
            }
        }
    }
}

pub fn op_eq<D: Dialect>(
    arena: &Arena,
    args: NodePtr,
    _max_cost: u64,
    _dialect: &D,
) -> Result<(u64, OpOut), ClvmError> {
    check_arg_count(arena, args, 2, "=")?;
    let (a0, rest) = split(arena, args)?;
    let a1 = split(arena, rest)?.0;
    let (equal, cost) = {
        let s0 = atom(arena, a0, "=")?;
        let s1 = atom(arena, a1, "=")?;
        (
            s0.as_ref() == s1.as_ref(),
            EQ_BASE_COST + (s0.len() as u64 + s1.len() as u64) * EQ_COST_PER_BYTE,
        )
    };
    Ok((
        cost,
        OpOut::Same(if equal { NodePtr::ONE } else { NodePtr::NIL }),
    ))
}

#[cfg(test)]
#[path = "../../tests/unit/clvm/core_ops/tests.rs"]
mod tests;
