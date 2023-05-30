use crate::bls12381::*;
use crate::clvm::utils::hash_256;
use num_bigint::BigInt;
use once_cell::sync::Lazy;
use std::ops::{Add, Mul, Neg, Sub};
use num_traits::One;
use crate::bls12381::fields::fq12::Fq12;
use crate::bls12381::fields::fq2::Fq2;
use crate::bls12381::fields::fq6::Fq6;
use crate::bls12381::fields::{OneQ, ZeroQ};
use crate::bls12381::fields::fq::Fq;

pub struct EllipticCurve {
    q: &'static BigInt,
    a: Either<&'static Fq, &'static Fq2>,
    b: Either<&'static Fq, &'static Fq2>,
    gx: &'static Fq,
    gy: &'static Fq,
    g2x: &'static Fq2,
    g2y: &'static Fq2,
    n: &'static BigInt,
    h: &'static BigInt,
    x: &'static BigInt,
    k: &'static BigInt,
    sqrt_n3: &'static BigInt,
    sqrt_n3m1o2: &'static BigInt,
}
static DEFAULT_EC: Lazy<EllipticCurve> = Lazy::new(|| EllipticCurve {
    q: &Q,
    a: Either::Left(&A),
    b: Either::Left(&B),
    gx: &GX,
    gy: &GY,
    g2x: &G2X,
    g2y: &G2Y,
    n: &N,
    h: &H,
    x: &X,
    k: &K,
    sqrt_n3: &SQRT_N3,
    sqrt_n3m1o2: &SQRT_N3M1O2,
});
static DEFAULT_EC_TWIST: Lazy<EllipticCurve> = Lazy::new(|| EllipticCurve {
    q: &Q,
    a: Either::Right(&A_TWIST),
    b: Either::Right(&B_TWIST),
    gx: &GX,
    gy: &GY,
    g2x: &G2X,
    g2y: &G2Y,
    n: &N,
    h: &H_EFF,
    x: &X,
    k: &K,
    sqrt_n3: &SQRT_N3,
    sqrt_n3m1o2: &SQRT_N3M1O2,
});

struct AffinePoint<F> {
    x: F,
    y: F,
    infinity: bool,
    ec: &'static EllipticCurve,
}
impl<F> AffinePoint<F>
where
    F: Add<F, Output = F>
        + Add<Fq, Output = F>
        + Add<Fq2, Output = F>
        + Add<Fq6, Output = F>
        + Add<Fq12, Output = F>
        + Mul<F, Output = F>
        + Mul<Fq, Output = F>
        + Mul<Fq2, Output = F>
        + Mul<Fq6, Output = F>
        + Mul<Fq12, Output = F>
        + Sub<F, Output = F>
        + Sub<Fq, Output = F>
        + Sub<Fq2, Output = F>
        + Sub<Fq6, Output = F>
        + Sub<Fq12, Output = F>
        + Neg
        + PartialEq
        + Eq
        + OneQ
        + Clone,
{
    pub fn new(x: F, y: F, infinity: bool, ec: Option<&'static EllipticCurve>) -> Self {
        AffinePoint {
            x,
            y,
            infinity,
            ec: ec.unwrap_or(&DEFAULT_EC),
        }
    }

    pub fn is_on_curve(&self) -> bool {
        if self.infinity {
            true
        } else {
            let left = self.y.clone() * self.y.clone();
            let s1 = self.x.clone() * self.x.clone() * self.x.clone();
            let s2 = match self.ec.a {
                Either::Left(f) => self.x.clone() * f.clone(),
                Either::Right(f) => self.x.clone() * f.clone(),
            };
            let right = match self.ec.b {
                Either::Left(f) => s1 + s2 + f.clone(),
                Either::Right(f) => s1 + s2 + f.clone(),
            };
            left == right
        }
    }
}
impl<F> Add for AffinePoint<F>
where
    F: Add<F, Output = F>
        + Add<Fq, Output = F>
        + Add<Fq2, Output = F>
        + Add<Fq6, Output = F>
        + Add<Fq12, Output = F>
        + Clone,
{
    type Output = F;

    fn add(self, rhs: Self) -> Self::Output {
        todo!()
    }
}
impl<F> Neg for AffinePoint<F>
where
    F: Neg<Output = F> + Clone,
{
    type Output = AffinePoint<F>;

    fn neg(self) -> Self::Output {
        AffinePoint {
            x: self.x,
            y: -self.y,
            infinity: self.infinity,
            ec: self.ec,
        }
    }
}
impl<F> Sub for AffinePoint<F>
where
    F: Add<F, Output = F>
        + Add<Fq, Output = F>
        + Add<Fq2, Output = F>
        + Add<Fq6, Output = F>
        + Add<Fq12, Output = F>
        + Neg<Output = F>
        + Clone,
{
    type Output = F;

    fn sub(self, rhs: Self) -> Self::Output {
        self + rhs.neg()
    }
}
impl<F> PartialEq for AffinePoint<F>
where
    F: PartialEq,
{
    fn eq(&self, other: &Self) -> bool {
        self.x == other.x && self.y == other.y && self.infinity == other.infinity
    }
}
impl<F> Into<Vec<u8>> for AffinePoint<F> {
    fn into(self) -> Vec<u8> {
        todo!()
    }
}
impl<F> From<JacobianPoint<F>> for AffinePoint<F> {
    fn from(value: JacobianPoint<F>) -> Self {
        AffinePoint {
            x: value.x,
            y: value.y,
            infinity: value.infinity,
            ec: value.ec,
        }
    }
}

struct JacobianPoint<F> {
    x: F,
    y: F,
    z: F,
    infinity: bool,
    ec: &'static EllipticCurve,
}
impl<F> JacobianPoint<F>
where
    F: Add<F, Output = F>
        + Add<Fq, Output = F>
        + Add<Fq2, Output = F>
        + Add<Fq6, Output = F>
        + Add<Fq12, Output = F>
        + Add<BigInt, Output = F>
        + Mul<F, Output = F>
        + Mul<Fq, Output = F>
        + Mul<Fq2, Output = F>
        + Mul<Fq6, Output = F>
        + Mul<Fq12, Output = F>
        + Mul<BigInt, Output = F>
        + Sub<F, Output = F>
        + Sub<Fq, Output = F>
        + Sub<Fq2, Output = F>
        + Sub<Fq6, Output = F>
        + Sub<Fq12, Output = F>
        + Sub<BigInt, Output = F>
        + Neg
        + PartialEq
        + Eq
        + Clone,
{
    pub fn new(x: F, y: F, z: F, infinity: bool, ec: Option<&'static EllipticCurve>) -> Self {
        JacobianPoint {
            x,
            y,
            z,
            infinity,
            ec: ec.unwrap_or(&DEFAULT_EC),
        }
    }
    pub fn is_on_curve(&self) -> bool {
        if self.infinity {
            true
        } else {
            let left = self.y.clone() * self.y.clone();
            let s1 = self.x.clone() * self.x.clone() * self.x.clone();
            let s2 = match self.ec.a {
                Either::Left(f) => self.x.clone() * f.clone(),
                Either::Right(f) => self.x.clone() * f.clone(),
            };
            let right = match self.ec.b {
                Either::Left(f) => s1 + s2 + f.clone(),
                Either::Right(f) => s1 + s2 + f.clone(),
            };
            left == right
        }
    }
    pub fn is_valid(&self) -> bool {
        self.is_on_curve() && (self.clone() * self.ec.n == g2infinity(None))
    }
    pub fn get_fingerprint(&self) -> i32 {
        let as_vec: Vec<u8> = (*self.clone()).into();
        let hashed = hash_256(&as_vec);
        let mut i32_buf: [u8; 4] = [0; 4];
        i32_buf.copy_from_slice(&hashed[0..4]);
        i32::from_be_bytes(i32_buf)
    }
}
impl<F> Into<Vec<u8>> for JacobianPoint<F> {
    fn into(self) -> Vec<u8> {
        todo!()
    }
}
impl<F> From<AffinePoint<F>> for JacobianPoint<F>
    where
        F: OneQ,
{
    fn from(value: AffinePoint<F>) -> Self {
        JacobianPoint {
            x: value.x,
            y: value.y,
            z: F::one(value.ec.q),
            infinity: value.infinity,
            ec: value.ec,
        }
    }
}
impl<F> Neg for JacobianPoint<F>
where
    F: Neg<Output = F> + Clone,
{
    type Output = JacobianPoint<F>;

    fn neg(self) -> Self::Output {
        JacobianPoint {
            x: self.x,
            y: -self.y,
            z: -self.z,
            infinity: self.infinity,
            ec: self.ec,
        }
    }
}
impl<F> Add for JacobianPoint<F>
where
    F: Add<F, Output = F>
        + Add<Fq, Output = F>
        + Add<Fq2, Output = F>
        + Add<Fq6, Output = F>
        + Add<Fq12, Output = F>
        + Clone,
{
    type Output = F;

    fn add(self, rhs: Self) -> Self::Output {
        todo!()
    }
}
impl<F> PartialEq for JacobianPoint<F>
where
    F: PartialEq,
{
    fn eq(&self, other: &Self) -> bool {
        self.x == other.x && self.y == other.y && self.infinity == other.infinity
    }
}

pub fn g1generator(ec: Option<EllipticCurve>) -> JacobianPoint<Fq> {
    if let Some(ec) = ec {
        JacobianPoint::from(AffinePoint::new(ec.gx.clone(), ec.gy.clone(), false, Some(&ec)))
    } else {
        JacobianPoint::from(AffinePoint::new(DEFAULT_EC.gx.clone(), DEFAULT_EC.gy.clone(), false, Some(&DEFAULT_EC)))
    }
}

pub fn g2generator(ec: Option<EllipticCurve>) -> JacobianPoint<Fq2> {
    if let Some(ec) = ec {
        JacobianPoint::from(AffinePoint::new(ec.g2x.clone(), ec.g2y.clone(), false, Some(&ec)))
    } else {
        JacobianPoint::from(AffinePoint::new(DEFAULT_EC_TWIST.g2x.clone(), DEFAULT_EC_TWIST.g2y.clone(), false, Some(&DEFAULT_EC_TWIST)))
    }
}

pub fn g1infinity<F: OneQ + ZeroQ>(ec: Option<EllipticCurve>) -> JacobianPoint<F> {
    if let Some(ec) = ec {
        JacobianPoint::new(F::one(ec.q), F::one(ec.q), F::zero(ec.q), true,Some(&ec))
    } else {
        JacobianPoint::new(F::one(ec.q), F::one(ec.q), F::zero(ec.q), true, Some(&DEFAULT_EC))
    }
}

pub fn g2infinity<F: OneQ + ZeroQ>(ec: Option<EllipticCurve>) -> JacobianPoint<F> {
    if let Some(ec) = ec {
        JacobianPoint::new(F::one(ec.q), F::one(ec.q), F::zero(ec.q), true, Some(&ec))
    } else {
        JacobianPoint::new(F::one(&DEFAULT_EC_TWIST.q), F::one(&DEFAULT_EC_TWIST.q), F::zero(&DEFAULT_EC_TWIST.q), true, Some(&DEFAULT_EC_TWIST))
    }
}