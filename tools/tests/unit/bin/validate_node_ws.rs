use super::{NO_TIMELORD_SERIES, metric_value};
use dg_xch_core::protocols::ProtocolMessageTypes;

// Code-discrimination unit: the harness must report the timelord frame ABSENT when a captured
// stream has it removed, and PRESENT when it is there — the same determination the live timelord
// loop makes (got = the stream carried a decodable NewPeakTimelord), reduced to the frame scan.
fn stream_has_peak_timelord(frame_types: &[u8]) -> bool {
    frame_types.contains(&(ProtocolMessageTypes::NewPeakTimelord as u8))
}

#[test]
fn reports_absent_when_peak_timelord_frame_removed() {
    // A realistic capture: NewSignagePoint(8), NewPeak(20), NewPeakTimelord(13), NewSignagePoint(8).
    let full = [8u8, 20, 13, 8];
    assert!(
        stream_has_peak_timelord(&full),
        "must detect NewPeakTimelord when present"
    );
    let removed: Vec<u8> = full.iter().copied().filter(|t| *t != 13).collect();
    assert!(
        !stream_has_peak_timelord(&removed),
        "must report ABSENT once the NewPeakTimelord frame is removed"
    );
}

#[test]
fn metric_value_reads_labeled_no_timelord_series() {
    let body = format!(
        "# TYPE fullnode_producer_candidates_dropped_total counter\n{NO_TIMELORD_SERIES} 7\nother 3\n"
    );
    assert_eq!(metric_value(&body, NO_TIMELORD_SERIES), Some(7.0));
    assert_eq!(metric_value("nothing here\n", NO_TIMELORD_SERIES), None);
}
