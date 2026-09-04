//! Behavioral tests for the arithmetic / hashing / bitwise / string
//! operators. Operands are quoted; results follow canonical CLVM operator semantics.
use crate::clvm::program::Program;
use crate::clvm::sexp::SExp;
use crate::clvm::utils::INFINITE_COST;
use crate::errors::ClvmError;
use num_bigint::BigInt;

#[test]
fn op_sha256_matches_the_reference_digest() {
    use sha2::Digest;
    let mut arena = crate::clvm::arena::Arena::new();
    for blobs in [
        vec![b"".to_vec()],
        vec![b"chia".to_vec()],
        vec![vec![0xAB; 31], vec![0xCD; 64]],
        vec![vec![0x01; 1_000_000]],
    ] {
        let mut reference = sha2::Sha256::new();
        for b in &blobs {
            reference.update(b);
        }
        let mut args = arena.new_atom(&[]).expect("nil");
        for b in blobs.iter().rev() {
            let item = arena.new_atom(b).expect("atom");
            args = arena.new_pair(item, args).expect("pair");
        }
        let (cost, out) = super::op_sha256(
            &arena,
            args,
            u64::MAX,
            &crate::clvm::dialect::ChiaDialect::new(0),
        )
        .expect("op runs");
        let (_, node) = out.materialize(&mut arena, cost).expect("materialize");
        let got = arena.atom(node).expect("digest atom");
        assert_eq!(got.as_ref(), reference.finalize().as_slice());
    }
}

fn run_op(op: u8, args: &[SExp<'static>]) -> Result<SExp<'static>, ClvmError> {
    let mut items = vec![SExp::from(op)];
    for a in args {
        items.push(SExp::from((1_u8, a.clone())));
    }
    let program = Program::new(SExp::from(items));
    program
        .run(INFINITE_COST, 0, &Program::default())
        .map(|(_c, out)| out.sexp().to_owned())
}

fn int(sexp: &SExp) -> BigInt {
    sexp.atom().unwrap().as_int()
}
fn bytes(sexp: &SExp) -> Vec<u8> {
    sexp.atom().unwrap().as_ref().to_vec()
}

#[test]
fn arithmetic_add_sub_mul() {
    assert_eq!(
        int(&run_op(16, &[SExp::from(5), SExp::from(7)]).unwrap()),
        BigInt::from(12)
    );
    assert_eq!(
        int(&run_op(17, &[SExp::from(10), SExp::from(3)]).unwrap()),
        BigInt::from(7)
    );
    assert_eq!(
        int(&run_op(18, &[SExp::from(6), SExp::from(7)]).unwrap()),
        BigInt::from(42)
    );
}

#[test]
fn div_floors_and_divmod_returns_pair() {
    // (/ 13 4) -> 3
    assert_eq!(
        int(&run_op(19, &[SExp::from(13), SExp::from(4)]).unwrap()),
        BigInt::from(3)
    );
    // (divmod 13 4) -> (3 . 1)
    let dm = run_op(20, &[SExp::from(13), SExp::from(4)]).unwrap();
    assert_eq!(int(dm.first().unwrap()), BigInt::from(3));
    assert_eq!(int(dm.rest().unwrap()), BigInt::from(1));
}

// Signed two's-complement atom decode.
#[test]
fn div_negative_divisor_floors_to_minus_two() {
    // (/ 10 -5) -> -2. An unsigned decode would read -5 (0xfb) as 251 -> 10/251 == 0.
    assert_eq!(
        int(&run_op(19, &[SExp::from(10), SExp::from(-5)]).unwrap()),
        BigInt::from(-2)
    );
}

// Signed two's-complement atom decode.
#[test]
fn add_with_negative_operand_is_signed() {
    // (+ -1 1) -> 0. An unsigned decode would read -1 (0xff) as 255 -> 256.
    assert_eq!(
        int(&run_op(16, &[SExp::from(-1), SExp::from(1)]).unwrap()),
        BigInt::from(0)
    );
}

// Signed-boundary coverage. Every case decodes a high-bit atom through the
// arithmetic path; big-endian signed two's-complement decode, minimal signed encode.
#[test]
fn signed_boundary_decode_and_roundtrip() {
    // 0x80 == -128, 0x0080 == +128 ⇒ sum 0 (empty atom).
    let s = run_op(16, &[SExp::from(-128), SExp::from(128)]).unwrap();
    assert_eq!(int(&s), BigInt::from(0));
    assert!(s.nullp());
    // 0xff00 == -256, 0x0100 == +256 ⇒ sum 0 (multi-byte negative decode).
    assert_eq!(
        int(&run_op(16, &[SExp::from(-256), SExp::from(256)]).unwrap()),
        BigInt::from(0)
    );
    // (- 0 1) == -1, whose minimal signed encoding is the atom 0xff.
    let neg_one = run_op(17, &[SExp::from(0), SExp::from(1)]).unwrap();
    assert_eq!(int(&neg_one), BigInt::from(-1));
    assert_eq!(bytes(&neg_one), vec![0xff]);
    // (+ -1 0) round-trips -1 back to atom 0xff.
    assert_eq!(
        bytes(&run_op(16, &[SExp::from(-1), SExp::from(0)]).unwrap()),
        vec![0xff]
    );
    // 0 encodes as the empty atom.
    assert!(run_op(17, &[SExp::from(5), SExp::from(5)]).unwrap().nullp());
    // (* -1 -1) == 1: two high-bit atoms multiply to a positive.
    assert_eq!(
        int(&run_op(18, &[SExp::from(-1), SExp::from(-1)]).unwrap()),
        BigInt::from(1)
    );
}

#[test]
fn div_negative_dividend_floors_toward_neg_infinity() {
    // (/ -7 2) == -4 (floors toward -inf), atom 0xfc.
    let q = run_op(19, &[SExp::from(-7), SExp::from(2)]).unwrap();
    assert_eq!(int(&q), BigInt::from(-4));
    assert_eq!(bytes(&q), vec![0xfc]);
}

#[test]
fn greater_than_with_negative_operand_is_signed() {
    // (> -1 1) -> () : -1 (0xff) must decode as negative, not 255.
    assert!(
        run_op(21, &[SExp::from(-1), SExp::from(1)])
            .unwrap()
            .nullp()
    );
    // (> 1 -1) -> 1.
    assert_eq!(
        int(&run_op(21, &[SExp::from(1), SExp::from(-1)]).unwrap()),
        BigInt::from(1)
    );
}

#[test]
fn div_by_zero_errors() {
    let err = run_op(19, &[SExp::from(5), SExp::from(0)]).unwrap_err();
    assert!(matches!(err, ClvmError::Unsupported(_)), "got {err:?}");
}

#[test]
fn greater_than_numeric_and_bytewise() {
    // (> 5 3) -> 1 ; (> 3 5) -> ()
    assert_eq!(
        int(&run_op(21, &[SExp::from(5), SExp::from(3)]).unwrap()),
        BigInt::from(1)
    );
    assert!(run_op(21, &[SExp::from(3), SExp::from(5)]).unwrap().nullp());
    // (>s 0x02 0x01) -> 1
    assert_eq!(
        int(&run_op(10, &[SExp::from(2), SExp::from(1)]).unwrap()),
        BigInt::from(1)
    );
}

#[test]
fn sha256_of_abc_matches_known_vector() {
    let out = run_op(11, &[SExp::from("abc")]).unwrap();
    assert_eq!(
        hex::encode(bytes(&out)),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[test]
fn strlen_substr_concat() {
    assert_eq!(
        int(&run_op(13, &[SExp::from("hello")]).unwrap()),
        BigInt::from(5)
    );
    // (substr "hello" 1 3) -> "el"
    let sub = run_op(12, &[SExp::from("hello"), SExp::from(1), SExp::from(3)]).unwrap();
    assert_eq!(bytes(&sub), b"el");
    // (concat "foo" "bar") -> "foobar"
    let cat = run_op(14, &[SExp::from("foo"), SExp::from("bar")]).unwrap();
    assert_eq!(bytes(&cat), b"foobar");
}

#[test]
fn shifts_ash_and_lsh() {
    // (ash 1 4) -> 16
    assert_eq!(
        int(&run_op(22, &[SExp::from(1), SExp::from(4)]).unwrap()),
        BigInt::from(16)
    );
    // (lsh 1 4) -> 16
    assert_eq!(
        int(&run_op(23, &[SExp::from(1), SExp::from(4)]).unwrap()),
        BigInt::from(16)
    );
}

#[test]
fn bitwise_and_or_xor_not() {
    assert_eq!(
        int(&run_op(24, &[SExp::from(15), SExp::from(51)]).unwrap()),
        BigInt::from(3)
    );
    assert_eq!(
        int(&run_op(25, &[SExp::from(15), SExp::from(48)]).unwrap()),
        BigInt::from(63)
    );
    assert_eq!(
        int(&run_op(26, &[SExp::from(15), SExp::from(51)]).unwrap()),
        BigInt::from(60)
    );
    // (lognot 0) -> -1
    assert_eq!(
        int(&run_op(27, &[SExp::from(0)]).unwrap()),
        BigInt::from(-1)
    );
}

#[test]
fn boolean_not_any_all() {
    assert_eq!(
        int(&run_op(32, &[SExp::default()]).unwrap()),
        BigInt::from(1)
    );
    assert!(run_op(32, &[SExp::from(5)]).unwrap().nullp());
    assert_eq!(
        int(&run_op(33, &[SExp::default(), SExp::from(1)]).unwrap()),
        BigInt::from(1)
    );
    assert!(
        run_op(33, &[SExp::default(), SExp::default()])
            .unwrap()
            .nullp()
    );
    assert_eq!(
        int(&run_op(34, &[SExp::from(1), SExp::from(2)]).unwrap()),
        BigInt::from(1)
    );
    assert!(
        run_op(34, &[SExp::from(1), SExp::default()])
            .unwrap()
            .nullp()
    );
}
