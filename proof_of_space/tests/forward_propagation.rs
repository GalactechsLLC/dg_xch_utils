use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::traits::SizedBytes;
use dg_xch_pos::plots::plot_writer::{forward_propagate, proof_bytes, proof_xs};
use dg_xch_pos::utils::bit_reader::BitReader;
use dg_xch_pos::verifier::validate_proof;

const K: u8 = 18;

fn plot_id() -> Bytes32 {
    Bytes32::from([0x2Au8; 32])
}

/// A challenge whose leading `k` bits are `f7`, which is what `validate_proof` compares against.
fn challenge_for(f7: u64, k: u8, tail: u8) -> Vec<u8> {
    let mut bits = BitReader::new(f7, k as usize);
    let mut remaining = 256 - k as usize;
    while remaining > 0 {
        let take = remaining.min(8);
        bits.append_value(u64::from(tail), take);
        remaining -= take;
    }
    bits.to_bytes()
}

#[test]
fn every_table_is_populated_and_sorted() {
    let tables = forward_propagate(K, plot_id()).expect("forward propagation");
    for table in 1..=7 {
        let entries = tables.table(table);
        assert!(!entries.is_empty(), "table {table} is empty");
        assert!(
            entries.windows(2).all(|w| w[0].y <= w[1].y),
            "table {table} is not sorted by y"
        );
    }
    assert_eq!(tables.table(1).len(), 1usize << K);
}

#[test]
fn a_table_seven_entry_yields_a_proof_the_verifier_accepts() {
    let tables = forward_propagate(K, plot_id()).expect("forward propagation");
    let id = plot_id().bytes();
    let mut checked = 0;
    for index in 0..tables.table(7).len().min(32) {
        let f7 = tables.f7(index);
        let xs = proof_xs(&tables, index);
        assert_eq!(xs.len(), 64, "proof at {index} is not 64 x values");
        let proof = proof_bytes(K, &xs);
        let challenge = challenge_for(f7, K, 0x5A);
        let quality =
            validate_proof(&id, K, &proof, &challenge).expect("proof runs through the verifier");
        assert_ne!(
            quality,
            Bytes32::default(),
            "proof at index {index} did not reproduce f7 {f7}"
        );
        checked += 1;
    }
    assert!(checked > 0, "table 7 had no entries to check");
}

#[test]
fn the_same_plot_id_forward_propagates_identically() {
    let a = forward_propagate(K, plot_id()).expect("forward propagation");
    let b = forward_propagate(K, plot_id()).expect("forward propagation");
    for table in 1..=7 {
        let (ta, tb) = (a.table(table), b.table(table));
        assert_eq!(ta.len(), tb.len(), "table {table} length moved");
        assert!(
            ta.iter()
                .zip(tb)
                .all(|(x, y)| x.y == y.y && x.left == y.left && x.right == y.right),
            "table {table} contents moved"
        );
    }
}

#[test]
fn a_different_plot_id_gives_a_different_chain() {
    let a = forward_propagate(K, plot_id()).expect("forward propagation");
    let b = forward_propagate(K, Bytes32::from([0x77u8; 32])).expect("forward propagation");
    assert_ne!(a.table(7)[0].y, b.table(7)[0].y);
}

#[test]
fn a_proof_is_refused_against_the_wrong_f7() {
    let tables = forward_propagate(K, plot_id()).expect("forward propagation");
    let id = plot_id().bytes();
    let xs = proof_xs(&tables, 0);
    let proof = proof_bytes(K, &xs);
    let wrong = challenge_for(tables.f7(0) ^ 1, K, 0x5A);
    let quality = validate_proof(&id, K, &proof, &wrong).expect("verifier runs");
    assert_eq!(quality, Bytes32::default(), "a wrong f7 was accepted");
}

#[test]
fn packing_round_trips_through_the_verifiers_reader() {
    let xs: Vec<u64> = (0..64).map(|i| (i * 3607) % (1u64 << K)).collect();
    let bytes = proof_bytes(K, &xs);
    assert_eq!(bytes.len(), 64 * K as usize / 8);
    assert_eq!(
        dg_xch_pos::verifier::uncompress_proof(&bytes, K as usize),
        xs
    );
}

#[test]
fn every_stored_pair_satisfies_the_match_rule() {
    let tables = forward_propagate(K, plot_id()).expect("forward propagation");
    for table in 2..=7usize {
        let previous = tables.table(table - 1);
        for (i, e) in tables.table(table).iter().enumerate() {
            let (yl, yr) = (previous[e.left as usize].y, previous[e.right as usize].y);
            assert!(
                dg_xch_pos::plots::fx_generator::fx_match(&yl, &yr),
                "table {table} entry {i} pair ({yl}, {yr}) does not match"
            );
        }
    }
}

#[test]
fn replaying_a_proof_reproduces_the_stored_chain() {
    use dg_xch_core::plots::PlotTable;
    use dg_xch_pos::plots::fx_generator::{fx_gen, get_proof_f1_and_meta};

    let tables = forward_propagate(K, plot_id()).expect("forward propagation");
    let index = 0usize;
    let xs = proof_xs(&tables, index);

    // The entry indices this proof touches, level by level, in the same left to right order the
    // 64 x values are laid out in.
    let mut levels: Vec<Vec<u32>> = vec![vec![index as u32]];
    for table in (2..=7usize).rev() {
        let current = levels.last().expect("a level").clone();
        let mut next = Vec::with_capacity(current.len() * 2);
        for e in &current {
            let entry = &tables.table(table)[*e as usize];
            next.push(entry.left);
            next.push(entry.right);
        }
        levels.push(next);
    }

    let mut fx = vec![0u64; 64];
    let mut meta = Vec::new();
    get_proof_f1_and_meta(u32::from(K), &plot_id().bytes(), &xs, &mut fx, &mut meta)
        .expect("f1 and meta");
    for (i, e) in levels[6].iter().enumerate() {
        assert_eq!(
            fx[i],
            tables.table(1)[*e as usize].y,
            "table 1 leaf {i} y diverged"
        );
    }

    let mut count = 64;
    for (step, table) in [
        PlotTable::Table2,
        PlotTable::Table3,
        PlotTable::Table4,
        PlotTable::Table5,
        PlotTable::Table6,
        PlotTable::Table7,
    ]
    .into_iter()
    .enumerate()
    {
        let mut i = 0;
        let mut dst = 0;
        while i < count {
            let mut out_y = 0u64;
            let mut out_meta = dg_xch_pos::utils::bit_reader::BitReader::default();
            fx_gen(
                table,
                u32::from(K),
                fx[i],
                &meta[i],
                &meta[i + 1],
                &mut out_y,
                &mut out_meta,
            )
            .expect("fx_gen");
            fx[dst] = out_y;
            meta[dst] = out_meta;
            i += 2;
            dst += 1;
        }
        count >>= 1;
        let level = &levels[5 - step];
        for (j, e) in level.iter().enumerate() {
            assert_eq!(
                fx[j],
                tables.table(step + 2)[*e as usize].y,
                "table {} entry {j} y diverged",
                step + 2
            );
        }
    }
}

#[test]
fn the_batch_f1_agrees_with_the_verifiers_f1() {
    use dg_xch_pos::f_calc::F1Calculator;
    use dg_xch_pos::plots::fx_generator::get_proof_f1_and_meta;

    let id = plot_id().bytes();
    let xs: Vec<u64> = (0..64u64).collect();
    let mut expected = vec![0u64; 64];
    let mut meta = Vec::new();
    get_proof_f1_and_meta(u32::from(K), &id, &xs, &mut expected, &mut meta).expect("f1");

    let f1 = F1Calculator::new(K, &id);
    let mut batched = vec![0u64; 64];
    f1.calculate_buckets(0, 64, &mut batched);

    assert_eq!(
        batched, expected,
        "batched f1 disagrees with the verifier's f1"
    );
}
#[test]
fn f1_extraction_matches_individual_bits_across_block_boundaries() {
    use dg_xch_pos::chacha8::{ChachaContext, chacha8_get_keystream, chacha8_keysetup};
    use dg_xch_pos::constants::K_EXTRA_BITS;
    use dg_xch_pos::plots::fx_generator::get_proof_f1_and_meta;

    let plot_id = [0x63; 32];
    let mut key = [0x63; 32];
    key[0] = 1;
    let mut context = ChachaContext { input: [0; 16] };
    chacha8_keysetup(&mut context, &key, None);
    for size in 32..=50u32 {
        for offset in (0..512u64).step_by(64) {
            let proof: [u64; 64] =
                std::array::from_fn(|index| (1 << (size - 1)) + offset + index as u64);
            let mut actual = [0; 64];
            let mut metadata = Vec::new();
            get_proof_f1_and_meta(size, &plot_id, &proof, &mut actual, &mut metadata).unwrap();
            for (index, value) in proof.iter().enumerate() {
                let position = value * u64::from(size);
                let mut stream = Vec::new();
                chacha8_get_keystream(&context, position / 512, 2, &mut stream);
                let mut expected = 0u64;
                for bit in position % 512..position % 512 + u64::from(size) {
                    expected = (expected << 1)
                        | u64::from((stream[bit as usize / 8] >> (7 - bit % 8)) & 1);
                }
                expected = (expected << K_EXTRA_BITS) | (value >> (size - u32::from(K_EXTRA_BITS)));
                assert_eq!(actual[index], expected, "k={size}, x={value}");
            }
        }
    }
}
