use dg_xch_core::plots::PlotTable;
use dg_xch_pos1::constants::{K_C3R, K_CHECKPOINT1INTERVAL, K_ENTRIES_PER_PARK, K_RVALUES};
use dg_xch_pos1::encoding::{ans_decode_deltas, ans_encode_deltas};
use dg_xch_pos1::entry_sizes::EntrySizes;

/// Deterministic geometric deltas, the shape a plot's line point deltas actually take: mostly
/// small, with a thin tail. 0xff is excluded because the decoder treats it as a corrupt park.
fn deltas(count: usize, mean: f64, seed: u64) -> Vec<u8> {
    let mut state = seed | 1;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let u = ((state >> 11) as f64 + 0.5) / (1u64 << 53) as f64;
        let v = (-mean * (1.0 - u).ln()).floor();
        out.push(v.clamp(0.0, 254.0) as u8);
    }
    out
}

fn round_trip(count: usize, mean: f64, r: f64, seed: u64) -> Vec<u8> {
    let source = deltas(count, mean, seed);
    let encoded = ans_encode_deltas(&source, r).expect("encodes");
    assert!(!encoded.is_empty(), "encoder produced nothing for r={r}");
    let (decoded_count, decoded) =
        ans_decode_deltas(&encoded, encoded.len(), count, r).expect("decodes");
    assert_eq!(decoded_count, count, "symbol count changed for r={r}");
    assert_eq!(
        decoded, source,
        "deltas changed through the round trip r={r}"
    );
    encoded
}

#[test]
fn line_point_deltas_round_trip_for_every_table_r_value() {
    // K_RVALUES[6] is 0.0, the unused table 7 slot.
    for (table, r) in K_RVALUES.iter().enumerate().take(6) {
        let mean = if table == 0 { 5.6 } else { 3.5 };
        round_trip(
            K_ENTRIES_PER_PARK as usize - 1,
            mean,
            *r,
            0xF00D + table as u64,
        );
    }
}

#[test]
fn c3_deltas_round_trip() {
    round_trip(K_CHECKPOINT1INTERVAL as usize, 2.4, K_C3R, 0xC3C3);
}

#[test]
fn an_encoded_park_fits_its_delta_budget() {
    // The park stride reserves calculate_max_deltas_size bytes for the deltas. If a realistic park
    // did not fit, every plot this writes would be malformed.
    for (table, r) in K_RVALUES.iter().enumerate().take(6) {
        let plot_table = match table {
            0 => PlotTable::Table1,
            1 => PlotTable::Table2,
            2 => PlotTable::Table3,
            3 => PlotTable::Table4,
            4 => PlotTable::Table5,
            _ => PlotTable::Table6,
        };
        let mean = if table == 0 { 5.6 } else { 3.5 };
        let encoded = round_trip(
            K_ENTRIES_PER_PARK as usize - 1,
            mean,
            *r,
            0xBEEF + table as u64,
        );
        let budget = EntrySizes::calculate_max_deltas_size(plot_table) as usize;
        assert!(
            encoded.len() <= budget,
            "table {table} encoded to {} bytes, budget is {budget}",
            encoded.len()
        );
    }
}

#[test]
fn an_all_zero_run_round_trips() {
    // The degenerate park: every line point exactly one apart.
    let source = vec![0u8; K_ENTRIES_PER_PARK as usize - 1];
    let encoded = ans_encode_deltas(&source, K_RVALUES[1]).expect("encodes");
    let (count, decoded) =
        ans_decode_deltas(&encoded, encoded.len(), source.len(), K_RVALUES[1]).expect("decodes");
    assert_eq!(count, source.len());
    assert_eq!(decoded, source);
}

#[test]
fn a_short_input_is_refused_rather_than_silently_truncated() {
    // Two symbols cannot pay for the two state flushes, so the reference returns nothing and the
    // caller is expected to store the park uncompressed.
    assert!(
        ans_encode_deltas(&[1, 2], K_RVALUES[1])
            .expect("encodes")
            .is_empty()
    );
}
