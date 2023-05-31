use dg_xch_pos::bitvec::BitVec;
use dg_xch_pos::constants::{
    PlotEntry, K_B, K_BATCH_SIZES, K_BC, K_C, K_EXTRA_BITS, K_EXTRA_BITS_POW,
};
use dg_xch_pos::f_calc::F1Calculator;
use dg_xch_pos::f_calc::FXCalculator;
use std::collections::HashMap;

fn check_match(yl: i64, yr: i64) -> bool {
    let bl = yl / K_BC as i64;
    let br = yr / K_BC as i64;
    if bl + 1 != br {
        return false; // Buckets don't match
    }
    for m in 0..K_EXTRA_BITS_POW {
        if (((yr % K_BC as i64) / K_C as i64 - ((yl % K_BC as i64) / K_C as i64)) - m as i64)
            % K_B as i64
            == 0
        {
            let mut c_diff = 2 * m as i64 + bl % 2;
            c_diff *= c_diff;
            if (((yr % K_BC as i64) % K_C as i64 - ((yl % K_BC as i64) % K_C as i64)) - c_diff)
                % K_C as i64
                == 0
            {
                return true;
            }
        }
    }
    return false;
}

//https://github.com/Chia-Network/chiapos/blob/0dd1c1c180d25c351abc48b2f35edf22a0b3dfea/tests/test.cpp#L335
fn verify_fc(t: u8, k: u8, left: u64, right: u64, y1: u64, y: u64, c: Option<u64>) {
    let sizes: [u8; 6] = [1, 2, 4, 4, 3, 2];
    let size = sizes[(t - 2) as usize];
    let calc = FXCalculator::new(k, t);
    let res = calc.calculate_bucket(
        &BitVec::new(y1 as u128, (k + K_EXTRA_BITS) as u32),
        &BitVec::new(left as u128, (k * size) as u32),
        &BitVec::new(right as u128, (k * size) as u32),
    );
    assert_eq!(res.0.get_value().unwrap(), y);
    assert_eq!(res.1.get_value(), c);
}

#[test]
fn test_f1() {
    let mut test_k = 35;
    let test_key: [u8; 32] = [
        0, 2, 3, 4, 5, 5, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 1, 2, 3, 41, 5, 6, 7, 8, 9, 10, 11,
        12, 13, 11, 15, 16,
    ];
    let f1 = F1Calculator::new(test_k, &test_key);
    let mut l = BitVec::new(525, test_k as u32);
    let mut result1 = f1.calculate_bucket(&l).unwrap();
    let mut l2 = BitVec::new(526, test_k as u32);
    let mut result2 = f1.calculate_bucket(&l2).unwrap();
    let mut l3 = BitVec::new(625, test_k as u32);
    let mut result3 = f1.calculate_bucket(&l3).unwrap();
    let mut results = vec![0; 256];
    f1.calculate_buckets(l.get_value().unwrap(), 101, results.as_mut());
    assert_eq!(result1.0.get_value().unwrap(), results[0]);
    assert_eq!(result2.0.get_value().unwrap(), results[1]);
    assert_eq!(result3.0.get_value().unwrap(), results[100]);

    let max_batch: u64 = 1 << K_BATCH_SIZES;
    test_k = 32;
    let f1_2 = F1Calculator::new(test_k, &test_key);
    l = BitVec::new(192837491, test_k as u32);
    result1 = f1_2.calculate_bucket(&l).unwrap();
    l2 = BitVec::new(192837491 + 1, test_k as u32);
    result2 = f1_2.calculate_bucket(&l2).unwrap();
    l3 = BitVec::new(192837491 + 2, test_k as u32);
    result3 = f1_2.calculate_bucket(&l3).unwrap();
    let l4 = BitVec::new(192837491 + max_batch as u128 - 1, test_k as u32);
    let result4 = f1_2.calculate_bucket(&l4).unwrap();

    f1_2.calculate_buckets(l.get_value().unwrap(), max_batch, results.as_mut());
    assert_eq!(result1.0.get_value().unwrap(), results[0]);
    assert_eq!(result2.0.get_value().unwrap(), results[1]);
    assert_eq!(result3.0.get_value().unwrap(), results[2]);
    assert_eq!(
        result4.0.get_value().unwrap(),
        results[(max_batch - 1) as usize]
    );
}

//https://github.com/Chia-Network/chiapos/blob/0dd1c1c180d25c351abc48b2f35edf22a0b3dfea/tests/test.cpp#L390
#[test]
fn test_f2() {
    let test_key_2: [u8; 32] = [
        20, 2, 5, 4, 51, 52, 23, 84, 91, 10, 111, 12, 13, 24, 151, 16, 228, 211, 254, 45, 92, 198,
        204, 10, 9, 10, 11, 129, 139, 171, 15, 18,
    ];
    //map<uint64_t, vector<pair<Bits, Bits>>> buckets;
    let mut buckets = HashMap::new();
    let k = 12u8;
    let num_buckets: u64 = (1u64 << (k + K_EXTRA_BITS)) / K_BC as u64 + 1;
    let mut x: u64 = 0;
    let f1 = F1Calculator::new(k, &test_key_2);
    for _ in 0..(1u64 << (k - 4)) + 1 {
        let mut y: [u64; (1u64 << 4) as usize] = [0; (1u64 << 4) as usize];
        f1.calculate_buckets(x, 1u64 << 4, y.as_mut());
        for i in 0..(1 << 4) {
            let bucket = y[i] / K_BC as u64;
            if !buckets.contains_key(&bucket) {
                buckets.insert(bucket, vec![]);
            }
            buckets.get_mut(&bucket).unwrap().push((
                BitVec::new(y[i] as u128, (k + K_EXTRA_BITS) as u32),
                BitVec::new(x as u128, k as u32),
            ));
            if x + 1 > (1u64 << k) - 1 {
                break;
            }
            x += 1;
        }
        if x + 1 > (1u64 << k) - 1 {
            break;
        }
    }
    let mut f2 = FXCalculator::new(k, 2);
    let mut total_matches = 0;
    for (k, v) in &buckets {
        if *k == num_buckets - 1 {
            continue;
        }
        let mut left_bucket = vec![];
        let mut right_bucket = vec![];
        for yx1 in v {
            left_bucket.push(PlotEntry {
                y: yx1.0.get_value().unwrap(),
                pos: 0,
                offset: 0,
                left_metadata: 0,
                right_metadata: 0,
                used: false,
                read_posoffset: 0,
            });
        }
        for yx2 in buckets.get(&(k + 1)).unwrap() {
            right_bucket.push(PlotEntry {
                y: yx2.0.get_value().unwrap(),
                pos: 0,
                offset: 0,
                left_metadata: 0,
                right_metadata: 0,
                used: false,
                read_posoffset: 0,
            });
        }
        left_bucket.sort_by(|l, r| l.y.cmp(&r.y));
        right_bucket.sort_by(|l, r| l.y.cmp(&r.y));

        let mut idx_l: [u16; 10000] = [0; 10000];
        let mut idx_r: [u16; 10000] = [0; 10000];

        let idx_count = f2.find_matches(
            &left_bucket,
            &right_bucket,
            Some(idx_l.as_mut()),
            Some(idx_r.as_mut()),
        );
        for i in 0..idx_count {
            check_match(
                left_bucket[idx_l[i as usize] as usize].y as i64,
                right_bucket[idx_r[i as usize] as usize].y as i64,
            );
        }
        total_matches += idx_count;
    }
    assert!(total_matches > (1 << k) / 2);
    assert!(total_matches < (1 << k) * 2);
}

//https://github.com/Chia-Network/chiapos/blob/0dd1c1c180d25c351abc48b2f35edf22a0b3dfea/tests/test.cpp#L464
#[test]
fn test_fx() {
    verify_fc(2, 16, 0x44cb, 0x204f, 0x20a61a, 0x2af546, Some(0x44cb204f));
    verify_fc(2, 16, 0x3c5f, 0xfda9, 0x3988ec, 0x15293b, Some(0x3c5ffda9));
    verify_fc(
        3,
        16,
        0x35bf992d,
        0x7ce42c82,
        0x31e541,
        0xf73b3,
        Some(0x35bf992d7ce42c82),
    );
    verify_fc(
        3,
        16,
        0x7204e52d,
        0xf1fd42a2,
        0x28a188,
        0x3fb0b5,
        Some(0x7204e52df1fd42a2),
    );
    verify_fc(
        4,
        16,
        0x5b6e6e307d4bedc,
        0x8a9a021ea648a7dd,
        0x30cb4c,
        0x11ad5,
        Some(0xd4bd0b144fc26138),
    );
    verify_fc(
        4,
        16,
        0xb9d179e06c0fd4f5,
        0xf06d3fef701966a0,
        0x1dd5b6,
        0xe69a2,
        Some(0xd02115f512009d4d),
    );
    verify_fc(
        5,
        16,
        0xc2cd789a380208a9,
        0x19999e3fa46d6753,
        0x25f01e,
        0x1f22bd,
        Some(0xabe423040a33),
    );
    verify_fc(
        5,
        16,
        0xbe3edc0a1ef2a4f0,
        0x4da98f1d3099fdf5,
        0x3feb18,
        0x31501e,
        Some(0x7300a3a03ac5),
    );
    verify_fc(
        6,
        16,
        0xc965815a47c5,
        0xf5e008d6af57,
        0x1f121a,
        0x1cabbe,
        Some(0xc8cc6947),
    );
    verify_fc(
        6,
        16,
        0xd420677f6cbd,
        0x5894aa2ca1af,
        0x2efde9,
        0xc2121,
        Some(0x421bb8ec),
    );
    verify_fc(7, 16, 0x5fec898f, 0x82283d15, 0x14f410, 0x24c3c2, Some(0x0));
    verify_fc(7, 16, 0x64ac5db9, 0x7923986, 0x590fd, 0x1c74a2, Some(0x0));
}
