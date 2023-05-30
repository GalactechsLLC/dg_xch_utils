use num_bigint::BigInt;
use num_traits::{One, Zero};
use crate::bls12381::fields::{FieldExtBase, OneQ, ZeroQ};
use crate::bls12381::fields::fq::Fq;
use crate::bls12381::fields::fq2::Fq2;

pub type Fq6 = FieldExtBase<Fq2>;
impl Fq6 {
    pub fn new(q: &'static BigInt, args: &[Fq2; 3]) -> Self {
        let extension: usize = 6;
        let embedding: usize = 3;
        FieldExtBase {
            root: Fq2::new(&q, &[Fq::one(q), Fq::one(q)]),
            q,
            extension,
            embedding,
            fields: args.to_vec(),
        }
    }
}
impl ZeroQ for Fq6 {
    fn zero(q: &'static BigInt) -> Self {
        Fq6::from(Fq::new(q, BigInt::zero()))
    }
}
impl OneQ for Fq6 {
    fn one(q: &'static BigInt) -> Self {
        Fq6::from(Fq::new(q, BigInt::one()))
    }
}
impl From<Fq> for Fq6 {
    fn from(value: Fq) -> Self {
        let q_ref = value.q;
        Fq6::new(q_ref, &[value.into(), Fq2::zero(q_ref), Fq2::zero(q_ref)])
    }
}