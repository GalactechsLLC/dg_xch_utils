pub mod fq;
pub mod fq2;
pub mod fq6;
pub mod fq12;

use num_bigint::BigInt;
use num_traits::Zero;
use std::ops::{Add, Mul, Neg, Sub};

pub trait OneQ {
    fn one(q: &'static BigInt) -> Self;
}
pub trait ZeroQ {
    fn zero(q: &'static BigInt) -> Self;
}

#[derive(Clone)]
pub struct FieldExtBase<F: Clone> {
    root: F,
    q: &'static BigInt,
    extension: usize,
    embedding: usize,
    fields: Vec<F>,
}
impl<F: Clone> FieldExtBase<F> {}
impl<F: Neg<Output = F> + Clone> Neg for FieldExtBase<F> {
    type Output = Self;

    fn neg(mut self) -> Self::Output {
        self.fields = self.fields.into_iter().map(|f| f.neg()).collect();
        self
    }
}
impl<F: Add<Output = F> + Clone> Add for FieldExtBase<F> {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        FieldExtBase {
            root: self.root,
            q: self.q,
            extension: self.extension,
            embedding: self.embedding,
            fields: self
                .fields
                .into_iter()
                .zip(rhs.fields)
                .map(|(a, b)| a + b)
                .collect(),
        }
    }
}
impl<F: Add<Output = F> + Neg<Output = F> + Clone> Sub for FieldExtBase<F> {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self::Output {
        self + rhs.neg()
    }
}
impl<F: Mul<Output = F> + Clone + Zero> Mul for FieldExtBase<F> {
    type Output = Self;

    fn mul(self, rhs: Self) -> Self::Output {
        let mut buf = vec![F::zero(); self.embedding];
        for (i, x) in self.fields.iter().enumerate() {
            for (j, y) in rhs.fields.iter().enumerate() {
                buf[(i + j) % self.embedding] =
                    buf[(i + j) % self.embedding].clone() + (x.clone() * y.clone());
            }
        }
        FieldExtBase {
            root: self.root,
            q: self.q,
            extension: self.extension,
            embedding: self.embedding,
            fields: buf,
        }
    }
}