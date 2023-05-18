use crate::bls12381::Q;
use num_bigint::BigInt;
use num_traits::{One, Zero};
use std::ops::{Add, Mul, Neg, Sub};
use crate::bls12381::fields::{OneQ, ZeroQ};
use crate::bls12381::fields::fq2::Fq2;

#[derive(Clone)]
pub struct Fq {
    pub(crate) q: &'static BigInt,
    value: BigInt,
}
impl Fq {
    pub fn new(q: &'static BigInt, value: BigInt) -> Self {
        Fq {
            q,
            value: value % q,
        }
    }
}
impl ZeroQ for Fq {
    fn zero(q: &'static BigInt) -> Self {
        Self {
            q,
            value: BigInt::zero(),
        }
    }
}
impl OneQ for Fq {
    fn one(q: &'static BigInt) -> Self {
        Self {
            q,
            value: BigInt::one(),
        }
    }
}
impl Neg for Fq {
    type Output = Self;

    fn neg(mut self) -> Self::Output {
        self.value = self.value.neg();
        self
    }
}
impl Add for Fq {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        Fq::new(&self.q, self.value + rhs.value)
    }
}
impl Sub for Fq {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self::Output {
        Fq::new(&self.q, self.value - rhs.value)
    }
}
impl Sub<Fq2> for Fq {
    type Output = Self;

    fn sub(self, rhs: Fq2) -> Self::Output {
        Fq::new(&self.q, self.value - rhs.value)
    }
}
impl Mul for Fq {
    type Output = Self;

    fn mul(self, rhs: Self) -> Self::Output {
        Fq::new(&self.q, self.value * rhs.value)
    }
}
impl<'a> Mul<&'a Fq> for &'a Fq {
    type Output = Fq;

    fn mul(self, rhs: Self) -> Self::Output {
        Fq::new(&self.q, &self.value * &rhs.value)
    }
}
impl PartialEq<Self> for Fq {
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value && self.q == other.q
    }
}
impl Eq for Fq {}
impl Default for Fq {
    fn default() -> Self {
        Fq {
            value: BigInt::zero(),
            q: &Q,
        }
    }
}