//! Weighted quality-score accumulator.

use std::borrow::Borrow;

/// Weighted quality score accumulator.  Each component contributes a clamped
/// value and a maximum; `total()` = Σclamp(v,0,max) / Σmax → 0..=1.
#[derive(Debug, Clone, Default)]
pub struct Score {
    scores: Vec<(f64, f64, &'static str)>,
    total: f64,
}

/// Handle returned by score-adding methods; allows post-hoc adjustment.
pub struct ScoreAdjustment<'a> {
    score: &'a mut f64,
}

impl Score {
    /// Create a new empty accumulator.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add `score` points if `has_it`, else 0.
    #[inline]
    pub fn has(&mut self, for_what: &'static str, score: u32, has_it: bool) -> ScoreAdjustment<'_> {
        self.score_f(
            for_what,
            f64::from(score),
            if has_it { f64::from(score) } else { 0. },
        )
    }

    /// Add `n` points (clamped to `max_score`).
    #[inline]
    pub fn n(
        &mut self,
        for_what: &'static str,
        max_score: u32,
        n: impl Into<i64>,
    ) -> ScoreAdjustment<'_> {
        self.score_f(for_what, f64::from(max_score), n.into() as f64)
    }

    /// Add `max_score * n` where `n ∈ 0..=1`.
    #[track_caller]
    pub fn frac(
        &mut self,
        for_what: &'static str,
        max_score: u32,
        n: impl Into<f64>,
    ) -> ScoreAdjustment<'_> {
        let n = n.into();
        assert!((0. ..=1.).contains(&n), "frac n={n} out of 0..=1");
        let max = f64::from(max_score);
        self.score_f(for_what, max, n * max)
    }

    /// Raw score entry.
    #[track_caller]
    pub fn score_f(
        &mut self,
        for_what: &'static str,
        max_score: f64,
        n: impl Into<f64>,
    ) -> ScoreAdjustment<'_> {
        let n = n.into();
        assert!(max_score > 0.);
        self.total += max_score;
        self.scores.push((n.max(0.), max_score, for_what));
        ScoreAdjustment {
            score: &mut self.scores.last_mut().unwrap().0,
        }
    }

    /// Embed a sub-score group; `max_score` is the weight of the sub-group in
    /// this accumulator.
    pub fn group(
        &mut self,
        for_what: &'static str,
        max_score: u32,
        group: impl Borrow<Self>,
    ) -> ScoreAdjustment<'_> {
        self.frac(for_what, max_score, group.borrow().total())
    }

    /// Compute the overall score in 0..=1.
    #[must_use]
    pub fn total(&self) -> f64 {
        if self.total == 0. {
            return 0.;
        }
        let sum: f64 = self
            .scores
            .iter()
            .map(|&(v, limit, _)| v.max(0.).min(limit))
            .sum();
        sum / self.total
    }
}

impl ScoreAdjustment<'_> {
    /// Multiply the stored value by `by`.
    pub fn mul(&mut self, by: f64) {
        *self.score *= by;
    }
    /// Apply an arbitrary transformation to the stored value.
    pub fn adj(&mut self, f: impl FnOnce(f64) -> f64) {
        *self.score = f(*self.score);
    }
}
