use num_bigint::BigInt;
use num_traits::{One, Zero};
use crate::bls12381::fields::{FieldExtBase, OneQ, ZeroQ};
use crate::bls12381::fields::fq::Fq;
use crate::bls12381::fields::fq2::Fq2;
use crate::bls12381::fields::fq6::Fq6;

pub type Fq12 = FieldExtBase<Fq6>;
impl Fq12 {
    pub fn new(q: &'static BigInt, args: &[Fq6]) -> Self {
        let extension: usize = 6;
        let embedding: usize = 3;
        FieldExtBase {
            root: Fq6::new(&q, &[Fq2::zero(&q), Fq2::one(&q), Fq2::zero(&q)]),
            q,
            extension,
            embedding,
            fields: args.to_vec(),
        }
    }
}
impl ZeroQ for Fq12 {
    fn zero(q: &'static BigInt) -> Self {
        Fq12::from(Fq::new(q, BigInt::zero()))
    }
}
impl OneQ for Fq12 {
    fn one(q: &'static BigInt) -> Self {
        Fq12::from(Fq::new(q, BigInt::one()))
    }
}
impl From<Fq> for Fq12 {
    fn from(value: Fq) -> Self {
        let q_ref = value.q;
        Fq12::new(q_ref, &[value.into(), Fq6::zero(q_ref), Fq6::zero(q_ref)])
    }
}