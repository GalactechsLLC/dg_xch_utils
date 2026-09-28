use super::*;

fn naive(xs: &[f64]) -> (f64, f64) {
    let n = xs.len() as f64;
    let mean = xs.iter().sum::<f64>() / n;
    let var = xs.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (n - 1.0);
    (mean, var)
}

fn samples() -> Vec<f64> {
    (0..1_000).map(|i| 18.75 + (i % 37) as f64 * 0.5).collect()
}

#[test]
fn moments_match_a_two_pass_computation() {
    let xs = samples();
    let mut w = Welford::new();
    for x in &xs {
        w.push(*x);
    }
    let (mean, var) = naive(&xs);
    assert_eq!(w.n(), xs.len() as u64);
    assert!((w.mean() - mean).abs() < 1e-9, "{} vs {mean}", w.mean());
    let got = w.variance().expect("more than one sample");
    assert!((got - var).abs() < 1e-9, "{got} vs {var}");
}

#[test]
fn merging_halves_matches_pushing_the_whole() {
    let xs = samples();
    let (left, right) = xs.split_at(xs.len() / 2);
    let fold = |chunk: &[f64]| {
        let mut w = Welford::new();
        for x in chunk {
            w.push(*x);
        }
        w
    };
    let merged = Welford::merge(&fold(left), &fold(right));
    let whole = fold(&xs);
    assert_eq!(merged.n(), whole.n());
    assert!((merged.mean() - whole.mean()).abs() < 1e-9);
    let (a, b) = (
        merged.variance().expect("n >= 2"),
        whole.variance().expect("n >= 2"),
    );
    assert!((a - b).abs() < 1e-9, "{a} vs {b}");
}

#[test]
fn the_reduction_is_bit_identical_for_the_same_leaves() {
    let leaves: Vec<Welford> = samples().into_iter().map(Welford::of).collect();
    let a = tree_reduce(&leaves);
    let b = tree_reduce(&leaves);
    assert_eq!(a, b);
    assert_eq!(a.mean().to_bits(), b.mean().to_bits());
}

#[test]
fn the_reduction_does_not_depend_on_worker_count() {
    // Workers claim runs round robin and fill their results back by run index. Whatever the
    // worker count, the leaf slice is the same and so is every bit of the answer.
    let xs = samples();
    let reference = tree_reduce(&xs.iter().copied().map(Welford::of).collect::<Vec<_>>());
    for workers in [1usize, 2, 3, 7, 16] {
        let mut leaves = vec![Welford::new(); xs.len()];
        for worker in 0..workers {
            for (i, x) in xs.iter().enumerate() {
                if i % workers == worker {
                    leaves[i] = Welford::of(*x);
                }
            }
        }
        let got = tree_reduce(&leaves);
        assert_eq!(
            got.mean().to_bits(),
            reference.mean().to_bits(),
            "{workers} workers drifted"
        );
        assert_eq!(got, reference, "{workers} workers drifted");
    }
}

#[test]
fn variance_and_error_need_two_samples() {
    let mut w = Welford::new();
    assert_eq!(w.n(), 0);
    assert!(w.variance().is_none());
    w.push(4.0);
    assert!(w.variance().is_none());
    assert!(w.std_error().is_none());
    w.push(6.0);
    assert_eq!(w.mean(), 5.0);
    assert_eq!(w.variance(), Some(2.0));
    assert_eq!(w.std_error(), Some(1.0));
}

#[test]
fn empty_and_single_leaf_reductions_are_well_defined() {
    assert_eq!(tree_reduce(&[]).n(), 0);
    let one = Welford::of(3.5);
    assert_eq!(tree_reduce(&[one]), one);
}
