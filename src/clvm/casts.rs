use lazy_static::lazy_static;
use num_bigint::BigInt;
use num_traits::{pow, Signed};
use std::io::Error;
use std::io::ErrorKind;

lazy_static! {
    pub static ref BIG_ZERO: BigInt = BigInt::from(0);
    pub static ref BIG_ONE: BigInt = BigInt::from(1);
    pub static ref BIG_TWO: BigInt = BigInt::from(2);
}

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
        let num = *n as u64;
        let pointer = extra_byte + byte4_remain + word_idx * 4;
        let setval = if v.is_negative() {
            (1_u64 << 32) - num - dec as u64
        } else {
            num
        };
        dec = 1;
        set_u32(&mut dv, pointer, setval as u32);
    }

    let lastbytes = u32_digits[u32_digits.len() - 1];
    let transform = |idx| {
        if v.is_negative() {
            (((1 << (8 * byte4_remain)) - lastbytes - dec) >> (8 * idx)) as u8
        } else {
            (lastbytes >> (8 * idx)) as u8
        }
    };

    for i in 0..byte4_remain {
        set_u8(&mut dv, extra_byte + i, transform(byte4_remain - i - 1));
    }

    Ok(dv)
}

pub fn set_u8(vec: &mut [u8], n: usize, v: u8) {
    vec[n] = v;
}

pub fn set_u32(vec: &mut [u8], n: usize, v: u32) {
    vec[n] = ((v >> 24) & 0xff) as u8;
    vec[n + 1] = ((v >> 16) & 0xff) as u8;
    vec[n + 2] = ((v >> 8) & 0xff) as u8;
    vec[n + 3] = (v & 0xff) as u8;
}
