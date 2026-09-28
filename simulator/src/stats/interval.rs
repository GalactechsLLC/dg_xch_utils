use crate::stats::moments::Welford;

/// Two-sided 95% standard normal quantile.
const Z_95: f64 = 1.959_963_984_540_054;

/// How an interval was estimated. Carried with the metric, since methods disagree on a skewed
/// metric.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntervalMethod {
    /// Mean plus or minus `Z_95` standard errors. Assumes the central limit theorem has bitten.
    NormalApprox,
}

/// A metric and its uncertainty. Every constructor sets the interval, the method, the sample
/// count, and the seed.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MetricResult {
    pub value: f64,
    pub lo: f64,
    pub hi: f64,
    pub method: IntervalMethod,
    pub n: u64,
    pub seed: u64,
}

impl MetricResult {
    /// The mean with a normal-approximation interval, or `None` below two samples, where the
    /// variance is undefined.
    #[must_use]
    pub fn normal_approx(w: &Welford, seed: u64) -> Option<Self> {
        let half_width = Z_95 * w.std_error()?;
        Some(Self {
            value: w.mean(),
            lo: w.mean() - half_width,
            hi: w.mean() + half_width,
            method: IntervalMethod::NormalApprox,
            n: w.n(),
            seed,
        })
    }

    #[must_use]
    pub fn half_width(&self) -> f64 {
        (self.hi - self.lo) / 2.0
    }

    #[must_use]
    pub fn contains(&self, x: f64) -> bool {
        self.lo <= x && x <= self.hi
    }
}

#[cfg(test)]
#[path = "../../tests/unit/stats/interval/tests.rs"]
mod tests;
