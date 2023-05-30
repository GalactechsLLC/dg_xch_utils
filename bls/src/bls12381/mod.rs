use num_bigint::BigInt;
use num_traits::{Num, Zero};
use once_cell::sync::Lazy;
use crate::bls12381::fields::fq2::Fq2;
use crate::bls12381::fields::fq::Fq;
use crate::bls12381::fields::ZeroQ;

mod elliptic_curve;
mod fields;
mod hkdf;

pub enum Either<L, R> {
    Left(L),
    Right(R),
}

// BLS parameter used to generate the other parameters
// Spec is found here: https://github.com/zkcrypto/pairing/tree/master/src/bls12_381
static X: Lazy<BigInt> = Lazy::new(|| BigInt::from_str_radix("-0xD201000000010000", 16).unwrap());

// 381 bit prime
// Also see fields:bls12381_q
static Q: Lazy<BigInt> = Lazy::new(|| {
    BigInt::from_str_radix("0x1A0111EA397FE69A4B1BA7B6434BACD764774B84F38512BF6730D2A0F6B0F6241EABFFFEB153FFFFB9FEFFFFFFFFAAAB", 16).unwrap()
});

// a,b and a2, b2, define the elliptic curve and twisted curve.
// y^2 = x^3 + 4
// y^2 = x^3 + 4(u + 1)

static A: Lazy<Fq> = Lazy::new(|| Fq::new(&Q, BigInt::zero()));
static B: Lazy<Fq> = Lazy::new(|| Fq::new(&Q, BigInt::from(4)));
static A_TWIST: Lazy<Fq2> = Lazy::new(|| Fq2::new(&Q, &[Fq::zero(&Q), Fq::zero(&Q)]));
static B_TWIST: Lazy<Fq2> = Lazy::new(|| {
    Fq2::new(
        &Q,
        &[Fq::new(&Q, BigInt::from(4)), Fq::new(&Q, BigInt::from(4))],
    )
});

// The generators for g1 and g2
static GX: Lazy<Fq> = Lazy::new(|| {
    Fq::new(&Q, BigInt::from_str_radix("0x17F1D3A73197D7942695638C4FA9AC0FC3688C4F9774B905A14E3A3F171BAC586C55E83FF97A1AEFFB3AF00ADB22C6BB", 16).unwrap())
});
static GY: Lazy<Fq> = Lazy::new(|| {
    Fq::new(&Q, BigInt::from_str_radix("0x08B3F481E3AAA0F1A09E30ED741D8AE4FCF5E095D5D00AF600DB18CB2C04B3EDD03CC744A2888AE40CAA232946C5E7E1", 16).unwrap())
});
static G2X: Lazy<Fq2> = Lazy::new(|| {
    Fq2::new(
        &Q,
        &[Fq::new(&Q, BigInt::from_str_radix("352701069587466618187139116011060144890029952792775240219908644239793785735715026873347600343865175952761926303160", 10).unwrap()),
                      Fq::new(&Q, BigInt::from_str_radix("3059144344244213709971259814753781636986470325476647558659373206291635324768958432433509563104347017837885763365758", 10).unwrap())]
    )
});
static G2Y: Lazy<Fq2> = Lazy::new(|| {
    Fq2::new(
        &Q,
        &[Fq::new(&Q,BigInt::from_str_radix("1985150602287291935568054521177171638300868978215655730859378665066344726373823718423869104263333984641494340347905", 10).unwrap()),
                      Fq::new(&Q, BigInt::from_str_radix("927553665492332455747201965776037880757740193453592970025027978793976877002675564980949289727957565575433344219582", 10).unwrap())]
    )
});

// The order of all three groups (g1, g2, and gt). Note, the elliptic curve E_twist
// actually has more valid points than this. This is relevant when hashing onto the
// curve, where we use a point that is not in g2, and map it into g2.
static N: Lazy<BigInt> = Lazy::new(|| {
    BigInt::from_str_radix(
        "0x73EDA753299D7D483339D80809A1D80553BDA402FFFE5BFEFFFFFFFF00000001",
        16,
    )
    .unwrap()
});

// Cofactor used to generate r torsion points
static H: Lazy<BigInt> =
    Lazy::new(|| BigInt::from_str_radix("0x396C8C005555E1568C00AAAB0000AAAB", 16).unwrap());

// https://tools.ietf.org/html/draft-irtf-cfrg-hash-to-curve-07#section-8.8.2
static H_EFF: Lazy<BigInt> = Lazy::new(|| {
    BigInt::from_str_radix("0xBC69F08F2EE75B3584C6A0EA91B352888E2A8E9145AD7689986FF031508FFE1329C2F178731DB956D82BF015D1212B02EC0EC69D7477C1AE954CBC06689F6A359894C0ADEBBF6B4E8020005AAA95551", 16).unwrap()
});

// Embedding degree
static K: Lazy<BigInt> = Lazy::new(|| BigInt::from(12));

// sqrt(-3) mod q
static SQRT_N3: Lazy<BigInt> = Lazy::new(|| {
    BigInt::from_str_radix("1586958781458431025242759403266842894121773480562120986020912974854563298150952611241517463240701", 10).unwrap()
});

// (sqrt(-3) - 1) / 2 mod q
static SQRT_N3M1O2: Lazy<BigInt> = Lazy::new(|| {
    BigInt::from_str_radix("793479390729215512621379701633421447060886740281060493010456487427281649075476305620758731620350", 10).unwrap()
});
