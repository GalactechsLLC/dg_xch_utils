use super::*;

fn plot_id() -> Bytes32 {
    let mut bytes = [0u8; 32];
    for (i, b) in bytes.iter_mut().enumerate() {
        *b = (i * 11 + 5) as u8;
    }
    Bytes32::from(bytes)
}

/// `(k, input, ciphertext)` for the plot id above.
const FEISTEL_VECTORS: &[(u32, u64, u64)] = &[
    (28, 0, 680_663_352_959_931),
    (28, 1, 53_950_537_582_426_653),
    (28, 1_250_999_896_491, 63_157_282_695_857_520),
    (28, 281_474_976_710_655, 26_361_481_967_541_154),
    (28, 12_345_678_901, 29_541_298_939_039_514),
    (30, 0, 1_064_829_078_371_294_044),
    (30, 1, 75_867_245_716_127_150),
    (30, 1_250_999_896_491, 395_489_400_676_364_053),
    (30, 281_474_976_710_655, 16_514_633_143_281_432),
    (30, 12_345_678_901, 257_764_385_613_865_671),
    (32, 0, 10_416_083_265_746_737_936),
    (32, 1, 10_621_545_006_909_718_083),
    (32, 1_250_999_896_491, 9_532_783_856_750_979_935),
    (32, 281_474_976_710_655, 13_712_526_740_761_935_299),
    (32, 12_345_678_901, 10_459_832_581_459_965_203),
];

#[test]
fn encryption_matches_the_reference_vectors() {
    for (i, (k, input, expected)) in FEISTEL_VECTORS.iter().enumerate() {
        let cipher = FeistelCipher::new(plot_id(), *k).expect("valid k");
        let block = if 2 * k >= 64 {
            *input
        } else {
            *input & ((1u64 << (2 * k)) - 1)
        };
        assert_eq!(
            cipher.encrypt(block),
            *expected,
            "vector {i} (k{k}) diverged"
        );
    }
}

#[test]
fn decryption_inverts_encryption() {
    for k in [28u32, 30, 32] {
        let cipher = FeistelCipher::new(plot_id(), k).expect("valid k");
        let block_mask = if 2 * k >= 64 {
            u64::MAX
        } else {
            (1u64 << (2 * k)) - 1
        };
        for v in [0u64, 1, 42, 1_250_999_896_491, 0xDEAD_BEEF_CAFE] {
            let block = v & block_mask;
            assert_eq!(cipher.decrypt(cipher.encrypt(block)), block, "k{k} v{v}");
        }
    }
}

#[test]
fn a_different_plot_id_gives_a_different_ciphertext() {
    let a = FeistelCipher::new(plot_id(), 28).expect("valid k");
    let b = FeistelCipher::new(Bytes32::from([9u8; 32]), 28).expect("valid k");
    assert_ne!(a.encrypt(12345), b.encrypt(12345));
}

#[test]
fn oversized_k_is_refused() {
    assert!(FeistelCipher::new(plot_id(), 33).is_err());
}
