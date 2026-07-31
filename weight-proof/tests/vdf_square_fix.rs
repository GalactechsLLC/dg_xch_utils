// Regression tests for class-group form squaring.

use dg_xch_vdf::form::Form;
use num_bigint::BigInt;

fn f(a: i64, b: i64, c: i64) -> Form {
    Form {
        a: BigInt::from(a),
        b: BigInt::from(b),
        c: BigInt::from(c),
    }
}

fn assert_square(input: (i64, i64, i64), expected: (i64, i64, i64)) {
    let got = f(input.0, input.1, input.2).square().expect("square ok");
    let (ea, eb, ec) = expected;
    assert_eq!(
        (got.a.clone(), got.b.clone(), got.c.clone()),
        (BigInt::from(ea), BigInt::from(eb), BigInt::from(ec)),
        "square({input:?}) reference={expected:?} got=({},{},{})",
        got.a,
        got.b,
        got.c
    );
}

/// The bug class: primitive forms with gcd(a,b) > 1. The old direct formula was wrong here; the general
/// composition must match the reference.
#[test]
fn square_correct_for_gcd_ab_greater_than_one() {
    assert_square((4, 2, 1), (1, 0, 3)); // gcd 2, D=-12 (the port's unit test, independently reproduced)
    assert_square((2, 2, 3), (1, 0, 5)); // gcd 2, D=-20
    assert_square((4, 4, 5), (1, 0, 16)); // gcd 4, D=-64
    assert_square((6, 6, 5), (1, 0, 21)); // gcd 6, D=-84
}

/// Regression: gcd(a,b) = 1 forms (the case the old formula already handled) must be unchanged/correct —
/// this is the "196 previously-passing vectors don't regress" property at the primitive level.
#[test]
fn square_correct_for_gcd_ab_equal_one_no_regression() {
    assert_square((2, 1, 3), (2, -1, 3)); // D=-23, the cyclic generator
    assert_square((3, 1, 2), (2, 1, 3)); // D=-23
    assert_square((5, 3, 2), (2, -1, 4)); // D=-31
    assert_square((7, 5, 3), (3, -1, 5)); // D=-59
}

/// Group-law sanity independent of the reference: cubing the D=-23 generator returns the identity
/// (class number 3), exercising square + a compose on distinct forms.
#[test]
fn cyclic_group_order_three_identity() {
    let g = f(2, 1, 3);
    let g2 = g.square().unwrap();
    let g3 = g2.multiply(&g).unwrap();
    assert_eq!(
        (g3.a.clone(), g3.b.clone(), g3.c.clone()),
        (BigInt::from(1), BigInt::from(1), BigInt::from(6)),
        "g^3 must be the identity (1,1,6) of D=-23"
    );
}
