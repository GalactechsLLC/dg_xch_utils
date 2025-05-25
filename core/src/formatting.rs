use hex::{decode, FromHexError};
use num_bigint::BigInt;
use num_traits::{pow, Signed};
use once_cell::sync::Lazy;
use std::io::{Error, ErrorKind};

pub static BIG_ZERO: Lazy<BigInt> = Lazy::new(|| BigInt::from(0));
pub static BIG_ONE: Lazy<BigInt> = Lazy::new(|| BigInt::from(1));
pub static BIG_TWO: Lazy<BigInt> = Lazy::new(|| BigInt::from(2));

#[must_use]
pub fn prep_hex_str<S: AsRef<str>>(to_fix: S) -> String {
    let lc = to_fix.as_ref().to_lowercase();
    if let Some(s) = lc.strip_prefix("0x") {
        s.to_string()
    } else {
        lc
    }
}

pub fn hex_to_bytes<S: AsRef<str>>(hex: S) -> Result<Vec<u8>, FromHexError> {
    decode(prep_hex_str(hex))
}

#[must_use]
pub fn number_from_slice(v: &[u8]) -> BigInt {
    if v.is_empty() {
        0.into()
    } else {
        BigInt::from_signed_bytes_be(v)
    }
}

#[must_use]
pub fn u64_to_bytes(v: u64) -> Vec<u8> {
    let mut rtn = Vec::new();
    if v.leading_zeros() == 0 {
        rtn.push(u8::MIN);
        let ary = v.to_be_bytes();
        rtn.extend_from_slice(&ary);
        rtn
    } else {
        let mut trim: bool = true;
        for b in v.to_be_bytes() {
            if trim {
                if b == u8::MIN {
                    continue;
                }
                rtn.push(b);
                trim = false;
            } else {
                rtn.push(b);
            }
        }
        rtn
    }
}

#[allow(clippy::cast_possible_truncation)]
pub fn bigint_to_bytes(v_: &BigInt, signed: bool) -> Result<Vec<u8>, Error> {
    let v = v_.clone();
    if v == *BIG_ZERO {
        return Ok(vec![]);
    }
    if !signed && v.is_negative() {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "OverflowError: can't convert negative int to unsigned".to_string(),
        ));
    }
    let mut byte_count = 1;
    let mut dec = 0;
    let div = if signed {
        BIG_ONE.clone()
    } else {
        BIG_ZERO.clone()
    };
    let b_pow = BigInt::from(1_u64 << 32); // b32.to_bigint().unwrap();
    if v.is_negative() {
        let mut right_hand = (-(v.clone()) + BIG_ONE.clone()) * (div + BIG_ONE.clone());
        while pow(b_pow.clone(), (byte_count - 1) / 4 + 1) < right_hand {
            byte_count += 4;
        }
        right_hand = -(v.clone()) * BIG_TWO.clone();
        while pow(BIG_TWO.clone(), 8 * byte_count) < right_hand {
            byte_count += 1;
        }
    } else {
        let mut right_hand = (v.clone() + BIG_ONE.clone()) * (div.clone() + BIG_ONE.clone());
        while pow(b_pow.clone(), (byte_count - 1) / 4 + 1) < right_hand {
            byte_count += 4;
        }
        right_hand = (v.clone() + BIG_ONE.clone()) * (div + BIG_ONE.clone());
        while pow(BigInt::from(2_u32), 8 * byte_count) < right_hand {
            byte_count += 1;
        }
    }
    let extra_byte = usize::from(
        signed
            && v > *BIG_ZERO
            && ((v.clone() >> ((byte_count - 1) * 8)) & BigInt::from(0x80_u32)) > *BIG_ZERO,
    );
    let total_bytes = byte_count + extra_byte;
    let mut dv = Vec::<u8>::with_capacity(total_bytes);
    let byte4_remain = byte_count % 4;
    let byte4_length = (byte_count - byte4_remain) / 4;
    dv.resize(total_bytes, 0);
    let (_sign, u32_digits) = v.to_u32_digits();
    for (i, n) in u32_digits.iter().take(byte4_length).enumerate() {
        let word_idx = byte4_length - i - 1;
        let num = u64::from(*n);
        let pointer = extra_byte + byte4_remain + word_idx * 4;
        let setval = if v.is_negative() {
            (1_u64 << 32) - num - u64::from(dec)
        } else {
            num
        };
        dec = 1;
        dv[pointer] = ((setval >> 24) & 0xff) as u8;
        dv[pointer + 1] = ((setval >> 16) & 0xff) as u8;
        dv[pointer + 2] = ((setval >> 8) & 0xff) as u8;
        dv[pointer + 3] = (setval & 0xff) as u8;
    }

    let last_bytes = u32_digits[u32_digits.len() - 1];
    let transform = |idx| {
        if v.is_negative() {
            (((1 << (8 * byte4_remain)) - last_bytes - dec) >> (8 * idx)) as u8
        } else {
            (last_bytes >> (8 * idx)) as u8
        }
    };

    for i in 0..byte4_remain {
        dv[extra_byte + i] = transform(byte4_remain - i - 1);
    }

    Ok(dv)
}

pub fn u64_from_bigint(item: &BigInt) -> Result<u64, Error> {
    if item.is_negative() {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "cannot convert negative integer to u64",
        ));
    }
    if *item > u64::MAX.into() {
        return Err(Error::new(ErrorKind::InvalidData, "u64::MAX exceeded"));
    }
    let bytes: Vec<u8> = item.to_signed_bytes_be();
    let mut slice = bytes.as_slice();
    // make number minimal by removing leading zeros
    while (!slice.is_empty()) && (slice[0] == 0) {
        if slice.len() > 1 && (slice[1] & 0x80 == 0x80) {
            break;
        }
        slice = &slice[1..];
    }
    let mut fixed_ary = [0u8; 8];
    let start = size_of::<u64>() - slice.len();
    fixed_ary[start..size_of::<u64>()].copy_from_slice(&slice[..(size_of::<u64>() - start)]);
    Ok(u64::from_be_bytes(fixed_ary))
}

fn u32_from_slice_impl(buf: &[u8], signed: bool) -> Option<u32> {
    if buf.is_empty() {
        return Some(0);
    }
    // too many bytes for u32
    if buf.len() > 4 {
        return None;
    }
    let sign_extend = (buf[0] & 0x80) != 0;
    let mut ret: u32 = if signed && sign_extend {
        0xffff_ffff
    } else {
        0
    };
    for b in buf {
        ret <<= 8;
        ret |= u32::from(*b);
    }
    Some(ret)
}

#[must_use]
pub fn u32_from_slice(buf: &[u8]) -> Option<u32> {
    u32_from_slice_impl(buf, false)
}

#[allow(clippy::cast_possible_wrap)]
#[must_use]
pub fn i32_from_slice(buf: &[u8]) -> Option<i32> {
    u32_from_slice_impl(buf, true).map(|v| v as i32)
}
