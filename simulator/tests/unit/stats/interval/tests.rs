use super::*;
use crate::stats::rng::{Domain, derive_rng};
use rand_chacha::ChaCha20Rng;
use rand_chacha::rand_core::Rng;

/// Box-Muller over the substream, so the coverage check below is driven by the same generator
/// the simulator itself uses.
fn standard_normal(rng: &mut ChaCha20Rng) -> f64 {
    let unit = |r: &mut ChaCha20Rng| ((r.next_u64() >> 11) as f64 + 0.5) / (1u64 << 53) as f64;
    let u1 = unit(rng);
    let u2 = unit(rng);
    (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
}

#[test]
fn an_interval_needs_two_samples() {
    let mut w = Welford::new();
    assert!(MetricResult::normal_approx(&w, 0).is_none());
    w.push(1.0);
    assert!(MetricResult::normal_approx(&w, 0).is_none());
    w.push(3.0);
    assert!(MetricResult::normal_approx(&w, 0).is_some());
}

#[test]
fn the_interval_is_centred_on_the_mean_and_carries_its_provenance() {
    let mut w = Welford::new();
    for x in [4.0, 6.0, 8.0, 10.0] {
        w.push(x);
    }
    let m = MetricResult::normal_approx(&w, 99).expect("four samples");
    assert_eq!(m.value, 7.0);
    assert_eq!(m.n, 4);
    assert_eq!(m.seed, 99);
    assert_eq!(m.method, IntervalMethod::NormalApprox);
    assert!((m.value - m.half_width() - m.lo).abs() < 1e-12);
    assert!((m.value + m.half_width() - m.hi).abs() < 1e-12);
    assert!(m.contains(7.0));
}

#[test]
fn a_wider_spread_gives_a_wider_interval() {
    let tight = {
        let mut w = Welford::new();
        for x in [9.9, 10.0, 10.1] {
            w.push(x);
        }
        MetricResult::normal_approx(&w, 0).expect("three samples")
    };
    let loose = {
        let mut w = Welford::new();
        for x in [5.0, 10.0, 15.0] {
            w.push(x);
        }
        MetricResult::normal_approx(&w, 0).expect("three samples")
    };
    assert!(loose.half_width() > tight.half_width());
}

#[test]
fn ninety_five_percent_intervals_cover_the_true_mean_about_that_often() {
    const TRIALS: usize = 2_000;
    const PER_TRIAL: usize = 60;
    const TRUE_MEAN: f64 = 10.0;
    const SIGMA: f64 = 3.0;

    let mut covered = 0;
    for trial in 0..TRIALS {
        let mut rng = derive_rng(1, Domain::Quality, trial as u64);
        let mut w = Welford::new();
        for _ in 0..PER_TRIAL {
            w.push(TRUE_MEAN + SIGMA * standard_normal(&mut rng));
        }
        if MetricResult::normal_approx(&w, 1)
            .expect("many samples")
            .contains(TRUE_MEAN)
        {
            covered += 1;
        }
    }
    // Fixed seeds, so the rate is deterministic. The binomial standard error at 2000 trials is
    // about 0.5 points.
    let rate = covered as f64 / TRIALS as f64;
    assert!((0.93..=0.97).contains(&rate), "coverage was {rate}");
}
