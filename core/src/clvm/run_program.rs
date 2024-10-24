use crate::clvm::dialect::Dialect;
use crate::clvm::sexp::{PairBuf, SExp, NULL};
use crate::clvm::utils::ptr_from_number;
use num_bigint::BigInt;
use std::io::Error;
use std::io::ErrorKind;

// lowered from 46
const QUOTE_COST: u64 = 20;

// lowered from 138
const APPLY_COST: u64 = 90;

// mandatory base cost for every operator we execute
const OP_COST: u64 = 1;

// lowered from measured 147 per bit. It doesn't seem to take this long in practice
const TRAVERSE_BASE_COST: u64 = 40;
const TRAVERSE_COST_PER_ZERO_BYTE: u64 = 4;
const TRAVERSE_COST_PER_BIT: u64 = 4;

pub type PreEval = Box<dyn Fn(&SExp, &SExp) -> Result<Option<Box<PostEval>>, Error>>;
pub type PostEval = dyn Fn(Option<&SExp>);

#[repr(u8)]
enum Operation {
    Apply,
    Cons,
    Eval,
    SwapEval,
    PostEval,
}

// `run_program` has two stacks: the operand stack (of `Node` objects) and the
// operator stack (of Operation)
struct RunProgramContext<D> {
    dialect: D,
    pre_eval: Option<PreEval>,
    posteval_stack: Vec<Box<PostEval>>,
    val_stack: Vec<SExp>,
    op_stack: Vec<Operation>,
}

impl<D: Dialect> RunProgramContext<D> {
    pub fn pop(&mut self) -> Result<SExp, Error> {
        match self.val_stack.pop() {
            None => Err(Error::new(
                ErrorKind::InvalidData,
                "runtime error: value stack empty",
            )),
            Some(k) => Ok(k),
        }
    }
    pub fn push(&mut self, node: SExp) {
        self.val_stack.push(node);
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

fn traverse_path(node_index: &[u8], args: &SExp) -> Result<(u64, SExp), Error> {
    let mut arg_list: &SExp = args;

    // find first non-zero byte
    let first_bit_byte_index = first_non_zero(node_index);

    let mut cost: u64 = TRAVERSE_BASE_COST
        + (first_bit_byte_index as u64) * TRAVERSE_COST_PER_ZERO_BYTE
        + TRAVERSE_COST_PER_BIT;

    if first_bit_byte_index >= node_index.len() {
        return Ok((cost, NULL.clone()));
    }

    // find first non-zero bit (the most significant bit is a sentinel)
    let last_bitmask = msb_mask(node_index[first_bit_byte_index]);

    // follow through the bits, moving left and right
    let mut byte_idx = node_index.len() - 1;
    let mut bitmask = 0x01;
    while byte_idx > first_bit_byte_index || bitmask < last_bitmask {
        let is_bit_set: bool = (node_index[byte_idx] & bitmask) != 0;
        match arg_list {
            SExp::Atom(_) => {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    format!("path into atom: {arg_list:?}"),
                ));
            }
            SExp::Pair(pair) => {
                arg_list = if is_bit_set {
                    &*pair.rest
                } else {
                    &*pair.first
                };
            }
        }
        if bitmask == 0x80 {
            bitmask = 0x01;
            byte_idx -= 1;
        } else {
            bitmask <<= 1;
        }
        cost += TRAVERSE_COST_PER_BIT;
    }
    Ok((cost, arg_list.clone()))
}

fn augment_cost_errors(r: Result<u64, Error>, max_cost: &SExp) -> Result<u64, Error> {
    if let Err(e) = r {
        if !format!("{e:?}").contains("cost exceeded") {
            Err(e)
        } else {
            Err(Error::new(
                ErrorKind::InvalidData,
                format!("Max Cost({max_cost:?}) Exceeded: {e:?}"),
            ))
        }
    } else {
        r
    }
}

impl<D: Dialect> RunProgramContext<D> {
    fn new(dialect: D, pre_eval: Option<PreEval>) -> Self {
        RunProgramContext {
            dialect,
            pre_eval,
            posteval_stack: Vec::new(),
            val_stack: Vec::new(),
            op_stack: Vec::new(),
        }
    }

    fn cons_op(&mut self) -> Result<u64, Error> {
        let v1 = self.pop()?;
        let v2 = self.pop()?;
        let p = SExp::Pair(PairBuf {
            first: Box::new(v1),
            rest: Box::new(v2),
        });
        self.push(p);
        Ok(0)
    }
}

impl<D: Dialect> RunProgramContext<D> {
    fn eval_op_atom(
        &mut self,
        operator_node: &SExp,
        operand_list: &SExp,
        args: &SExp,
    ) -> Result<u64, Error> {
        let op_atom = operator_node.atom()?;
        if op_atom.data == self.dialect.quote_kw() {
            self.push(operand_list.clone());
            Ok(QUOTE_COST)
        } else {
            self.op_stack.push(Operation::Apply);
            self.push(operator_node.clone());
            let mut operands: &SExp = operand_list;
            loop {
                match operands {
                    SExp::Atom(buf) => {
                        if buf.data.is_empty() {
                            break;
                        }
                        return Err(Error::new(
                            ErrorKind::InvalidData,
                            format!("bad operand list: {operand_list:?}"),
                        ));
                    }
                    SExp::Pair(pair) => {
                        self.op_stack.push(Operation::SwapEval);
                        self.push(args.clone());
                        self.push(pair.first.as_ref().clone());
                        operands = pair.rest.as_ref();
                    }
                }
            }
            self.push(NULL.clone());
            Ok(OP_COST)
        }
    }

    fn eval_pair(&mut self, program: &SExp, args: &SExp) -> Result<u64, Error> {
        let (op_node, op_list) = match program {
            SExp::Atom(path) => {
                let r = traverse_path(&path.data, args)?;
                self.push(r.1.clone());
                return Ok(r.0);
            }
            SExp::Pair(pair) => (&*pair.first, &*pair.rest),
        };
        if let SExp::Pair(pair) = &op_node {
            if let SExp::Atom(_) = pair.first.as_ref() {
                if pair.rest.nullp() {
                    self.push(pair.first.as_ref().clone());
                    self.push(op_list.clone());
                    self.op_stack.push(Operation::Apply);
                    return Ok(APPLY_COST);
                }
            }
            return Err(Error::new(
                ErrorKind::Unsupported,
                format!("in ((X)...) syntax X must be lone atom: {pair:?}"),
            ));
        };
        self.eval_op_atom(op_node, op_list, args)
    }

    fn swap_eval_op(&mut self) -> Result<u64, Error> {
        let v2 = self.pop()?;
        let program = self.pop()?;
        let args = self.pop()?;
        self.push(v2);
        let post_eval = match self.pre_eval {
            None => None,
            Some(ref pre_eval) => pre_eval(&program, &args)?,
        };
        if let Some(post_eval) = post_eval {
            self.posteval_stack.push(post_eval);
            self.op_stack.push(Operation::PostEval);
        };

        self.op_stack.push(Operation::Cons);
        self.eval_pair(&program, &args)
    }

    fn eval_op(&mut self) -> Result<u64, Error> {
        let pair = self.pop()?;
        match pair {
            SExp::Atom(_) => Err(Error::new(
                ErrorKind::InvalidInput,
                format!("pair expected: {pair:?}"),
            )),
            SExp::Pair(pair) => {
                let post_eval = match self.pre_eval {
                    None => None,
                    Some(ref pre_eval) => pre_eval(&pair.first, &pair.rest)?,
                };
                if let Some(post_eval) = post_eval {
                    self.posteval_stack.push(post_eval);
                    self.op_stack.push(Operation::PostEval);
                };

                self.eval_pair(&pair.first, &pair.rest)
            }
        }
    }

    fn apply_op(&mut self, max_cost: u64) -> Result<u64, Error> {
        let operand_list = self.pop()?;
        let operator = self.pop()?;
        if let SExp::Pair(_) = operator {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                format!("internal error: {operator:?}"),
            ));
        }
        let op_atom = operator.atom()?;
        if op_atom.data == self.dialect.apply_kw() {
            if operand_list.arg_count_is(2) {
                let (new_program, arg_wrap) = operand_list.split()?;
                let (new_args, _) = arg_wrap.split()?;
                let post_eval = match self.pre_eval {
                    None => None,
                    Some(ref pre_eval) => pre_eval(new_program, new_args)?,
                };
                if let Some(post_eval) = post_eval {
                    self.posteval_stack.push(post_eval);
                    self.op_stack.push(Operation::PostEval);
                };

                self.eval_pair(new_program, new_args)
                    .map(|c| c + APPLY_COST)
            } else {
                Err(Error::new(
                    ErrorKind::InvalidInput,
                    format!("apply requires exactly 2 parameters: {operand_list:?}"),
                ))
            }
        } else {
            let (cost, result) = self.dialect.op(operator, operand_list, max_cost)?;
            self.push(result);
            Ok(cost)
        }
    }

    pub fn run_program(
        &mut self,
        program: &SExp,
        args: &SExp,
        max_cost: u64,
    ) -> Result<(u64, SExp), Error> {
        self.val_stack = vec![SExp::Pair((program, args).into())];
        self.op_stack = vec![Operation::Eval];
        let max_cost = if max_cost == 0 { u64::MAX } else { max_cost };
        let max_cost_number: BigInt = max_cost.into();
        let max_cost_ptr = ptr_from_number(&max_cost_number)?;
        let mut cost: u64 = 0;
        loop {
            let top = self.op_stack.pop();
            let Some(op) = top else { break };
            cost += match op {
                Operation::Apply => {
                    augment_cost_errors(self.apply_op(max_cost - cost), &max_cost_ptr)?
                }
                Operation::Cons => self.cons_op()?,
                Operation::Eval => augment_cost_errors(self.eval_op(), &max_cost_ptr)?,
                Operation::SwapEval => augment_cost_errors(self.swap_eval_op(), &max_cost_ptr)?,
                Operation::PostEval => {
                    let f = self.posteval_stack.pop().ok_or_else(|| {
                        Error::new(ErrorKind::InvalidData, "post_eval_stack is empty")
                    })?;
                    let peek = self.val_stack.last();
                    f(peek);
                    0
                }
            };
            if cost > max_cost {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    format!("cost exceeded: {max_cost_ptr:?}"),
                ));
            }
        }
        Ok((cost, self.pop()?))
    }
}

pub fn run_program<'a, D: Dialect>(
    dialect: D,
    program: &'a SExp,
    args: &'a SExp,
    max_cost: u64,
    pre_eval: Option<PreEval>,
) -> Result<(u64, SExp), Error> {
    let mut rpc = RunProgramContext::new(dialect, pre_eval);
    rpc.run_program(program, args, max_cost)
}
