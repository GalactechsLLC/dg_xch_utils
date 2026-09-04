use crate::clvm::arena::{Arena, Checkpoint, NodeKind, NodePtr};
use crate::clvm::dialect::{ChiaDialect, Dialect};
use crate::clvm::sexp::SExp;
use crate::errors::ClvmError;
use log::debug;
use std::time::Instant;

const QUOTE_COST: u64 = 20;
const APPLY_COST: u64 = 90;
const OP_COST: u64 = 1;
const TRAVERSE_BASE_COST: u64 = 40;
const TRAVERSE_COST_PER_ZERO_BYTE: u64 = 4;
const TRAVERSE_COST_PER_BIT: u64 = 4;

#[repr(u8)]
enum Operation {
    Apply,
    Cons,
    Eval,
    SwapEval,
}

/// The CLVM evaluator over the compact node [`Arena`]: every eval intermediate lives in
/// the arena's typed pools as a u32 handle — `cons` is an 8-byte pair-pool push, a
/// path-traversal result is a handle copy, and an op result is allocated exactly once.
pub struct ClvmRuntime {
    dialect: ChiaDialect,
    arena: Arena,
    value_stack: Vec<NodePtr>,
    op_stack: Vec<Operation>,
    /// One per pending application, taken before its operands are evaluated — the point rewound
    /// to when the operator returns a self-contained result.
    checkpoint_stack: Vec<Checkpoint>,
    max_cost: u64,
}

impl ClvmRuntime {
    #[must_use]
    pub fn new(max_cost: u64, flags: u32) -> Self {
        ClvmRuntime {
            dialect: ChiaDialect::new(flags),
            arena: Arena::new(),
            value_stack: vec![],
            op_stack: vec![],
            checkpoint_stack: vec![],
            max_cost,
        }
    }

    /// Allocation counters (atoms, pairs, heap bytes incl. ghost accounting) of the last
    /// run — memory-pressure diagnostics for the probes.
    #[must_use]
    pub fn arena_counters(&self) -> (usize, usize, usize) {
        self.arena.counters()
    }

    pub fn run(&mut self, program: &SExp, args: &SExp) -> Result<(u64, SExp<'static>), ClvmError> {
        let (cost, node) = self.run_in_arena(program, args)?;
        Ok((cost, self.arena.export(node)))
    }

    /// Evaluate `program` and leave the result in this runtime's arena, returning its
    /// [`NodePtr`] and the cost — WITHOUT `export`ing it to an owned `SExp` tree.
    ///
    /// [`Self::run`] exports the result, which is unsafe for an adversarial block generator
    /// whose output the caller streams: `Arena::export` deep-copies each atom by reference, so a
    /// `concat`/`substr` ladder emitting one shared ~268 MB integer `num` times is copied `num`
    /// times and OOM-kills the process. A caller that needs to bound a large output must walk it
    /// from [`Self::arena`] rather than `run`, charging condition cost incrementally and bailing
    /// at the first duplicate or at `MAX_BLOCK_COST_CLVM`.
    pub fn run_in_arena(
        &mut self,
        program: &SExp,
        args: &SExp,
    ) -> Result<(u64, NodePtr), ClvmError> {
        self.reset();
        let program = self.arena.import(program)?;
        let args = self.arena.import(args)?;
        self.value_stack.push(program);
        self.value_stack.push(args);
        self.op_stack.push(Operation::Eval);
        let max_cost = if self.max_cost == 0 {
            u64::MAX
        } else {
            self.max_cost
        };
        let mut current_cost: u64 = 0;
        let start = Instant::now();
        while let Some(op) = self.op_stack.pop() {
            current_cost += match op {
                Operation::Apply => self.apply_op(max_cost - current_cost)?,
                Operation::Cons => self.cons()?,
                Operation::Eval => self.eval_op()?,
                Operation::SwapEval => self.swap_eval_op()?,
            };
            if current_cost > max_cost {
                return Err(ClvmError::CostExceeded(current_cost, self.max_cost));
            }
        }
        let duration = start.elapsed();
        debug!("Program duration: {duration:?}");
        let return_value = self.value_stack.pop().ok_or(ClvmError::ValueStackEmpty)?;
        Ok((current_cost, return_value))
    }

    /// Borrow the arena holding the last [`Self::run_in_arena`] result, so a caller can walk
    /// a large/deep output iteratively (bounded, streaming) instead of exporting it.
    #[must_use]
    pub fn arena(&self) -> &Arena {
        &self.arena
    }

    fn reset(&mut self) {
        self.value_stack.clear();
        self.op_stack.clear();
        self.checkpoint_stack.clear();
        self.arena.reset();
    }

    fn cons(&mut self) -> Result<u64, ClvmError> {
        let first = self.value_stack.pop().ok_or(ClvmError::ValueStackEmpty)?;
        let rest = self.value_stack.pop().ok_or(ClvmError::ValueStackEmpty)?;
        let pair = self.arena.new_pair(first, rest)?;
        self.value_stack.push(pair);
        Ok(0)
    }

    fn traverse_path(
        arena: &Arena,
        node_index: &[u8],
        args: NodePtr,
    ) -> Result<(u64, NodePtr), ClvmError> {
        let mut arg_list = args;
        // find first non-zero byte
        let first_bit_byte_index = first_non_zero(node_index);
        let mut cost: u64 = TRAVERSE_BASE_COST
            + (first_bit_byte_index as u64) * TRAVERSE_COST_PER_ZERO_BYTE
            + TRAVERSE_COST_PER_BIT;
        if first_bit_byte_index >= node_index.len() {
            return Ok((cost, NodePtr::NIL));
        }
        // find first non-zero bit (the most significant bit is a sentinel)
        let last_bitmask = msb_mask(node_index[first_bit_byte_index]);
        // follow through the bits, moving left and right
        let mut byte_idx = node_index.len() - 1;
        let mut bitmask = 0x01;
        while byte_idx > first_bit_byte_index || bitmask < last_bitmask {
            let is_bit_set: bool = (node_index[byte_idx] & bitmask) != 0;
            let NodeKind::Pair(first, rest) = arena.node_kind(arg_list) else {
                return Err(ClvmError::ExpectedPairGotAtom(arena.display(arg_list)));
            };
            arg_list = if is_bit_set { rest } else { first };
            if bitmask == 0x80 {
                bitmask = 0x01;
                byte_idx -= 1;
            } else {
                bitmask <<= 1;
            }
            cost += TRAVERSE_COST_PER_BIT;
        }
        Ok((cost, arg_list))
    }

    fn eval_op_atom(
        &mut self,
        operator_node: NodePtr,
        operand_list: NodePtr,
        args: NodePtr,
    ) -> Result<u64, ClvmError> {
        let is_quote = {
            let op_atom = self
                .arena
                .atom(operator_node)
                .ok_or_else(|| ClvmError::ExpectedAtomGotPair(self.arena.display(operator_node)))?;
            op_atom.as_ref() == self.dialect.quote_kw()
        };
        if is_quote {
            self.value_stack.push(operand_list);
            Ok(QUOTE_COST)
        } else {
            self.op_stack.push(Operation::Apply);
            self.checkpoint_stack.push(self.arena.checkpoint());
            self.value_stack.push(operator_node);
            let mut operands = operand_list;
            loop {
                match self.arena.node_kind(operands) {
                    NodeKind::Atom => {
                        if self.arena.nullp(operands) {
                            break;
                        }
                        return Err(ClvmError::InvalidOperandList(self.arena.display(operands)));
                    }
                    NodeKind::Pair(first, rest) => {
                        self.op_stack.push(Operation::SwapEval);
                        self.value_stack.push(args);
                        self.value_stack.push(first);
                        operands = rest;
                    }
                }
            }
            self.value_stack.push(NodePtr::NIL);
            Ok(OP_COST)
        }
    }

    fn eval_pair(&mut self, program: NodePtr, args: NodePtr) -> Result<u64, ClvmError> {
        let (op_node, op_list) = match self.arena.node_kind(program) {
            NodeKind::Atom => {
                let r = {
                    let path = self.arena.atom(program).expect("node_kind atom has bytes");
                    Self::traverse_path(&self.arena, path.as_ref(), args)?
                };
                self.value_stack.push(r.1);
                return Ok(r.0);
            }
            NodeKind::Pair(first, rest) => (first, rest),
        };
        if let NodeKind::Pair(inner_first, inner_rest) = self.arena.node_kind(op_node) {
            if matches!(self.arena.node_kind(inner_first), NodeKind::Atom)
                && self.arena.nullp(inner_rest)
            {
                self.value_stack.push(inner_first);
                self.value_stack.push(op_list);
                self.op_stack.push(Operation::Apply);
                // Every Apply frame owns a checkpoint; without one this frame would pop an
                // enclosing frame's and rewind live siblings.
                self.checkpoint_stack.push(self.arena.checkpoint());
                return Ok(APPLY_COST);
            }
            return Err(ClvmError::InvalidSyntax(format!(
                "in ((X)...) syntax X must be lone atom: {}",
                self.arena.debug_fmt(op_node)
            )));
        };
        self.eval_op_atom(op_node, op_list, args)
    }

    fn swap_eval_op(&mut self) -> Result<u64, ClvmError> {
        let v2_index = self.value_stack.pop().ok_or(ClvmError::ValueStackEmpty)?;
        let program = self.value_stack.pop().ok_or(ClvmError::ValueStackEmpty)?;
        let args = self.value_stack.pop().ok_or(ClvmError::ValueStackEmpty)?;
        self.value_stack.push(v2_index);
        // Cons must be queued before the operand eval so the accumulated operand list is
        // rebuilt in the correct order.
        self.op_stack.push(Operation::Cons);
        self.eval_pair(program, args)
    }

    fn eval_op(&mut self) -> Result<u64, ClvmError> {
        let args = self.value_stack.pop().ok_or(ClvmError::ValueStackEmpty)?;
        let program = self.value_stack.pop().ok_or(ClvmError::ValueStackEmpty)?;
        self.eval_pair(program, args)
    }

    fn apply_op(&mut self, max_cost: u64) -> Result<u64, ClvmError> {
        let operand_list = self.value_stack.pop().ok_or(ClvmError::ValueStackEmpty)?;
        let operator = self.value_stack.pop().ok_or(ClvmError::ValueStackEmpty)?;
        let is_apply = {
            let op_atom = self
                .arena
                .atom(operator)
                .ok_or_else(|| ClvmError::ExpectedAtomGotPair(self.arena.display(operator)))?;
            op_atom.as_ref() == self.dialect.apply_kw()
        };
        if is_apply {
            // This frame's checkpoint is released, not restored: the evaluation continues from
            // the applied program, whose result may reach anything allocated so far.
            self.checkpoint_stack.pop();
            if self.arena.arg_count_is(operand_list, 2) {
                let (new_program, arg_wrap) = self.arena.next(operand_list).ok_or_else(|| {
                    ClvmError::ExpectedPairGotAtom(self.arena.display(operand_list))
                })?;
                let (new_args, _) = self
                    .arena
                    .next(arg_wrap)
                    .ok_or_else(|| ClvmError::ExpectedPairGotAtom(self.arena.display(arg_wrap)))?;
                self.eval_pair(new_program, new_args)
                    .map(|c| c + APPLY_COST)
            } else {
                Err(ClvmError::InvalidApplyArgs(
                    self.arena.display(operand_list),
                ))
            }
        } else {
            let (cost, out) = self
                .dialect
                .op(&self.arena, operator, operand_list, max_cost)?;
            // A self-contained description borrows nothing, so the operand evaluation's
            // allocations are unreachable and can be rewound before the result is written.
            if out.is_self_contained()
                && let Some(cp) = self.checkpoint_stack.pop()
            {
                self.arena.restore(cp);
            } else {
                self.checkpoint_stack.pop();
            }
            let (cost, result) = out.materialize(&mut self.arena, cost)?;
            self.value_stack.push(result);
            Ok(cost)
        }
    }
}

// return a bitmask with a single bit set, for the most significant set bit in
// the input byte
#[allow(clippy::cast_possible_truncation)]
fn msb_mask(byte: u8) -> u8 {
    let mut byte = u32::from(byte | (byte >> 1));
    byte |= byte >> 2;
    byte |= byte >> 4;
    debug_assert!((byte + 1) >> 1 <= 0x80);
    ((byte + 1) >> 1) as u8
}

// return the index of the first non-zero byte in buf. If all bytes are 0, the
// length (one past end) will be returned.
const fn first_non_zero(buf: &[u8]) -> usize {
    let mut c: usize = 0;
    while c < buf.len() && buf[c] == 0 {
        c += 1;
    }
    c
}

#[cfg(test)]
#[path = "../../tests/unit/clvm/runtime/tests.rs"]
mod tests;
