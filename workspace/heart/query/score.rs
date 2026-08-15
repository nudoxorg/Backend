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

/// A stable, total tiebreak key for a ranked value.
///
/// # Why this exists
///
/// [`Scored`] used to order by *score alone*. Two hits with the same score then
/// had no defined relative order, so a `sort` over them was free to return
/// either arrangement — which is exactly the input keyset pagination cannot
/// tolerate: the cursor `(score, key)` assumes the item *after* a given
/// `(score, key)` is deterministic, and a score-only order leaves the tie group
/// unordered, so the page boundary could drop or repeat a row on resume.
///
/// Threading a stable per-value key into the order makes ties deterministic.
/// Every ranked payload already carries one (`Symbol::id`, `Id<_>` itself), so
/// this is a projection, not new data.
///
/// # Contract
///
/// `rank_key` must be a **total identity**: equal keys imply equal values. The
/// derivation from a content/entity id (which is what every implementor here
/// uses) satisfies this by construction — two values with the same durable id
/// are the same thing — which is what keeps [`Scored`]'s [`Ord`] consistent with
/// its derived [`Eq`].
pub trait RankKey {
    /// The comparable key type (an id, usually `Copy`).
    type Key: Ord;
    /// The stable tiebreak key for this value.
    fn rank_key(&self) -> Self::Key;
}

/// Any tagged id is its own rank key.
impl<T> RankKey for crate::identity::Id<T> {
    type Key = crate::identity::Id<T>;
    fn rank_key(&self) -> Self::Key {
        *self
    }
}

impl<T: RankKey + Eq> Ord for Scored<T> {
    /// The total **ranking order**: higher score first, then `rank_key`
    /// ascending on ties. Sorting a slice of `Scored<T>` with the default
    /// ascending [`Ord`] therefore yields best-first in exactly the
    /// `(score DESC, key ASC)` total order the keyset cursor
    /// ([`crate::page::keyset_page`]) advances through — the two can never
    /// disagree about which hit follows which.
    ///
    /// [`Score`] is `nutype(validate(finite))`, so its own `Ord` is total (no
    /// NaN can exist to poison the comparison); this method inherits that
    /// totality.
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        other
            .score
            .cmp(&self.score)
            .then_with(|| self.value.rank_key().cmp(&other.value.rank_key()))
    }
}

impl<T: RankKey + Eq> PartialOrd for Scored<T> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct Row {
        id: u32,
    }
    impl RankKey for Row {
        type Key = u32;
        fn rank_key(&self) -> u32 {
            self.id
        }
    }

    fn s(id: u32, score: f32) -> Scored<Row> {
        Scored::new(Row { id }, Score::try_new(score).unwrap())
    }

    #[test]
    fn ties_break_deterministically_by_key_ascending() {
        // Three hits, two of them score-tied. A score-only order left the two
        // tied rows in arbitrary relative position; the key tiebreak pins it.
        let mut rows = [s(9, 0.5), s(2, 0.9), s(4, 0.5)];
        rows.sort();
        let order: Vec<u32> = rows.iter().map(|r| r.value.id).collect();
        // Best score first (id 2), then the tie group by key ascending (4, 9).
        assert_eq!(order, vec![2, 4, 9]);
    }

    #[test]
    fn order_is_best_first() {
        assert!(s(1, 0.9) < s(1, 0.1), "a higher score sorts earlier");
    }
}
