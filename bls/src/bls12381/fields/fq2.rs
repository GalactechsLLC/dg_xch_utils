use num_bigint::BigInt;
use num_traits::{One, Zero};
use std::ops::Neg;
use crate::bls12381::fields::{FieldExtBase, OneQ, ZeroQ};
use crate::bls12381::fields::fq::Fq;

pub type Fq2 = FieldExtBase<Fq>;
impl Fq2 {
    pub fn new(q: &'static BigInt, args: &[Fq; 2]) -> Self {
        let extension: usize = 2;
        let embedding: usize = 2;
        FieldExtBase {
            root: Fq::new(&q, BigInt::one().neg()),
            q,
            extension,
            embedding,
            fields: args.to_vec(),
        }
    }
}
impl ZeroQ for Fq2 {
    fn zero(q: &'static BigInt) -> Self {
        Fq2::from(Fq::new(q, BigInt::zero()))
    }
}
impl OneQ for Fq2 {
    fn one(q: &'static BigInt) -> Self {
        Fq2::from(Fq::new(q, BigInt::one()))
    }
}
impl From<Fq> for Fq2 {
    fn from(value: Fq) -> Self {
        let q_ref = value.q;
        Fq2::new(q_ref, &[value, Fq::zero(q_ref)])
    }
}