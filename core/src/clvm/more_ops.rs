use crate::blockchain::coin::Coin;
use crate::blockchain::sized_bytes::Bytes32;
use crate::clvm::arena::{Arena, ArgCursor, NodeKind, NodePtr};
use crate::clvm::debug_ops::op_print;
use crate::clvm::dialect::Dialect;
use crate::clvm::pure_ops::OpOut;
use crate::clvm::sexp_ext::SExpNumber;
use crate::clvm::utils::{
    CANONICAL_INTS, DISABLE_OP, LIMITS, NEW_COST_MODEL, atom, check_arg_count, check_cost,
    i32_atom, int_atom, number_with_len, split, two_ints,
};
use crate::errors::ClvmError;
use crate::formatting::{number_from_slice, u32_from_slice, u64_from_bigint};
use crate::traits::SizedBytes;
use num_bigint::{BigInt, BigUint, Sign};
#[cfg(feature = "bls")]
use num_integer::Integer;
use num_traits::{Signed, Zero};
#[cfg(feature = "bls")]
use once_cell::sync::Lazy;
use std::ops::BitAndAssign;
use std::ops::BitOrAssign;
use std::ops::BitXorAssign;

pub(crate) const MALLOC_COST_PER_BYTE: u64 = 10;

const ARITH_BASE_COST: u64 = 99;
// Hard-fork operator costs: `modpow` (60) and `mod` (61), active on mainnet since the hard
// fork at 5,496,000. The NEW_* constants are the NEW_COST_MODEL variants; the two models are
// selected at dispatch by the NEW_COST_MODEL flag.
const MODPOW_BASE_COST: u64 = 17000;
const MODPOW_COST_PER_BYTE_BASE_VALUE: u64 = 38;
const MODPOW_COST_PER_BYTE_EXPONENT: u64 = 3;
const MODPOW_COST_PER_BYTE_MOD: u64 = 21;
const NEW_MODPOW_PER_ITERATION_COST: u64 = 4000;
const NEW_MODPOW_EXPONENT_MULTIPLIER: u64 = 8;
const NEW_DIV_BASE_COST: u64 = 1000;
const NEW_DIV_LINEAR_COST_PER_BYTE: u64 = 50;
const NEW_DIV_SQUARE_COST_PER_BYTE_DIVIDER: u64 = 10;
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
#[cfg(feature = "bls")]
const POINT_ADD_BASE_COST: u64 = 101_094;
// increased from 419994 to better model Raspberry PI
#[cfg(feature = "bls")]
const POINT_ADD_COST_PER_ARG: u64 = 1_343_980;

// Raspberry PI 4 is about 2.833543 / 0.447859 = 6.32686 times slower
// in the pubkey benchmark

// increased from 419535 to better model Raspberry PI
#[cfg(feature = "bls")]
const PUBKEY_BASE_COST: u64 = 1_325_730;
// increased from 12 to closer model Raspberry PI
#[cfg(feature = "bls")]
const PUBKEY_COST_PER_BYTE: u64 = 38;

const COIN_ID_COST: u64 =
    SHA256_BASE_COST + SHA256_COST_PER_ARG * 3 + SHA256_COST_PER_BYTE * (32 + 32 + 8) - 153;

fn limbs_for_int(v: &BigInt) -> u64 {
    v.bits().div_ceil(8)
}

fn limbs_for_num(v: &SExpNumber) -> u64 {
    match v {
        SExpNumber::BigInt(int) => int.bits().div_ceil(8),
        SExpNumber::I128(i) => u64::from(128 - i.unsigned_abs().leading_zeros()).div_ceil(8),
    }
}

pub(crate) fn new_atom_and_cost(cost: u64, buf: &[u8]) -> Result<(u64, OpOut), ClvmError> {
    let c = buf.len() as u64 * MALLOC_COST_PER_BYTE;
    Ok((cost + c, OpOut::small(buf)))
}

fn malloc_number(cost: u64, n: &SExpNumber) -> Result<(u64, OpOut), ClvmError> {
    // The malloc surcharge depends on the encoded length, which only exists once the arena has
    // written it — the materialization site adds it.
    Ok((cost, OpOut::Number(n.clone())))
}

fn malloc_bigint(cost: u64, n: &BigInt) -> Result<(u64, OpOut), ClvmError> {
    Ok((cost, OpOut::Number(SExpNumber::BigInt(n.clone()))))
}

pub fn op_unknown<D: Dialect>(
    arena: &Arena,
    o: NodePtr,
    args: NodePtr,
    max_cost: u64,
    _dialect: &D,
) -> Result<(u64, OpOut), ClvmError> {
    let (cost_function, cost_multiplier) = {
        let op_atom = arena
            .atom(o)
            .ok_or_else(|| ClvmError::ExpectedAtomGotPair(arena.display(o)))?;
        let op = op_atom.as_ref();
        if op.is_empty() || (op.len() >= 2 && op[0] == 0xff && op[1] == 0xff) {
            return Err(ClvmError::ReservedOperator(format!(
                "Reserved Operator: {op:?}"
            )));
        }
        let cost_function = (op[op.len() - 1] & 0b1100_0000) >> 6;
        let cost_multiplier: u64 = match u32_from_slice(&op[0..op.len() - 1]) {
            Some(v) => u64::from(v),
            None => {
                return Err(ClvmError::InvalidOperator(format!(
                    "Invalid Operator: {op:?}"
                )));
            }
        };
        (cost_function, cost_multiplier)
    };
    let mut cost = match cost_function {
        1 => {
            let mut cost = ARITH_BASE_COST;
            let mut byte_count: u64 = 0;
            let mut cursor = ArgCursor::new(args);
            while let Some(arg) = cursor.next(arena) {
                cost += ARITH_COST_PER_ARG;
                let blob = int_atom(arena, arg, "unknown op")?;
                byte_count += blob.len() as u64;
                check_cost(cost + (byte_count * ARITH_COST_PER_BYTE), max_cost)?;
            }
            cost + (byte_count * ARITH_COST_PER_BYTE)
        }
        2 => {
            let mut cost = MUL_BASE_COST;
            let mut first_iter: bool = true;
            let mut l0: u64 = 0;
            let mut cursor = ArgCursor::new(args);
            while let Some(arg) = cursor.next(arena) {
                let blob = int_atom(arena, arg, "unknown op")?;
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
            let mut cursor = ArgCursor::new(args);
            while let Some(arg) = cursor.next(arena) {
                cost += CONCAT_COST_PER_ARG;
                let blob = atom(arena, arg, "unknown op")?;
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
        Err(ClvmError::Unsupported(format!(
            "Invalid Operator: {}",
            arena.debug_fmt(o)
        )))
    } else {
        Ok((cost, OpOut::Same(NodePtr::NIL)))
    }
}

pub fn op_sha256<D: Dialect>(
    arena: &Arena,
    args: NodePtr,
    max_cost: u64,
    _dialect: &D,
) -> Result<(u64, OpOut), ClvmError> {
    let mut cost = SHA256_BASE_COST;
    let mut byte_count: usize = 0;
    // ring's SHA-256 (BoringSSL's vectorized block function) sustains ~1.5x the sha2 crate's
    // software throughput on x86 without SHA-NI and edges it on aarch64 NEON — and cost-maxed
    // mainnet blocks exist that hash ~5 GiB through this one operator (2 cost/byte), where that
    // ratio is the whole body-validation wall clock.
    let mut hasher = ring::digest::Context::new(&ring::digest::SHA256);
    let mut cursor = ArgCursor::new(args);
    while let Some(arg) = cursor.next(arena) {
        cost += SHA256_COST_PER_ARG;
        check_cost(cost + byte_count as u64 * SHA256_COST_PER_BYTE, max_cost)?;
        let blob = atom(arena, arg, "sha256")?;
        byte_count += blob.len();
        hasher.update(blob.as_ref());
    }
    cost += byte_count as u64 * SHA256_COST_PER_BYTE;
    let digest = hasher.finish();
    // DGXCH_TRACE_SHA256=1 logs every sha256 call's args + digest. Checked once per
    // process; a per-call env read would serialize across replay workers.
    static TRACE_SHA256: std::sync::LazyLock<bool> =
        std::sync::LazyLock::new(|| std::env::var_os("DGXCH_TRACE_SHA256").is_some());
    if *TRACE_SHA256 {
        let mut dump = String::new();
        let mut cursor = ArgCursor::new(args);
        while let Some(arg) = cursor.next(arena) {
            if let Some(blob) = arena.atom(arg) {
                dump.push(' ');
                for b in blob.as_ref() {
                    dump.push_str(&format!("{b:02x}"));
                }
            }
        }
        eprintln!(
            "sha256_trace:{} ->{}",
            dump,
            digest
                .as_ref()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        );
    }
    new_atom_and_cost(cost, digest.as_ref())
}

pub fn op_add<D: Dialect>(
    arena: &Arena,
    args: NodePtr,
    max_cost: u64,
    _dialect: &D,
) -> Result<(u64, OpOut), ClvmError> {
    let mut cost = ARITH_BASE_COST;
    let mut byte_count: usize = 0;
    let mut total = SExpNumber::I128(0);
    let mut cursor = ArgCursor::new(args);
    while let Some(blob) = cursor.next(arena) {
        cost += ARITH_COST_PER_ARG;
        check_cost(cost + (byte_count as u64 * ARITH_COST_PER_BYTE), max_cost)?;
        let num_with_len = number_with_len(arena, blob)?;
        total += num_with_len.0;
        byte_count += num_with_len.1;
    }
    cost += byte_count as u64 * ARITH_COST_PER_BYTE;
    malloc_number(cost, &total)
}

pub fn op_subtract<D: Dialect>(
    arena: &Arena,
    args: NodePtr,
    max_cost: u64,
    _dialect: &D,
) -> Result<(u64, OpOut), ClvmError> {
    let mut cost = ARITH_BASE_COST;
    let mut byte_count: usize = 0;
    let mut first = true;
    let mut total = SExpNumber::I128(0);
    let mut cursor = ArgCursor::new(args);
    while let Some(blob) = cursor.next(arena) {
        cost += ARITH_COST_PER_ARG;
        check_cost(cost + (byte_count as u64 * ARITH_COST_PER_BYTE), max_cost)?;
        let num_with_len = number_with_len(arena, blob)?;
        byte_count += num_with_len.1;
        if first {
            first = false;
            total = num_with_len.0;
        } else {
            total -= num_with_len.0;
        }
    }
    cost += byte_count as u64 * ARITH_COST_PER_BYTE;
    malloc_number(cost, &total)
}

pub fn op_multiply<D: Dialect>(
    arena: &Arena,
    args: NodePtr,
    max_cost: u64,
    _dialect: &D,
) -> Result<(u64, OpOut), ClvmError> {
    let mut first: bool = true;
    let mut cost: u64 = MUL_BASE_COST;
    let mut total = SExpNumber::I128(1);
    let mut l0 = 0u64;
    let mut cursor = ArgCursor::new(args);
    while let Some(blob) = cursor.next(arena) {
        check_cost(cost, max_cost)?;
        let num_with_len = number_with_len(arena, blob)?;
        if first {
            l0 = num_with_len.1 as u64;
            total = num_with_len.0;
            first = false;
            continue;
        }
        let l1 = num_with_len.1 as u64;
        total *= num_with_len.0;
        cost += MUL_COST_PER_OP;
        cost += (l0 + l1) * MUL_LINEAR_COST_PER_BYTE;
        cost += (l0 * l1) / MUL_SQUARE_COST_PER_BYTE_DIVIDER;
        l0 = limbs_for_num(&total);
    }
    trace_arith(arena, "mul", args, &total);
    malloc_number(cost, &total)
}

// DGXCH_TRACE_ARITH=1 logs each arithmetic op's raw operand atoms and result.
// Checked once per process, as with the sha256 tap.
fn trace_arith(arena: &Arena, op: &str, args: NodePtr, result: &SExpNumber) {
    static TRACE_ARITH: std::sync::LazyLock<bool> =
        std::sync::LazyLock::new(|| std::env::var_os("DGXCH_TRACE_ARITH").is_some());
    if !*TRACE_ARITH {
        return;
    }
    let mut dump = String::new();
    let mut cursor = ArgCursor::new(args);
    while let Some(arg) = cursor.next(arena) {
        dump.push(' ');
        match arena.atom(arg) {
            Some(a) => {
                for b in a.as_ref() {
                    dump.push_str(&format!("{b:02x}"));
                }
            }
            None => dump.push_str("<pair>"),
        }
    }
    match result {
        SExpNumber::I128(i) => eprintln!("arith_trace:{op}{dump} ->{i}"),
        SExpNumber::BigInt(b) => eprintln!("arith_trace:{op}{dump} ->{b}"),
    }
}

pub fn op_div_impl(arena: &Arena, args: NodePtr, mempool: bool) -> Result<(u64, OpOut), ClvmError> {
    let ((a0, l0), (a1, l1)) = two_ints(arena, args, "/")?;
    let cost = DIV_BASE_COST + ((l0 + l1) as u64) * DIV_COST_PER_BYTE;
    if a1.sign() == Sign::NoSign {
        Err(ClvmError::Unsupported(format!(
            "div with 0 : {}",
            arena.debug_fmt(split(arena, args)?.0)
        )))?
    } else {
        if mempool && (a0.sign() == Sign::Minus || a1.sign() == Sign::Minus) {
            Err(ClvmError::Unsupported(format!(
                "div operator with negative operands is deprecated: {}",
                arena.debug_fmt(args)
            )))?
        }
        let (mut q, r) = a0.div_mod_floor(&a1);
        // this is to preserve a buggy behavior from the initial implementation of this operator.
        if q == SExpNumber::I128(-1) && !r.is_zero() {
            q += SExpNumber::I128(1);
        }
        trace_arith(arena, "div", args, &q);
        malloc_number(cost, &q)
    }
}

pub fn op_div<D: Dialect>(
    arena: &Arena,
    args: NodePtr,
    _max_cost: u64,
    _dialect: &D,
) -> Result<(u64, OpOut), ClvmError> {
    op_div_impl(arena, args, false)
}

// NEW_COST_MODEL div cost: linear + quadratic-by-product terms.
fn compute_new_div_cost(l0: usize, l1: usize) -> Result<u64, ClvmError> {
    let mut cost = NEW_DIV_BASE_COST;
    cost += (l0 as u64 + l1 as u64) * NEW_DIV_LINEAR_COST_PER_BYTE;
    let square = (l0 as u64)
        .checked_mul(l1 as u64)
        .ok_or_else(|| ClvmError::Overflow("new div cost".to_string()))?;
    Ok(cost + square / NEW_DIV_SQUARE_COST_PER_BYTE_DIVIDER)
}

// modpow cost, both models.
fn compute_modpow_cost(
    bsize: usize,
    esize: usize,
    msize: usize,
    new_cost_model: bool,
) -> Result<u64, ClvmError> {
    let mut cost = MODPOW_BASE_COST;
    if new_cost_model {
        let m = msize as u64;
        let inner = m
            .checked_mul(m)
            .and_then(|mm| mm.checked_add(NEW_MODPOW_PER_ITERATION_COST))
            .ok_or_else(|| ClvmError::Overflow("modpow cost".to_string()))?;
        let exp_term = (esize as u64)
            .checked_mul(NEW_MODPOW_EXPONENT_MULTIPLIER)
            .and_then(|e| e.checked_mul(inner))
            .ok_or_else(|| ClvmError::Overflow("modpow cost".to_string()))?;
        cost = cost
            .checked_add(exp_term)
            .and_then(|c| c.checked_add((bsize as u64).checked_mul(m)?))
            .ok_or_else(|| ClvmError::Overflow("modpow cost".to_string()))?;
    } else {
        cost += bsize as u64 * MODPOW_COST_PER_BYTE_BASE_VALUE;
        cost += (esize as u64 * esize as u64) * MODPOW_COST_PER_BYTE_EXPONENT;
        cost += (msize as u64 * msize as u64) * MODPOW_COST_PER_BYTE_MOD;
    }
    Ok(cost)
}

fn number_to_bigint(n: SExpNumber) -> BigInt {
    match n {
        SExpNumber::I128(i) => BigInt::from(i),
        SExpNumber::BigInt(b) => b,
    }
}

// `mod` (operator 61): floor modulus, division-by-zero rejected, malloc-costed result.
// DISABLE_OP caps the dividend at 2048 bytes and LIMITS caps operands at 256/1024
// (both only until NEW_COST_MODEL bounds the cost instead).
pub fn op_mod<D: Dialect>(
    arena: &Arena,
    args: NodePtr,
    _max_cost: u64,
    dialect: &D,
) -> Result<(u64, OpOut), ClvmError> {
    let flags = dialect.flags();
    let new_cost_model = (flags & NEW_COST_MODEL) != 0;
    let ((a0, l0), (a1, l1)) = two_ints(arena, args, "mod")?;
    if (flags & DISABLE_OP) != 0 && !new_cost_model && l0 > 2048 {
        Err(ClvmError::Unsupported("mod operand too large".to_string()))?;
    }
    if (flags & LIMITS) != 0 && !new_cost_model && (l0 > 256 || l1 > 1024) {
        Err(ClvmError::Unsupported("mod operand too large".to_string()))?;
    }
    let cost = if new_cost_model {
        compute_new_div_cost(l0, l1)?
    } else {
        DIV_BASE_COST + ((l0 + l1) as u64) * DIV_COST_PER_BYTE
    };
    if a1.sign() == Sign::NoSign {
        Err(ClvmError::Unsupported(format!(
            "mod with 0 : {}",
            arena.debug_fmt(split(arena, args)?.0)
        )))?;
    }
    let (_, r) = a0.div_mod_floor(&a1);
    malloc_number(cost, &r)
}

// `modpow` (operator 60): base^exponent mod modulus; negative exponent and zero
// modulus rejected; LIMITS caps every operand at 256 bytes until NEW_COST_MODEL; malloc-costed
// result. Dispatch rejects the operator entirely under DISABLE_OP (soft fork 8).
pub fn op_modpow<D: Dialect>(
    arena: &Arena,
    args: NodePtr,
    max_cost: u64,
    dialect: &D,
) -> Result<(u64, OpOut), ClvmError> {
    let flags = dialect.flags();
    let new_cost_model = (flags & NEW_COST_MODEL) != 0;
    check_arg_count(arena, args, 3, "modpow")?;
    let (first, rest) = split(arena, args)?;
    let (base, bsize) = number_with_len(arena, first)?;
    let (second, rest) = split(arena, rest)?;
    let (exponent, esize) = number_with_len(arena, second)?;
    let third = split(arena, rest)?.0;
    let (modulus, msize) = number_with_len(arena, third)?;

    let cost = compute_modpow_cost(bsize, esize, msize, new_cost_model)?;
    check_cost(cost, max_cost)?;

    if (flags & LIMITS) != 0 && !new_cost_model && (bsize > 256 || esize > 256 || msize > 256) {
        Err(ClvmError::Unsupported(
            "modpow operand too large".to_string(),
        ))?;
    }
    if exponent.sign() == Sign::Minus {
        Err(ClvmError::Unsupported(
            "ModPow with Negative Exponent".to_string(),
        ))?;
    }
    if modulus.sign() == Sign::NoSign {
        Err(ClvmError::Unsupported("modpow with 0 modulus".to_string()))?;
    }
    let ret =
        number_to_bigint(base).modpow(&number_to_bigint(exponent), &number_to_bigint(modulus));
    malloc_bigint(cost, &ret)
}

pub fn op_div_deprecated<D: Dialect>(
    arena: &Arena,
    args: NodePtr,
    _max_cost: u64,
    _dialect: &D,
) -> Result<(u64, OpOut), ClvmError> {
    op_div_impl(arena, args, true)
}

pub fn op_divmod<D: Dialect>(
    arena: &Arena,
    args: NodePtr,
    _max_cost: u64,
    _dialect: &D,
) -> Result<(u64, OpOut), ClvmError> {
    let ((a0, l0), (a1, l1)) = two_ints(arena, args, "/")?;
    let cost = DIV_MOD_BASE_COST + ((l0 + l1) as u64) * DIV_MOD_COST_PER_BYTE;
    if a1.sign() == Sign::NoSign {
        Err(ClvmError::Unsupported(format!(
            "div with 0 : {}",
            arena.debug_fmt(split(arena, args)?.0)
        )))
    } else {
        let (q, r) = a0.div_mod_floor(&a1);
        Ok((cost, OpOut::NumberPair(Box::new((q, r)))))
    }
}

pub fn op_gr<D: Dialect>(
    arena: &Arena,
    args: NodePtr,
    _max_cost: u64,
    _dialect: &D,
) -> Result<(u64, OpOut), ClvmError> {
    check_arg_count(arena, args, 2, ">")?;
    let (a0, rest) = split(arena, args)?;
    let a1 = split(arena, rest)?.0;
    let v0 = int_atom(arena, a0, ">")?;
    let v1 = int_atom(arena, a1, ">")?;
    let cost = GR_BASE_COST + (v0.len() + v1.len()) as u64 * GR_COST_PER_BYTE;
    Ok((
        cost,
        OpOut::Same(
            if number_from_slice(v0.as_ref()) > number_from_slice(v1.as_ref()) {
                NodePtr::ONE
            } else {
                NodePtr::NIL
            },
        ),
    ))
}

pub fn op_gr_bytes<D: Dialect>(
    arena: &Arena,
    args: NodePtr,
    _max_cost: u64,
    _dialect: &D,
) -> Result<(u64, OpOut), ClvmError> {
    check_arg_count(arena, args, 2, ">s")?;
    let (a0, rest) = split(arena, args)?;
    let a1 = split(arena, rest)?.0;
    let v0 = atom(arena, a0, ">s")?;
    let v1 = atom(arena, a1, ">s")?;
    let cost = GRS_BASE_COST + (v0.len() + v1.len()) as u64 * GRS_COST_PER_BYTE;
    Ok((
        cost,
        OpOut::Same(if v0.as_ref() > v1.as_ref() {
            NodePtr::ONE
        } else {
            NodePtr::NIL
        }),
    ))
}

pub fn op_strlen<D: Dialect>(
    arena: &Arena,
    args: NodePtr,
    _max_cost: u64,
    _dialect: &D,
) -> Result<(u64, OpOut), ClvmError> {
    check_arg_count(arena, args, 1, "strlen")?;
    let a0 = split(arena, args)?.0;
    let size = atom(arena, a0, "strlen")?.len();
    let cost = STRLEN_BASE_COST + size as u64 * STRLEN_COST_PER_BYTE;
    #[allow(clippy::cast_possible_wrap)]
    malloc_number(cost, &SExpNumber::I128(size as i128))
}

#[allow(clippy::cast_possible_truncation)]
#[allow(clippy::cast_sign_loss)]
#[allow(clippy::cast_possible_wrap)]
pub fn op_substr<D: Dialect>(
    arena: &Arena,
    args: NodePtr,
    _max_cost: u64,
    _dialect: &D,
) -> Result<(u64, OpOut), ClvmError> {
    let ac = arena.arg_count(args, 3);
    if !(2..=3).contains(&ac) {
        Err(ClvmError::Unsupported(format!(
            "substr takes exactly 2 or 3 arguments: {}",
            arena.debug_fmt(args)
        )))?;
    }
    let (a0, rest) = split(arena, args)?;
    let size = atom(arena, a0, "substr")?.len();
    let (i1_node, rest) = split(arena, rest)?;
    let i1 = i32_atom(arena, i1_node, "substr")?;
    let i2 = if ac == 3 {
        let i2_node = split(arena, rest)?.0;
        i32_atom(arena, i2_node, "substr")?
    } else {
        size as i32
    };
    if i2 < 0 || i1 < 0 || i2 as usize > size || i2 < i1 {
        Err(ClvmError::Unsupported(format!(
            "invalid indices for substr: {}",
            arena.debug_fmt(args)
        )))
    } else {
        // Zero-copy view into the source atom: the op charges base cost 1 with no malloc
        // cost, so a copying substr would be unmetered allocation.
        let r = OpOut::Substr(a0, i1 as u32, i2 as u32);
        let cost: u64 = 1;
        Ok((cost, r))
    }
}

pub fn op_concat<D: Dialect>(
    arena: &Arena,
    args: NodePtr,
    max_cost: u64,
    _dialect: &D,
) -> Result<(u64, OpOut), ClvmError> {
    let mut cost = CONCAT_BASE_COST;
    let mut total_size: usize = 0;
    let mut terms = Vec::<NodePtr>::new();
    let mut cursor = ArgCursor::new(args);
    while let Some(arg) = cursor.next(arena) {
        cost += CONCAT_COST_PER_ARG;
        check_cost(cost + total_size as u64 * CONCAT_COST_PER_BYTE, max_cost)?;
        match arena.atom_len(arg) {
            None => {
                return Err(ClvmError::Unsupported(format!(
                    "concat on list: {}",
                    arena.debug_fmt(arg)
                )));
            }
            Some(len) => total_size += len,
        };
        terms.push(arg);
    }

    cost += total_size as u64 * CONCAT_COST_PER_BYTE;
    cost += total_size as u64 * MALLOC_COST_PER_BYTE;
    check_cost(cost, max_cost)?;
    Ok((cost, OpOut::Concat(terms, total_size)))
}

pub fn op_ash<D: Dialect>(
    arena: &Arena,
    args: NodePtr,
    _max_cost: u64,
    _dialect: &D,
) -> Result<(u64, OpOut), ClvmError> {
    check_arg_count(arena, args, 2, "ash")?;
    let (a0, rest) = split(arena, args)?;
    let (i0, l0) = {
        let b0 = int_atom(arena, a0, "ash")?;
        (number_from_slice(b0.as_ref()), b0.len() as u64)
    };
    let a1_node = split(arena, rest)?.0;
    let a1 = i32_atom(arena, a1_node, "ash")?;
    if !(-65535..=65535).contains(&a1) {
        return Err(ClvmError::Unsupported(format!(
            "shift too large: {}",
            arena.debug_fmt(a1_node)
        )));
    }

    let v: BigInt = if a1 > 0 { i0 << a1 } else { i0 >> -a1 };
    let l1 = limbs_for_int(&v);
    let cost = A_SHIFT_BASE_COST + (l0 + l1) * A_SHIFT_COST_PER_BYTE;
    malloc_bigint(cost, &v)
}

pub fn op_lsh<D: Dialect>(
    arena: &Arena,
    args: NodePtr,
    _max_cost: u64,
    _dialect: &D,
) -> Result<(u64, OpOut), ClvmError> {
    check_arg_count(arena, args, 2, "lsh")?;
    let (a0, rest) = split(arena, args)?;
    let (i0, l0) = {
        let b0 = int_atom(arena, a0, "lsh")?;
        (BigUint::from_bytes_be(b0.as_ref()), b0.len() as u64)
    };
    let a1_node = split(arena, rest)?.0;
    let a1 = i32_atom(arena, a1_node, "lsh")?;
    if !(-65535..=65535).contains(&a1) {
        return Err(ClvmError::Unsupported(format!(
            "shift too large: {}",
            arena.debug_fmt(a1_node)
        )));
    }
    let i0: BigInt = i0.into();
    let v: BigInt = if a1 > 0 { i0 << a1 } else { i0 >> -a1 };
    let l1 = limbs_for_int(&v);
    let cost = LSHIFT_BASE_COST + (l0 + l1) * LSHIFT_COST_PER_BYTE;
    malloc_bigint(cost, &v)
}

fn binop_reduction(
    op_name: &'static str,
    initial_value: BigInt,
    arena: &Arena,
    input: NodePtr,
    max_cost: u64,
    op_f: fn(&mut BigInt, &BigInt) -> (),
) -> Result<(u64, OpOut), ClvmError> {
    let mut total = initial_value;
    let mut arg_size: usize = 0;
    let mut cost = LOG_BASE_COST;
    let mut cursor = ArgCursor::new(input);
    while let Some(arg) = cursor.next(arena) {
        let (n0, blob_len) = {
            let blob = int_atom(arena, arg, op_name)?;
            (number_from_slice(blob.as_ref()), blob.len())
        };
        op_f(&mut total, &n0);
        arg_size += blob_len;
        cost += LOG_COST_PER_ARG;
        check_cost(cost + (arg_size as u64 * LOG_COST_PER_BYTE), max_cost)?;
    }
    cost += arg_size as u64 * LOG_COST_PER_BYTE;
    malloc_bigint(cost, &total)
}

fn logand_op(a: &mut BigInt, b: &BigInt) {
    a.bitand_assign(b);
}

pub fn op_logand<D: Dialect>(
    arena: &Arena,
    args: NodePtr,
    max_cost: u64,
    _dialect: &D,
) -> Result<(u64, OpOut), ClvmError> {
    let v: BigInt = (-1).into();
    binop_reduction("logand", v, arena, args, max_cost, logand_op)
}

fn logior_op(a: &mut BigInt, b: &BigInt) {
    a.bitor_assign(b);
}

pub fn op_logior<D: Dialect>(
    arena: &Arena,
    args: NodePtr,
    max_cost: u64,
    _dialect: &D,
) -> Result<(u64, OpOut), ClvmError> {
    let v: BigInt = 0.into();
    binop_reduction("logior", v, arena, args, max_cost, logior_op)
}

fn logxor_op(a: &mut BigInt, b: &BigInt) {
    a.bitxor_assign(b);
}

pub fn op_logxor<D: Dialect>(
    arena: &Arena,
    args: NodePtr,
    max_cost: u64,
    _dialect: &D,
) -> Result<(u64, OpOut), ClvmError> {
    let v: BigInt = (0).into();
    binop_reduction("logxor", v, arena, args, max_cost, logxor_op)
}

pub fn op_lognot<D: Dialect>(
    arena: &Arena,
    args: NodePtr,
    _max_cost: u64,
    _dialect: &D,
) -> Result<(u64, OpOut), ClvmError> {
    check_arg_count(arena, args, 1, "lognot")?;
    let a0 = split(arena, args)?.0;
    let (mut n, v0_len) = {
        let v0 = int_atom(arena, a0, "lognot")?;
        (number_from_slice(v0.as_ref()), v0.len())
    };
    n = !n;
    let cost = LOG_NOT_BASE_COST + ((v0_len as u64) * LOG_NOT_COST_PER_BYTE);
    malloc_bigint(cost, &n)
}

pub fn op_not<D: Dialect>(
    arena: &Arena,
    args: NodePtr,
    _max_cost: u64,
    _dialect: &D,
) -> Result<(u64, OpOut), ClvmError> {
    check_arg_count(arena, args, 1, "not")?;
    let a0 = split(arena, args)?.0;
    let r = if arena.non_nil(a0) {
        NodePtr::NIL
    } else {
        NodePtr::ONE
    };
    let cost = BOOL_BASE_COST;
    Ok((cost, OpOut::Same(r)))
}

pub fn op_any<D: Dialect>(
    arena: &Arena,
    args: NodePtr,
    max_cost: u64,
    _dialect: &D,
) -> Result<(u64, OpOut), ClvmError> {
    let mut cost = BOOL_BASE_COST;
    let mut is_any = false;
    let mut cursor = ArgCursor::new(args);
    while let Some(arg) = cursor.next(arena) {
        cost += BOOL_COST_PER_ARG;
        check_cost(cost, max_cost)?;
        is_any = is_any || arena.non_nil(arg);
    }
    Ok((
        cost,
        OpOut::Same(if is_any { NodePtr::ONE } else { NodePtr::NIL }),
    ))
}

pub fn op_all<D: Dialect>(
    arena: &Arena,
    args: NodePtr,
    max_cost: u64,
    dialect: &D,
) -> Result<(u64, OpOut), ClvmError> {
    let mut cost = BOOL_BASE_COST;
    let mut is_all = true;
    match arena.node_kind(args) {
        NodeKind::Pair(first, _) => {
            // Check for Special Print Case
            let is_print = arena
                .atom(first)
                .is_some_and(|a| a.as_ref() == dialect.print_kw());
            if is_print {
                let mut print_args = Vec::new();
                let mut cursor = ArgCursor::new(args);
                let mut skipped = false;
                while let Some(arg) = cursor.next(arena) {
                    if skipped {
                        print_args.push(arg);
                    } else {
                        skipped = true;
                    }
                }
                let _ = op_print(arena, &print_args, max_cost, dialect);
                cost += BOOL_COST_PER_ARG * 3;
                Ok((
                    cost,
                    OpOut::Same(if is_all { NodePtr::ONE } else { NodePtr::NIL }),
                ))
            } else {
                // Normal Case
                let mut cursor = ArgCursor::new(args);
                while let Some(arg) = cursor.next(arena) {
                    cost += BOOL_COST_PER_ARG;
                    check_cost(cost, max_cost)?;
                    is_all = is_all && arena.non_nil(arg);
                }
                Ok((
                    cost,
                    OpOut::Same(if is_all { NodePtr::ONE } else { NodePtr::NIL }),
                ))
            }
        }
        NodeKind::Atom => Ok((
            cost,
            OpOut::Same(if is_all { NodePtr::ONE } else { NodePtr::NIL }),
        )),
    }
}

pub fn op_softfork<D: Dialect>(
    arena: &Arena,
    args: NodePtr,
    max_cost: u64,
    dialect: &D,
) -> Result<(u64, OpOut), ClvmError> {
    match arena.next(args) {
        Some((first, _rest)) => {
            let cost_bytes = int_atom(arena, first, "softfork")?;
            // Soft fork 9 (CANONICAL_INTS): the cost argument must be canonically encoded —
            // at most one leading 0x00, and only when the next byte's high bit is set. Below
            // `soft_fork9_height` the flag is unset and any leading zeros are accepted.
            if (dialect.flags() & CANONICAL_INTS) != 0 {
                let buf = cost_bytes.as_ref();
                if !buf.is_empty() && buf[0] == 0 && (buf.len() < 2 || (buf[1] & 0x80) == 0) {
                    return Err(ClvmError::Unsupported(
                        "softfork requires cost with no leading zeros".to_string(),
                    ));
                }
            }
            let n: BigInt = number_from_slice(cost_bytes.as_ref());
            if n.sign() == Sign::Plus {
                if n > BigInt::from(max_cost) {
                    return Err(ClvmError::Unsupported(format!(
                        "Max Cost({max_cost}) Exceded: {n}"
                    )));
                }
                let cost: u64 = TryFrom::try_from(&n).map_err(|e| {
                    ClvmError::Unsupported(format!("Failed to convert Atom to Int: {e:?}"))
                })?;
                Ok((cost, OpOut::Same(NodePtr::NIL)))
            } else {
                Err(ClvmError::Unsupported(format!(
                    "Cost must be > 0, found {n}"
                )))
            }
        }
        None => Err(ClvmError::Unsupported(
            "Softfork takes at least 1 argument".to_string(),
        )),
    }
}

#[cfg(feature = "bls")]
static GROUP_ORDER: Lazy<BigInt> = Lazy::new(|| {
    let order_as_bytes = &[
        0x73, 0xed, 0xa7, 0x53, 0x29, 0x9d, 0x7d, 0x48, 0x33, 0x39, 0xd8, 0x08, 0x09, 0xa1, 0xd8,
        0x05, 0x53, 0xbd, 0xa4, 0x02, 0xff, 0xfe, 0x5b, 0xfe, 0xff, 0xff, 0xff, 0xff, 0x00, 0x00,
        0x00, 0x01,
    ];
    let n = BigUint::from_bytes_be(order_as_bytes);
    n.into()
});

#[cfg(feature = "bls")]
pub(crate) fn mod_group_order(n: &BigInt) -> BigInt {
    let order = GROUP_ORDER.clone();
    let mut remainder = n.mod_floor(&order);
    if remainder.sign() == Sign::Minus {
        remainder += order;
    }
    remainder
}

/// G1 primitives for the two CLVM BLS operators, over raw `blst` FFI.
///
/// `parse_g1_compressed` accepts only canonical compressed encodings:
/// compressed-flag/infinity/sort-bit canonicality, x < p, on-curve, and the G1 subgroup
/// check, with `0xc0 || 0^47` as the only infinity encoding.
#[cfg(feature = "bls")]
pub(crate) mod bls_g1 {
    use blst::{
        BLST_ERROR, blst_p1, blst_p1_add_or_double_affine, blst_p1_affine, blst_p1_affine_in_g1,
        blst_p1_compress, blst_p1_generator, blst_p1_mult, blst_p1_uncompress, blst_scalar,
        blst_scalar_from_be_bytes,
    };
    use std::mem::MaybeUninit;

    /// The point at infinity: `blst` represents it as the all-zero point (Z = 0).
    pub(crate) fn identity() -> blst_p1 {
        blst_p1::default()
    }

    /// `generator * n` for a big-endian, group-order-reduced, non-negative integer.
    /// `n_be` must be non-empty (a reduced zero is `[0]`).
    pub(crate) fn generator_mul_be(n_be: &[u8]) -> blst_p1 {
        debug_assert!(!n_be.is_empty() && n_be.len() <= 32);
        // SAFETY: `blst_scalar_from_be_bytes` fully initializes `scalar` for 1..=32 input
        // bytes; `blst_p1_mult` reads 256 scalar bits (the full blst_scalar width) and
        // fully initializes `point`. All pointers are valid locals.
        unsafe {
            let mut scalar = MaybeUninit::<blst_scalar>::uninit();
            blst_scalar_from_be_bytes(scalar.as_mut_ptr(), n_be.as_ptr(), n_be.len());
            let mut point = MaybeUninit::<blst_p1>::uninit();
            blst_p1_mult(
                point.as_mut_ptr(),
                blst_p1_generator(),
                scalar.as_ptr().cast::<u8>(),
                256,
            );
            point.assume_init()
        }
    }

    /// Parse a 48-byte compressed G1 element: canonical-infinity guards,
    /// `blst_p1_uncompress`, then `is_inf || in_g1`; `None` for every invalid class.
    pub(crate) fn parse_g1_compressed(bytes: &[u8; 48]) -> Option<blst_p1_affine> {
        let zeros_only = bytes[1..].iter().all(|b| *b == 0);
        if (bytes[0] & 0xc0) == 0xc0 {
            // The only canonical infinity encoding is 0xc0 followed by 47 zero bytes.
            if bytes[0] != 0xc0 || !zeros_only {
                return None;
            }
            // Affine infinity: the all-zero affine point (x = y = 0).
            return Some(blst_p1_affine::default());
        }
        if (bytes[0] & 0xc0) != 0x80 {
            // Compressed flag clear, or infinity flag without the compressed flag.
            return None;
        }
        if zeros_only && (bytes[0] & 0x3f) == 0 {
            // x = 0 without the infinity flag is non-canonical
            return None;
        }
        // SAFETY: `bytes` is a valid 48-byte buffer; on BLST_SUCCESS `affine` is initialized.
        let affine = unsafe {
            let mut affine = MaybeUninit::<blst_p1_affine>::uninit();
            if blst_p1_uncompress(affine.as_mut_ptr(), bytes.as_ptr()) != BLST_ERROR::BLST_SUCCESS {
                return None;
            }
            affine.assume_init()
        };
        // Subgroup check; infinity was handled above.
        // SAFETY: `affine` is initialized and on the curve.
        if unsafe { blst_p1_affine_in_g1(&raw const affine) } {
            Some(affine)
        } else {
            None
        }
    }

    /// `acc += p` on the affine addend (complete formula, handles doubling and either
    /// operand at infinity).
    pub(crate) fn add_assign(acc: &mut blst_p1, p: &blst_p1_affine) {
        // SAFETY: both operands are valid initialized points.
        unsafe {
            blst_p1_add_or_double_affine(&raw mut *acc, &raw const *acc, &raw const *p);
        }
    }

    /// Compressed encoding via `blst_p1_compress`; infinity encodes as `0xc0 || 0^47`.
    pub(crate) fn to_compressed(p: &blst_p1) -> [u8; 48] {
        // SAFETY: `p` is a valid initialized point; `blst_p1_compress` writes all 48 bytes.
        unsafe {
            let mut bytes = MaybeUninit::<[u8; 48]>::uninit();
            blst_p1_compress(bytes.as_mut_ptr().cast::<u8>(), &raw const *p);
            bytes.assume_init()
        }
    }
}

#[cfg(feature = "bls")]
pub fn op_pubkey_for_exp<D: Dialect>(
    arena: &Arena,
    args: NodePtr,
    max_cost: u64,
    _dialect: &D,
) -> Result<(u64, OpOut), ClvmError> {
    check_arg_count(arena, args, 1, "pubkey_for_exp")?;
    let a0 = split(arena, args)?.0;
    let (exp, v0_len) = {
        let v0 = int_atom(arena, a0, "pubkey_for_exp")?;
        (mod_group_order(&number_from_slice(v0.as_ref())), v0.len())
    };
    let cost = PUBKEY_BASE_COST + (v0_len as u64) * PUBKEY_COST_PER_BYTE;
    // The exponent-priced cost is checked against `max_cost` BEFORE the scalar
    // multiplication; the 48-byte malloc surcharge is added to the returned cost but is
    // not part of the in-operator check.
    check_cost(cost, max_cost)?;
    // `exp` is reduced mod the group order, so it is non-negative and its big-endian
    // magnitude (`[0]` for zero) is at most 32 bytes.
    let point = bls_g1::generator_mul_be(&exp.to_bytes_be().1);
    new_atom_and_cost(cost, &bls_g1::to_compressed(&point))
}

#[cfg(not(feature = "bls"))]
pub fn op_pubkey_for_exp<D: Dialect>(
    _arena: &Arena,
    _args: NodePtr,
    _max_cost: u64,
    _dialect: &D,
) -> Result<(u64, OpOut), ClvmError> {
    Err(ClvmError::Unsupported(
        "pubkey_for_exp requires dg_xch_core to be built with the `bls` feature".to_string(),
    ))
}

#[cfg(feature = "bls")]
pub fn op_point_add<D: Dialect>(
    arena: &Arena,
    args: NodePtr,
    max_cost: u64,
    _dialect: &D,
) -> Result<(u64, OpOut), ClvmError> {
    let mut cost = POINT_ADD_BASE_COST;
    let mut total = bls_g1::identity();
    let mut cursor = ArgCursor::new(args);
    while let Some(arg) = cursor.next(arena) {
        // POINT_ADD_COST_PER_ARG is charged and checked against `max_cost` for EVERY
        // argument, before parsing it — a cost-exhausted budget errors even when the
        // pending argument is garbage.
        cost += POINT_ADD_COST_PER_ARG;
        check_cost(cost, max_cost)?;
        let blob = atom(arena, arg, "point_add")?;
        let blob_bytes = blob.as_ref();
        if blob_bytes.len() == 48 {
            let mut as_array: [u8; 48] = [0; 48];
            as_array.clone_from_slice(&blob_bytes[0..48]);
            // any invalid 48-byte encoding (non-canonical infinity, x = 0 without the
            // infinity flag, x >= p, off-curve, wrong subgroup) errors the whole
            // operator — no silent skip
            if let Some(point) = bls_g1::parse_g1_compressed(&as_array) {
                bls_g1::add_assign(&mut total, &point);
            } else {
                let blob_hex: String = hex::encode(blob_bytes);
                Err(ClvmError::InvalidInput(format!(
                    "point_add atom is not a G1 point: {blob_hex}"
                )))?;
            }
        } else {
            let blob_hex: String = hex::encode(blob_bytes);
            let msg = format!(
                "point_add expects blob, got {blob_hex}: Length of bytes object not equal to G1Element::SIZE"
            );
            Err(ClvmError::Unsupported(format!(
                "{msg} {}",
                arena.debug_fmt(args)
            )))?;
        }
    }
    new_atom_and_cost(cost, &bls_g1::to_compressed(&total))
}

#[cfg(not(feature = "bls"))]
pub fn op_point_add<D: Dialect>(
    _arena: &Arena,
    _args: NodePtr,
    _max_cost: u64,
    _dialect: &D,
) -> Result<(u64, OpOut), ClvmError> {
    Err(ClvmError::Unsupported(
        "point_add requires dg_xch_core to be built with the `bls` feature".to_string(),
    ))
}

pub fn op_coinid<D: Dialect>(
    arena: &Arena,
    args: NodePtr,
    _max_cost: u64,
    _dialect: &D,
) -> Result<(u64, OpOut), ClvmError> {
    let mut args_list = arena.as_atom_list(args);
    if args_list.len() != 3 {
        Err(ClvmError::InvalidArgCount(format!(
            "coinid expects 3 args, got {} {}",
            args_list.len(),
            arena.debug_fmt(args)
        )))?;
    }
    let amount = args_list.pop().expect("Length Already Checked");
    let puzzle_hash = args_list.pop().expect("Length Already Checked");
    let parent_coin_info = args_list.pop().expect("Length Already Checked");
    if parent_coin_info.len() != 32 {
        Err(ClvmError::InvalidInput(format!(
            "invalid parent coin id, {}",
            hex::encode(&parent_coin_info)
        )))?;
    }
    if puzzle_hash.len() != 32 {
        Err(ClvmError::InvalidInput(format!(
            "invalid puzzle hash, {}",
            hex::encode(&puzzle_hash)
        )))?;
    }
    let as_int = if !amount.is_empty() {
        let as_int = number_from_slice(&amount);
        if as_int.is_negative() {
            Err(ClvmError::InvalidInput(format!(
                "coin amount cannot be negative, {}",
                number_from_slice(&amount)
            )))?;
        }
        if amount.len() > 9 || (amount.len() == 9 && amount[0] != 0) {
            Err(ClvmError::InvalidInput(format!(
                "coin amount exceeds max, {as_int}"
            )))?;
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
    // The 32-byte coin id is a freshly allocated atom, so — like every other hashing op —
    // it carries `len * MALLOC_COST_PER_BYTE` (32 * 10 = 320) on top of the base COIN_ID_COST.
    let coin_id = coin.coin_id();
    new_atom_and_cost(COIN_ID_COST, coin_id.as_ref())
}
#[cfg(test)]
#[path = "../../tests/unit/clvm/more_ops/tests.rs"]
mod tests;
