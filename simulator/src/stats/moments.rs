//! Online moments and the reduction used to combine them.
//!
//! Floating point addition is not associative, so an aggregate that folds results in whatever order
//! workers happen to finish is not reproducible. Leaves are therefore combined by [`tree_reduce`] in
//! index order with a shape fixed by the leaf count alone, which is what makes a campaign's answer
//! independent of how many workers ran it.

/// Mean and variance accumulated in one pass, after Welford. `merge` is the pairwise form from
/// Chan, Golub, and LeVeque.
#[derive(Debug, Clone, Copy, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct Welford {
    n: u64,
    mean: f64,
    m2: f64,
}

impl Welford {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A single-sample accumulator: one run's contribution, before any combining.
    #[must_use]
    pub fn of(x: f64) -> Self {
        Self {
            n: 1,
            mean: x,
            m2: 0.0,
        }
    }

    pub fn push(&mut self, x: f64) {
        self.n += 1;
        let delta = x - self.mean;
        self.mean += delta / self.n as f64;
        self.m2 += delta * (x - self.mean);
    }

    #[must_use]
    pub fn merge(a: &Self, b: &Self) -> Self {
        if a.n == 0 {
            return *b;
        }
        if b.n == 0 {
            return *a;
        }
        let n = a.n + b.n;
        let delta = b.mean - a.mean;
        let na = a.n as f64;
        let nb = b.n as f64;
        Self {
            n,
            mean: a.mean + delta * (nb / n as f64),
            m2: a.m2 + b.m2 + delta * delta * (na * nb / n as f64),
        }
    }

    #[must_use]
    pub fn n(&self) -> u64 {
        self.n
    }

    #[must_use]
    pub fn mean(&self) -> f64 {
        self.mean
    }

    /// Sample variance, `None` for fewer than two samples.
    #[must_use]
    pub fn variance(&self) -> Option<f64> {
        (self.n >= 2).then(|| self.m2 / (self.n - 1) as f64)
    }

    #[must_use]
    pub fn std_dev(&self) -> Option<f64> {
        self.variance().map(f64::sqrt)
    }

    /// Standard error of the mean.
    #[must_use]
    pub fn std_error(&self) -> Option<f64> {
        self.std_dev().map(|s| s / (self.n as f64).sqrt())
    }
}

/// Combine leaves pairwise in index order. The recursion splits at the midpoint, so the shape
/// depends only on `leaves.len()` and the result is bit-identical across runs and worker counts.
///
/// Leaves must be per-run, not per-worker: pre-merging a worker's runs into one leaf makes the
/// shape depend on the work distribution and reintroduces the drift this avoids.
#[must_use]
pub fn tree_reduce(leaves: &[Welford]) -> Welford {
    match leaves.len() {
        0 => Welford::new(),
        1 => leaves[0],
        n => {
            let (left, right) = leaves.split_at(n / 2);
            Welford::merge(&tree_reduce(left), &tree_reduce(right))
        }
    }
}

#[cfg(test)]
#[path = "../../tests/unit/stats/moments/tests.rs"]
mod tests;
