//! The hand (D-Hand): at most five things you held with intent. Visiting
//! never counts; holding does (⌘D, ⌘-click, Space-Space, a copied
//! signature, a compare, an added package).
//!
//! The hand keeps what you hold and when you last touched each; the order
//! it is *shown* in is recomputed from the held set (what feeds what), so it
//! is never stored.

use crate::core::PackageId;
use crate::navigation::Coordinate;
use std::sync::Arc;

/// Why a thing is in the hand.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum HeldWhy {
    /// ⌘D, ⌘-click, Space-Space.
    Pin,
    /// A copied signature.
    Copy,
    /// A compare.
    Compare,
    /// An added package.
    Add,
}

/// One held thing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Held {
    /// Its package (the route's).
    pub package: PackageId,
    /// The declaration, when it is one (a package holds none).
    pub id: Option<Coordinate>,
    /// Why it was held.
    pub why: HeldWhy,
    /// When it was held (unix ms).
    pub held_at: u64,
    /// When it was last touched: held, gone to, or used (unix ms).
    pub touched_at: u64,
}

impl Held {
    /// Whether `other` is the same thing.
    #[must_use]
    pub fn same(&self, other: &Self) -> bool {
        self.package == other.package && self.id == other.id
    }
}

/// A card unused for a day goes hollow.
pub const HOLLOW_MS: u64 = 24 * 60 * 60 * 1000;

/// What the hand holds, in the order it was held.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Hand {
    held: Arc<[Held]>,
}

impl Hand {
    /// At most five cards.
    pub const MAX: usize = 5;

    /// A hand of `held` (the first [`Self::MAX`]).
    #[must_use]
    pub fn of(held: impl IntoIterator<Item = Held>) -> Self {
        Self {
            held: held.into_iter().take(Self::MAX).collect(),
        }
    }

    /// What it holds, in the order it was held.
    #[must_use]
    pub fn held(&self) -> &[Held] {
        &self.held
    }

    /// Whether it holds nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.held.is_empty()
    }

    /// Whether `held` holds the same thing as a card.
    #[must_use]
    pub fn holds(&self, held: &Held) -> bool {
        self.held.iter().any(|card| card.same(held))
    }

    /// Holds `item`. Holding what it already holds touches it; a sixth card
    /// pushes out the least recently touched.
    #[must_use]
    pub fn hold(&self, item: Held) -> Self {
        let mut held: Vec<Held> = self.held.to_vec();
        if let Some(card) = held.iter_mut().find(|card| card.same(&item)) {
            card.touched_at = item.touched_at;
            return Self { held: held.into() };
        }
        if held.len() >= Self::MAX
            && let Some(oldest) = held
                .iter()
                .enumerate()
                .min_by_key(|(_, card)| card.touched_at)
                .map(|(n, _)| n)
        {
            held.remove(oldest);
        }
        held.push(item);
        Self { held: held.into() }
    }

    /// Lets go of what matches `item`.
    #[must_use]
    pub fn let_go(&self, item: &Held) -> Self {
        Self {
            held: self
                .held
                .iter()
                .filter(|card| !card.same(item))
                .cloned()
                .collect(),
        }
    }

    /// Marks `item` touched at `at`.
    #[must_use]
    pub fn touch(&self, item: &Held, at: u64) -> Self {
        Self {
            held: self
                .held
                .iter()
                .map(|card| {
                    let mut card = card.clone();
                    if card.same(item) {
                        card.touched_at = card.touched_at.max(at);
                    }
                    card
                })
                .collect(),
        }
    }
}

/// Whether `card` went hollow by `now` (unused for a day).
#[must_use]
pub const fn hollow(card: &Held, now: u64) -> bool {
    now.saturating_sub(card.touched_at) > HOLLOW_MS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(name: &str, at: u64) -> Held {
        Held {
            package: PackageId::new("/fixture/present").expect("package"),
            id: Some(
                Coordinate::new(&format!("/fixture/present::glyph.rs:138::{name}"))
                    .expect("coordinate"),
            ),
            why: HeldWhy::Pin,
            held_at: at,
            touched_at: at,
        }
    }

    #[test]
    fn five_at_most_and_the_sixth_pushes_out_the_least_recently_touched() {
        let mut hand = Hand::default();
        for (n, name) in ["a", "b", "c", "d", "e"].iter().enumerate() {
            hand = hand.hold(card(name, n as u64 * 10));
        }
        // Touch the oldest: "b" is now the least recently touched.
        hand = hand.touch(&card("a", 0), 100);
        hand = hand.hold(card("f", 200));
        let names: Vec<&str> = hand
            .held()
            .iter()
            .map(|c| {
                c.id.as_ref()
                    .map_or("", |id| id.as_str().rsplit("::").next().unwrap_or(""))
            })
            .collect();
        assert_eq!(names, ["a", "c", "d", "e", "f"]);
    }

    #[test]
    fn holding_what_it_holds_touches_it_in_place() {
        let hand = Hand::default()
            .hold(card("a", 0))
            .hold(card("b", 5))
            .hold(card("a", 50));
        assert_eq!(hand.held().len(), 2);
        assert_eq!(hand.held()[0].touched_at, 50);
        assert!(!hollow(&hand.held()[0], 50 + HOLLOW_MS));
        assert!(hollow(&hand.held()[1], 50 + HOLLOW_MS));
        assert!(
            hand.let_go(&card("a", 0))
                .held()
                .iter()
                .all(|c| !c.same(&card("a", 0)))
        );
    }
}
