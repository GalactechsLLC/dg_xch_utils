use crate::clvm::arena::{Arena, NodePtr};
use crate::clvm::dialect::Dialect;
use crate::clvm::more_ops::new_atom_and_cost;
use crate::clvm::pure_ops::OpOut;
use crate::clvm::utils::{atom, check_cost};
use crate::errors::ClvmError;
use sha3::{Digest, Keccak256};

pub fn op_keccak256<D: Dialect>(
    arena: &Arena,
    mut args: NodePtr,
    max_cost: u64,
    _dialect: &D,
) -> Result<(u64, OpOut), ClvmError> {
    let mut cost = 50;
    let mut hash = Keccak256::new();
    while let Some((arg, rest)) = arena.next(args) {
        args = rest;
        let bytes = atom(arena, arg, "keccak256")?;
        cost += 160 + bytes.as_ref().len() as u64 * 2;
        check_cost(cost, max_cost)?;
        hash.update(bytes.as_ref());
    }
    check_cost(cost + 320, max_cost)?;
    new_atom_and_cost(cost, &hash.finalize())
}
