use crate::clvm::sexp::{SExp, NULL, ONE};
use crate::clvm::utils::{atom, check_arg_count};
use std::io::{Error, ErrorKind};

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

pub fn op_if(args: &SExp, _max_cost: u64) -> Result<(u64, SExp), Error> {
    check_arg_count(args, 3, "i")?;
    let (cond, mut chosen_node) = args.split()?;
    if cond.nullp() {
        chosen_node = chosen_node.split()?.1;
    }
    Ok((IF_COST, chosen_node.split()?.0.clone()))
}

pub fn op_cons(args: &SExp, _max_cost: u64) -> Result<(u64, SExp), Error> {
    check_arg_count(args, 2, "c")?;
    let (first, rest) = args.split()?;
    Ok((CONS_COST, SExp::Pair((first, rest.split()?.0).into())))
}

pub fn op_first(args: &SExp, _max_cost: u64) -> Result<(u64, SExp), Error> {
    check_arg_count(args, 1, "f")?;
    Ok((FIRST_COST, args.split()?.0.split()?.0.clone()))
}

pub fn op_rest(args: &SExp, _max_cost: u64) -> Result<(u64, SExp), Error> {
    check_arg_count(args, 1, "r")?;
    Ok((REST_COST, args.split()?.0.split()?.1.clone()))
}

pub fn op_listp(args: &SExp, _max_cost: u64) -> Result<(u64, SExp), Error> {
    check_arg_count(args, 1, "l")?;
    match args.first()?.pair() {
        Ok(_) => Ok((LISTP_COST, ONE.clone())),
        _ => Ok((LISTP_COST, NULL.clone())),
    }
}

pub fn op_raise(args: &SExp, _max_cost: u64) -> Result<(u64, SExp), Error> {
    let pair = args.pair()?;
    if pair.rest.nullp() {
        Err(Error::new(
            ErrorKind::Unsupported,
            format!("clvm raise: {:?}", pair.first.atom()?),
        ))
    } else {
        Err(Error::new(
            ErrorKind::Unsupported,
            format!("clvm raise: {:?}", &pair.rest),
        ))
    }
}

pub fn op_eq(args: &SExp, _max_cost: u64) -> Result<(u64, SExp), Error> {
    check_arg_count(args, 2, "=")?;
    let s0 = atom(args.first()?, "=")?;
    let s1 = atom(args.rest()?.first()?, "=")?;
    let cost = EQ_BASE_COST + (s0.len() as u64 + s1.len() as u64) * EQ_COST_PER_BYTE;
    Ok((cost, if s0 == s1 { ONE.clone() } else { NULL.clone() }))
}
