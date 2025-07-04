use crate::blockchain::coin::Coin;
use crate::blockchain::sized_bytes::Bytes32;
use crate::clvm::debug_ops::op_print;
use crate::clvm::dialect::Dialect;
use crate::clvm::parser::sexp_to_bytes;
use crate::clvm::sexp::{AtomBuf, SExp};
use crate::clvm::utils::{atom, check_arg_count, check_cost, i32_atom, int_atom, two_ints};
use crate::constants::{NULL_SEXP, ONE_SEXP};
use crate::formatting::{number_from_slice, u32_from_slice, u64_from_bigint};
use crate::traits::SizedBytes;
use bls12_381::{G1Affine, G1Projective, Scalar};
use num_bigint::{BigInt, BigUint, Sign};
use num_integer::Integer;
use num_traits::{Signed, Zero};
use once_cell::sync::Lazy;
use sha2::Digest;
use sha2::Sha256;
use std::convert::TryFrom;
use std::io::{Error, ErrorKind};
use std::ops::BitAndAssign;
use std::ops::BitOrAssign;
use std::ops::BitXorAssign;

const MALLOC_COST_PER_BYTE: u64 = 10;

const ARITH_BASE_COST: u64 = 99;
const ARITH_COST_PER_ARG: u64 = 320;
const ARITH_COST_PER_BYTE: u64 = 3;

const LOG_BASE_COST: u64 = 100;
const LOG_COST_PER_ARG: u64 = 264;
const LOG_COST_PER_BYTE: u64 = 3;

const LOG_NOT_BASE_COST: u64 = 331;
const LOG_NOT_COST_PER_BYTE: u64 = 3;

const MUL_BASE_COST: u64 = 92;
const MUL_COST_PER_OP: u64 = 885;
const MUL_LINEAR_COST_PER_BYTE: u64 = 6;
const MUL_SQUARE_COST_PER_BYTE_DIVIDER: u64 = 128;

const GR_BASE_COST: u64 = 498;
const GR_COST_PER_BYTE: u64 = 2;

const GRS_BASE_COST: u64 = 117;
const GRS_COST_PER_BYTE: u64 = 1;

const STRLEN_BASE_COST: u64 = 173;
const STRLEN_COST_PER_BYTE: u64 = 1;

const CONCAT_BASE_COST: u64 = 142;
const CONCAT_COST_PER_ARG: u64 = 135;
const CONCAT_COST_PER_BYTE: u64 = 3;

const DIV_MOD_BASE_COST: u64 = 1116;
const DIV_MOD_COST_PER_BYTE: u64 = 6;

const DIV_BASE_COST: u64 = 988;
const DIV_COST_PER_BYTE: u64 = 4;

const SHA256_BASE_COST: u64 = 87;
const SHA256_COST_PER_ARG: u64 = 134;
const SHA256_COST_PER_BYTE: u64 = 2;

const A_SHIFT_BASE_COST: u64 = 596;
const A_SHIFT_COST_PER_BYTE: u64 = 3;

const LSHIFT_BASE_COST: u64 = 277;
const LSHIFT_COST_PER_BYTE: u64 = 3;

pub const BOOL_BASE_COST: u64 = 200;
const BOOL_COST_PER_ARG: u64 = 300;

// Raspberry PI 4 is about 7.679960 / 1.201742 = 6.39 times slower
// in the point_add benchmark

// increased from 31592 to better model Raspberry PI
const POINT_ADD_BASE_COST: u64 = 101_094;
// increased from 419994 to better model Raspberry PI
const POINT_ADD_COST_PER_ARG: u64 = 1_343_980;

// Raspberry PI 4 is about 2.833543 / 0.447859 = 6.32686 times slower
// in the pubkey benchmark

// increased from 419535 to better model Raspberry PI
const PUBKEY_BASE_COST: u64 = 1_325_730;
// increased from 12 to closer model Raspberry PI
const PUBKEY_COST_PER_BYTE: u64 = 38;

const COIN_ID_COST: u64 =
    SHA256_BASE_COST + SHA256_COST_PER_ARG * 3 + SHA256_COST_PER_BYTE * (32 + 32 + 8) - 153;

fn limbs_for_int(v: &BigInt) -> u64 {
    v.bits().div_ceil(8)
}

fn new_atom_and_cost(cost: u64, buf: &[u8]) -> (u64, SExp) {
    let c = buf.len() as u64 * MALLOC_COST_PER_BYTE;
    (cost + c, SExp::Atom(buf.to_vec().into()))
}

fn malloc_cost(cost: u64, ptr: SExp) -> Result<(u64, SExp), Error> {
    let c = ptr.atom()?.data.len() as u64 * MALLOC_COST_PER_BYTE;
    Ok((cost + c, ptr))
}

pub fn op_unknown<D: Dialect>(
    o: &SExp,
    args: &SExp,
    max_cost: u64,
    _dialect: &D,
) -> Result<(u64, SExp), Error> {
    let op = &o.atom()?.data;
    if op.is_empty() || (op.len() >= 2 && op[0] == 0xff && op[1] == 0xff) {
        return Err(Error::new(
            ErrorKind::Unsupported,
            format!("Reserved Operator: {:?}", &op),
        ));
    }
    let cost_function = (op[op.len() - 1] & 0b1100_0000) >> 6;
    let cost_multiplier: u64 = match u32_from_slice(&op[0..op.len() - 1]) {
        Some(v) => u64::from(v),
        None => {
            return Err(Error::new(
                ErrorKind::Unsupported,
                format!("Invalid Operator: {:?}", &op),
            ));
        }
    };
    let mut cost = match cost_function {
        1 => {
            let mut cost = ARITH_BASE_COST;
            let mut byte_count: u64 = 0;
            for arg in args {
                cost += ARITH_COST_PER_ARG;
                let blob = int_atom(arg, "unknown op")?;
                byte_count += blob.len() as u64;
                check_cost(cost + (byte_count * ARITH_COST_PER_BYTE), max_cost)?;
            }
            cost + (byte_count * ARITH_COST_PER_BYTE)
        }
        2 => {
            let mut cost = MUL_BASE_COST;
            let mut first_iter: bool = true;
            let mut l0: u64 = 0;
            for arg in args {
                let blob = int_atom(arg, "unknown op")?;
                if first_iter {
                    l0 = blob.len() as u64;
                    first_iter = false;
                    continue;
                }
                let l1 = blob.len() as u64;
                cost += MUL_COST_PER_OP;
                cost += (l0 + l1) * MUL_LINEAR_COST_PER_BYTE;
                cost += (l0 * l1) / MUL_SQUARE_COST_PER_BYTE_DIVIDER;
                l0 += l1;
                check_cost(cost, max_cost)?;
            }
            cost
        }
        3 => {
            let mut cost = CONCAT_BASE_COST;
            let mut total_size: u64 = 0;
            for arg in args {
                cost += CONCAT_COST_PER_ARG;
                let blob = atom(arg, "unknown op")?;
                total_size += blob.len() as u64;
                check_cost(cost + total_size * CONCAT_COST_PER_BYTE, max_cost)?;
            }
            cost + total_size * CONCAT_COST_PER_BYTE
        }
        _ => 1,
    };
    check_cost(cost, max_cost)?;
    cost *= cost_multiplier + 1;
    if cost > u64::from(u32::MAX) {
        Err(Error::new(
            ErrorKind::Unsupported,
            format!("Invalid Operator: {o:?}"),
        ))
    } else {
        Ok((cost, NULL_SEXP.clone()))
    }
}

pub fn op_sha256<D: Dialect>(
    args: &SExp,
    max_cost: u64,
    _dialect: &D,
) -> Result<(u64, SExp), Error> {
    let mut cost = SHA256_BASE_COST;
    let mut byte_count: usize = 0;
    let mut hasher = Sha256::new();
    for arg in args {
        cost += SHA256_COST_PER_ARG;
        check_cost(cost + byte_count as u64 * SHA256_COST_PER_BYTE, max_cost)?;
        let blob = atom(arg, "sha256")?;
        byte_count += blob.len();
        hasher.update(blob);
    }
    cost += byte_count as u64 * SHA256_COST_PER_BYTE;
    Ok(new_atom_and_cost(cost, &hasher.finalize()))
}

pub fn op_add<D: Dialect>(args: &SExp, max_cost: u64, _dialect: &D) -> Result<(u64, SExp), Error> {
    let mut cost = ARITH_BASE_COST;
    let mut byte_count: usize = 0;
    let mut total: BigInt = 0.into();
    for arg in args {
        cost += ARITH_COST_PER_ARG;
        check_cost(cost + (byte_count as u64 * ARITH_COST_PER_BYTE), max_cost)?;
        let blob = int_atom(arg, "+")?;
        let v: BigInt = number_from_slice(blob);
        byte_count += blob.len();
        total += v;
    }
    let total = SExp::try_from(&total)?;
    cost += byte_count as u64 * ARITH_COST_PER_BYTE;
    malloc_cost(cost, total)
}

pub fn op_subtract<D: Dialect>(
    args: &SExp,
    max_cost: u64,
    _dialect: &D,
) -> Result<(u64, SExp), Error> {
    let mut cost = ARITH_BASE_COST;
    let mut byte_count: usize = 0;
    let mut total: BigInt = 0.into();
    let mut is_first = true;
    for arg in args {
        cost += ARITH_COST_PER_ARG;
        check_cost(cost + byte_count as u64 * ARITH_COST_PER_BYTE, max_cost)?;
        let blob = int_atom(arg, "-")?;
        let v: BigInt = number_from_slice(blob);
        byte_count += blob.len();
        if is_first {
            total += v;
        } else {
            total -= v;
        };
        is_first = false;
    }
    let total = SExp::try_from(&total)?;
    cost += byte_count as u64 * ARITH_COST_PER_BYTE;
    malloc_cost(cost, total)
}

pub fn op_multiply<D: Dialect>(
    args: &SExp,
    max_cost: u64,
    _dialect: &D,
) -> Result<(u64, SExp), Error> {
    let mut cost: u64 = MUL_BASE_COST;
    let mut first_iter: bool = true;
    let mut total: BigInt = 1.into();
    let mut l0 = 0u64;
    for arg in args {
        check_cost(cost, max_cost)?;
        let blob = int_atom(arg, "*")?;
        if first_iter {
            l0 = blob.len() as u64;
            total = number_from_slice(blob);
            first_iter = false;
            continue;
        }
        let l1 = blob.len() as u64;

        total *= number_from_slice(blob);
        cost += MUL_COST_PER_OP;

        cost += (l0 + l1) * MUL_LINEAR_COST_PER_BYTE;
        cost += (l0 * l1) / MUL_SQUARE_COST_PER_BYTE_DIVIDER;

        l0 = limbs_for_int(&total);
    }
    let total = SExp::try_from(&total)?;
    malloc_cost(cost, total)
}

pub fn op_div_impl(args: &SExp, mempool: bool) -> Result<(u64, SExp), Error> {
    let (a0, l0, a1, l1) = two_ints(args, "/")?;
    let cost = DIV_BASE_COST + ((l0 + l1) as u64) * DIV_COST_PER_BYTE;
    if a1.sign() == Sign::NoSign {
        Err(Error::new(
            ErrorKind::Unsupported,
            format!("div with 0 : {:?}", args.first()?),
        ))
    } else {
        if mempool && (a0.sign() == Sign::Minus || a1.sign() == Sign::Minus) {
            return Err(Error::new(
                ErrorKind::Unsupported,
                format!("div operator with negative operands is deprecated: {args:?}"),
            ));
        }
        let (mut q, r) = a0.div_mod_floor(&a1);
        // this is to preserve a buggy behavior from the initial implementation of this operator.
        if q == (-1).into() && r != 0.into() {
            q += 1;
        }
        let q1 = SExp::try_from(&q)?;
        malloc_cost(cost, q1)
    }
}

pub fn op_div<D: Dialect>(args: &SExp, _max_cost: u64, _dialect: &D) -> Result<(u64, SExp), Error> {
    op_div_impl(args, false)
}

pub fn op_div_deprecated<D: Dialect>(
    args: &SExp,
    _max_cost: u64,
    _dialect: &D,
) -> Result<(u64, SExp), Error> {
    op_div_impl(args, true)
}

pub fn op_divmod<D: Dialect>(
    args: &SExp,
    _max_cost: u64,
    _dialect: &D,
) -> Result<(u64, SExp), Error> {
    let (a0, l0, a1, l1) = two_ints(args, "divmod")?;
    let cost = DIV_MOD_BASE_COST + ((l0 + l1) as u64) * DIV_MOD_COST_PER_BYTE;
    if a1.sign() == Sign::NoSign {
        Err(Error::new(
            ErrorKind::Unsupported,
            format!("div with 0 : {:?}", args.first()?),
        ))
    } else {
        let (q, r) = a0.div_mod_floor(&a1);
        let q1 = SExp::try_from(&q)?;
        let r1 = SExp::try_from(&r)?;

        let c = (q1.atom()?.data.len() + r1.atom()?.data.len()) as u64 * MALLOC_COST_PER_BYTE;
        let r: SExp = q1.cons(r1);
        Ok((cost + c, r))
    }
}

pub fn op_gr<D: Dialect>(args: &SExp, _max_cost: u64, _dialect: &D) -> Result<(u64, SExp), Error> {
    check_arg_count(args, 2, ">")?;
    let a0 = args.first()?;
    let a1 = args.rest()?.first()?;
    let v0 = int_atom(a0, ">")?;
    let v1 = int_atom(a1, ">")?;
    let cost = GR_BASE_COST + (v0.len() + v1.len()) as u64 * GR_COST_PER_BYTE;
    Ok((
        cost,
        if number_from_slice(v0) > number_from_slice(v1) {
            ONE_SEXP.clone() //Todo maybe impl copy
        } else {
            NULL_SEXP.clone() //Todo maybe impl copy
        },
    ))
}

pub fn op_gr_bytes<D: Dialect>(
    args: &SExp,
    _max_cost: u64,
    _dialect: &D,
) -> Result<(u64, SExp), Error> {
    check_arg_count(args, 2, ">s")?;
    let a0 = args.first()?;
    let a1 = args.rest()?.first()?;
    let v0 = atom(a0, ">s")?;
    let v1 = atom(a1, ">s")?;
    let cost = GRS_BASE_COST + (v0.len() + v1.len()) as u64 * GRS_COST_PER_BYTE;
    Ok((
        cost,
        if v0 > v1 {
            ONE_SEXP.clone()
        } else {
            NULL_SEXP.clone()
        },
    ))
}

pub fn op_strlen<D: Dialect>(
    args: &SExp,
    _max_cost: u64,
    _dialect: &D,
) -> Result<(u64, SExp), Error> {
    check_arg_count(args, 1, "strlen")?;
    let a0 = args.first()?;
    let v0 = atom(a0, "strlen")?;
    let size = v0.len();
    let size_num: BigInt = size.into();
    let size_node = SExp::try_from(&size_num)?;
    let cost = STRLEN_BASE_COST + size as u64 * STRLEN_COST_PER_BYTE;
    malloc_cost(cost, size_node)
}

#[allow(clippy::cast_possible_truncation)]
#[allow(clippy::cast_sign_loss)]
#[allow(clippy::cast_possible_wrap)]
pub fn op_substr<D: Dialect>(
    args: &SExp,
    _max_cost: u64,
    _dialect: &D,
) -> Result<(u64, SExp), Error> {
    let ac = args.arg_count(3);
    if !(2..=3).contains(&ac) {
        return Err(Error::new(
            ErrorKind::Unsupported,
            format!("substr takes exactly 2 or 3 arguments: {args:?}"),
        ));
    }
    let a0 = args.first()?;
    let s0 = atom(a0, "substr")?;
    let size = s0.len();
    let rest = args.rest()?;
    let i1 = i32_atom(rest.first()?, "substr")?;
    let rest = rest.rest()?;

    let i2 = if ac == 3 {
        i32_atom(rest.first()?, "substr")?
    } else {
        size as i32
    };
    if i2 < 0 || i1 < 0 || i2 as usize > size || i2 < i1 {
        Err(Error::new(
            ErrorKind::Unsupported,
            format!("invalid indices for substr: {args:?}"),
        ))
    } else {
        let r = a0.substr(i1 as usize, i2 as usize)?;
        let cost: u64 = 1;
        Ok((cost, r))
    }
}

pub fn op_concat<D: Dialect>(
    args: &SExp,
    max_cost: u64,
    _dialect: &D,
) -> Result<(u64, SExp), Error> {
    let mut cost = CONCAT_BASE_COST;
    let mut total_size: usize = 0;
    let mut terms = Vec::<&SExp>::new();
    for arg in args {
        cost += CONCAT_COST_PER_ARG;
        check_cost(cost + total_size as u64 * CONCAT_COST_PER_BYTE, max_cost)?;
        match arg {
            SExp::Pair(_) => {
                return Err(Error::new(
                    ErrorKind::Unsupported,
                    format!("concat on list: {arg:?}"),
                ));
            }
            SExp::Atom(b) => total_size += b.data.len(),
        };
        terms.push(arg);
    }

    cost += total_size as u64 * CONCAT_COST_PER_BYTE;
    cost += total_size as u64 * MALLOC_COST_PER_BYTE;
    check_cost(cost, max_cost)?;
    let new_atom = SExp::concat(&terms)?;
    Ok((cost, new_atom))
}

pub fn op_ash<D: Dialect>(args: &SExp, _max_cost: u64, _dialect: &D) -> Result<(u64, SExp), Error> {
    check_arg_count(args, 2, "ash")?;
    let a0 = args.first()?;
    let b0 = int_atom(a0, "ash")?;
    let i0 = number_from_slice(b0);
    let l0 = b0.len() as u64;
    let rest = args.rest()?;
    let a1 = i32_atom(rest.first()?, "ash")?;
    if !(-65535..=65535).contains(&a1) {
        return Err(Error::new(
            ErrorKind::Unsupported,
            format!("shift too large: {:?}", args.rest()?.first()?),
        ));
    }

    let v: BigInt = if a1 > 0 { i0 << a1 } else { i0 >> -a1 };
    let l1 = limbs_for_int(&v);
    let r = SExp::try_from(&v)?;
    let cost = A_SHIFT_BASE_COST + (l0 + l1) * A_SHIFT_COST_PER_BYTE;
    malloc_cost(cost, r)
}

pub fn op_lsh<D: Dialect>(args: &SExp, _max_cost: u64, _dialect: &D) -> Result<(u64, SExp), Error> {
    check_arg_count(args, 2, "lsh")?;
    let a0 = args.first()?;
    let b0 = int_atom(a0, "lsh")?;
    let i0 = BigUint::from_bytes_be(b0);
    let l0 = b0.len() as u64;
    let rest = args.rest()?;
    let a1 = i32_atom(rest.first()?, "lsh")?;
    if !(-65535..=65535).contains(&a1) {
        return Err(Error::new(
            ErrorKind::Unsupported,
            format!("shift too large: {:?}", args.rest()?.first()?),
        ));
    }
    let i0: BigInt = i0.into();
    let v: BigInt = if a1 > 0 { i0 << a1 } else { i0 >> -a1 };
    let l1 = limbs_for_int(&v);
    let r = SExp::try_from(&v)?;
    let cost = LSHIFT_BASE_COST + (l0 + l1) * LSHIFT_COST_PER_BYTE;
    malloc_cost(cost, r)
}

fn binop_reduction(
    op_name: &'static str,
    initial_value: BigInt,
    input: &SExp,
    max_cost: u64,
    op_f: fn(&mut BigInt, &BigInt) -> (),
) -> Result<(u64, SExp), Error> {
    let mut total = initial_value;
    let mut arg_size: usize = 0;
    let mut cost = LOG_BASE_COST;
    for arg in input.iter() {
        let blob = int_atom(arg, op_name)?;
        let n0 = number_from_slice(blob);
        op_f(&mut total, &n0);
        arg_size += blob.len();
        cost += LOG_COST_PER_ARG;
        check_cost(cost + (arg_size as u64 * LOG_COST_PER_BYTE), max_cost)?;
    }
    cost += arg_size as u64 * LOG_COST_PER_BYTE;
    let total = SExp::try_from(&total)?;
    malloc_cost(cost, total)
}

fn logand_op(a: &mut BigInt, b: &BigInt) {
    a.bitand_assign(b);
}

pub fn op_logand<D: Dialect>(
    args: &SExp,
    max_cost: u64,
    _dialect: &D,
) -> Result<(u64, SExp), Error> {
    let v: BigInt = (-1).into();
    binop_reduction("logand", v, args, max_cost, logand_op)
}

fn logior_op(a: &mut BigInt, b: &BigInt) {
    a.bitor_assign(b);
}

pub fn op_logior<D: Dialect>(
    args: &SExp,
    max_cost: u64,
    _dialect: &D,
) -> Result<(u64, SExp), Error> {
    let v: BigInt = 0.into();
    binop_reduction("logior", v, args, max_cost, logior_op)
}

fn logxor_op(a: &mut BigInt, b: &BigInt) {
    a.bitxor_assign(b);
}

pub fn op_logxor<D: Dialect>(
    args: &SExp,
    max_cost: u64,
    _dialect: &D,
) -> Result<(u64, SExp), Error> {
    let v: BigInt = (0).into();
    binop_reduction("logxor", v, args, max_cost, logxor_op)
}

pub fn op_lognot<D: Dialect>(
    args: &SExp,
    _max_cost: u64,
    _dialect: &D,
) -> Result<(u64, SExp), Error> {
    check_arg_count(args, 1, "lognot")?;
    let a0 = args.first()?;
    let v0 = int_atom(a0, "lognot")?;
    let mut n: BigInt = number_from_slice(v0);
    n = !n;
    let cost = LOG_NOT_BASE_COST + ((v0.len() as u64) * LOG_NOT_COST_PER_BYTE);
    let r = SExp::try_from(&n)?;
    malloc_cost(cost, r)
}

pub fn op_not<D: Dialect>(args: &SExp, _max_cost: u64, _dialect: &D) -> Result<(u64, SExp), Error> {
    check_arg_count(args, 1, "not")?;
    let r: SExp = SExp::from_bool(!args.first()?.as_bool()).clone();
    let cost = BOOL_BASE_COST;
    Ok((cost, r))
}

pub fn op_any<D: Dialect>(args: &SExp, max_cost: u64, _dialect: &D) -> Result<(u64, SExp), Error> {
    let mut cost = BOOL_BASE_COST;
    let mut is_any = false;
    for arg in args.iter() {
        cost += BOOL_COST_PER_ARG;
        check_cost(cost, max_cost)?;
        is_any = is_any || arg.as_bool();
    }
    let total = SExp::from_bool(is_any).clone();
    Ok((cost, total))
}

pub fn op_all<D: Dialect>(args: &SExp, max_cost: u64, dialect: &D) -> Result<(u64, SExp), Error> {
    let mut cost = BOOL_BASE_COST;
    let mut is_all = true;
    match args.first() {
        Ok(arg) => {
            //Check for Special Print Case
            if arg.atom().map(|d| d.data.as_slice()).unwrap_or(&[]) == dialect.print_kw() {
                let mut out = NULL_SEXP.clone();
                for arg in args.iter().skip(1) {
                    out = arg.clone().cons(out);
                }
                let _ = op_print(&out, max_cost, dialect);
                cost += BOOL_COST_PER_ARG * 3;
                Ok((cost, SExp::from_bool(is_all).clone()))
            } else {
                //Normal Case
                for arg in args.iter() {
                    cost += BOOL_COST_PER_ARG;
                    check_cost(cost, max_cost)?;
                    is_all = is_all && arg.as_bool();
                }
                let total = SExp::from_bool(is_all).clone();
                Ok((cost, total))
            }
        }
        Err(_) => {
            let total = SExp::from_bool(is_all).clone();
            Ok((cost, total))
        }
    }
}

pub fn op_softfork<D: Dialect>(
    args: &SExp,
    max_cost: u64,
    _dialect: &D,
) -> Result<(u64, SExp), Error> {
    match args.pair() {
        Ok(pair) => {
            let n: BigInt = number_from_slice(int_atom(&pair.first, "softfork")?);
            if n.sign() == Sign::Plus {
                if n > BigInt::from(max_cost) {
                    return Err(Error::new(
                        ErrorKind::Unsupported,
                        format!("Max Cost({max_cost}) Exceded: {n}"),
                    ));
                }
                let cost: u64 = TryFrom::try_from(&n).map_err(|e| {
                    Error::new(
                        ErrorKind::InvalidData,
                        format!("Failed to convert Atom to Int: {e:?}"),
                    )
                })?;
                Ok((cost, NULL_SEXP.clone()))
            } else {
                Err(Error::new(
                    ErrorKind::Unsupported,
                    format!("Cost must be > 0, found {n}"),
                ))
            }
        }
        _ => Err(Error::new(
            ErrorKind::Unsupported,
            "Softfork takes at least 1 argument",
        )),
    }
}

static GROUP_ORDER: Lazy<BigInt> = Lazy::new(|| {
    let order_as_bytes = &[
        0x73, 0xed, 0xa7, 0x53, 0x29, 0x9d, 0x7d, 0x48, 0x33, 0x39, 0xd8, 0x08, 0x09, 0xa1, 0xd8,
        0x05, 0x53, 0xbd, 0xa4, 0x02, 0xff, 0xfe, 0x5b, 0xfe, 0xff, 0xff, 0xff, 0xff, 0x00, 0x00,
        0x00, 0x01,
    ];
    let n = BigUint::from_bytes_be(order_as_bytes);
    n.into()
});

fn mod_group_order(n: &BigInt) -> BigInt {
    let order = GROUP_ORDER.clone();
    let mut remainder = n.mod_floor(&order);
    if remainder.sign() == Sign::Minus {
        remainder += order;
    }
    remainder
}

fn number_to_scalar(n: &BigInt) -> Scalar {
    let (sign, as_u8): (Sign, Vec<u8>) = n.to_bytes_le();
    let mut scalar_array: [u8; 32] = [0; 32];
    scalar_array[..as_u8.len()].clone_from_slice(&as_u8[..]);
    let exp: Scalar = Scalar::from_bytes(&scalar_array).unwrap();
    if sign == Sign::Minus {
        exp.neg()
    } else {
        exp
    }
}

pub fn op_pubkey_for_exp<D: Dialect>(
    args: &SExp,
    _max_cost: u64,
    _dialect: &D,
) -> Result<(u64, SExp), Error> {
    check_arg_count(args, 1, "pubkey_for_exp")?;
    let a0 = args.first()?;
    let v0 = int_atom(a0, "pubkey_for_exp")?;
    let exp: BigInt = mod_group_order(&number_from_slice(v0));
    let cost = PUBKEY_BASE_COST + (v0.len() as u64) * PUBKEY_COST_PER_BYTE;
    let exp: Scalar = number_to_scalar(&exp);
    let point: G1Projective = G1Affine::generator() * exp;
    let point: G1Affine = point.into();
    Ok(new_atom_and_cost(cost, &point.to_compressed()))
}

pub fn op_point_add<D: Dialect>(
    args: &SExp,
    max_cost: u64,
    _dialect: &D,
) -> Result<(u64, SExp), Error> {
    let mut cost = POINT_ADD_BASE_COST;
    let mut total: G1Projective = G1Projective::identity();
    for arg in args {
        let blob = atom(arg, "point_add")?;
        let mut is_ok: bool = blob.len() == 48;
        if is_ok {
            let mut as_array: [u8; 48] = [0; 48];
            as_array.clone_from_slice(&blob[0..48]);
            let v = G1Affine::from_compressed(&as_array);
            is_ok = v.is_some().into();
            if is_ok {
                let point = v.unwrap();
                cost += POINT_ADD_COST_PER_ARG;
                check_cost(cost, max_cost)?;
                total += &point;
            }
        } else {
            let blob: String = hex::encode(sexp_to_bytes(arg)?);
            let msg = format!("point_add expects blob, got {blob}: Length of bytes object not equal to G1Element::SIZE");
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!("{msg} {args:?}"),
            ));
        }
    }
    let total: G1Affine = total.into();
    Ok(new_atom_and_cost(cost, &total.to_compressed()))
}

pub fn op_coinid<D: Dialect>(
    args: &SExp,
    _max_cost: u64,
    _dialect: &D,
) -> Result<(u64, SExp), Error> {
    let mut args_list = args.as_atom_list();
    if args_list.len() != 3 {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!("coinid expects 3 args, got {} {args:?}", args_list.len()),
        ));
    }
    let amount = args_list.pop().expect("Length Already Checked");
    let puzzle_hash = args_list.pop().expect("Length Already Checked");
    let parent_coin_info = args_list.pop().expect("Length Already Checked");
    if parent_coin_info.len() != 32 {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!("invalid parent coin id, {}", hex::encode(&parent_coin_info)),
        ));
    }
    if puzzle_hash.len() != 32 {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!("invalid puzzle hash, {}", hex::encode(&puzzle_hash)),
        ));
    }
    let as_int = if !amount.is_empty() {
        let as_int = number_from_slice(&amount);
        if as_int.is_negative() {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!(
                    "coin amount cannot be negative, {}",
                    number_from_slice(&amount)
                ),
            ));
        }
        if amount.len() > 9 || (amount.len() == 9 && amount[0] != 0) {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!("coin amount exceeds max, {as_int}"),
            ));
        }
        as_int
    } else {
        BigInt::zero()
    };
    let coin = Coin {
        parent_coin_info: Bytes32::parse(&parent_coin_info)?,
        puzzle_hash: Bytes32::parse(&puzzle_hash)?,
        amount: u64_from_bigint(&as_int)?,
    };
    Ok((
        COIN_ID_COST,
        SExp::Atom(AtomBuf::new(coin.coin_id().into())),
    ))
}
