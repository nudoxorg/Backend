//! The single scored-result wrapper for every search surface.
//!
//! Replaces the old `Hit<T>` (which made the score `Option`al — a search hit
//! with no score is meaningless). A [`Scored`] always carries a provably-finite
//! [`Score`], so ranking can never encounter a `None` or a `NaN`.

use serde::{Deserialize, Serialize};

use crate::score::Score;

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
	pub const fn new(value: T, score: Score) -> Self { Self { value, score } }

	/// Map the payload, preserving the score.
	pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Scored<U> {
		Scored { value: f(self.value), score: self.score }
	}
}

impl<T: PartialEq> PartialOrd for Scored<T> {
	/// Ordered by score alone (descending relevance is the caller's convention).
	fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
		self.score.partial_cmp(&other.score)
	}
}
