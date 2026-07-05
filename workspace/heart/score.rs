//! Relevance scoring and the scored-value wrapper.

use nutype::nutype;
use serde::{Deserialize, Serialize};

#[nutype(
    validate(finite),
    derive(
        Debug,
        Clone,
        Copy,
        PartialEq,
        Eq,
        PartialOrd,
        Ord,
        Display,
        Serialize,
        Deserialize
    )
)]
pub struct Score(f32);

/// A value paired with its (provably finite) relevance score.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scored<T> {
    /// The result payload.
    pub value: T,
    /// Its relevance score.
    pub score: Score,
}

impl<T> Scored<T> {
    /// Pair a value with a score.
    pub const fn new(value: T, score: Score) -> Self {
        Self { value, score }
    }

    /// Map the payload, preserving the score.
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Scored<U> {
        Scored {
            value: f(self.value),
            score: self.score,
        }
    }
}

impl<T: PartialEq> PartialOrd for Scored<T> {
    /// Ordered by score alone (descending relevance is the caller's convention).
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        self.score.partial_cmp(&other.score)
    }
}
