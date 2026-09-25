//! Delta-aware work units — the load-bearing abstraction of the index frontier.
//!
//! # Why this exists
//!
//! The capability review (and INDEX-CAPABILITY L43) found the same defect in
//! three places: cost tracks *accumulated state*, not *change*. Git enumerate
//! re-lists every tag; Homebrew re-upserts the whole formula dump; outbox
//! projections re-materialize what already landed. This module is the shared
//! vocabulary that makes "do nothing when nothing changed" the typed default.
//!
//! # Shape
//!
//! - [`Cursor`] — opaque, ecosystem-owned high-water mark (ETag, commit,
//!   catalog timestamp, generation hash). Compared for equality only.
//! - [`Delta<T>`] — the three sets a poller may emit. Empty sets are
//!   first-class; an empty delta with an advanced cursor is a clock bump.
//! - [`PollOutcome<T>`] — either [`Unchanged`](PollOutcome::Unchanged) (cursor
//!   held) or [`Advanced`](PollOutcome::Advanced) (cursor moved, delta may be
//!   empty).
//! - [`ContentKey`] — identity of a projectable unit whose *payload hash*
//!   decides whether a sink must rewrite. Used by IR roots and vector points.

use blake3::Hash as Blake3Hash;
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

/// An opaque high-water mark. Equality is the only operation: if two cursors
/// compare equal, the upstream reported no progress.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Cursor {
    /// Stable feed / stem / sink identity this cursor belongs to.
    pub scope: SmolStr,
    /// The mark itself (ETag, hex digest, ISO timestamp, …).
    pub mark: SmolStr,
}

impl Cursor {
    /// Construct a cursor for `scope` at `mark`.
    pub fn new(scope: impl Into<SmolStr>, mark: impl Into<SmolStr>) -> Self {
        Self {
            scope: scope.into(),
            mark: mark.into(),
        }
    }
}

/// The three disjoint sets a delta-aware poller may emit for one tick.
///
/// Invariant: an item must not appear in more than one set. Callers that
/// violate this lose determinism at the apply site; [`Delta::is_empty`] does
/// not check disjointness (that is a producer responsibility, asserted in
/// tests).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delta<T> {
    /// Newly observed items.
    pub added: Vec<T>,
    /// Items whose identity was known but whose payload changed.
    pub changed: Vec<T>,
    /// Items that disappeared from the upstream view.
    pub removed: Vec<T>,
}

impl<T> Delta<T> {
    /// A delta with nothing in any set.
    ///
    /// Not derived from [`Default`]: that bound would require `T: Default`,
    /// and content keys are not defaultable.
    pub fn empty() -> Self {
        Self {
            added: Vec::new(),
            changed: Vec::new(),
            removed: Vec::new(),
        }
    }

    /// True when every set is empty (clock-bump case).
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.changed.is_empty() && self.removed.is_empty()
    }

    /// Total items touched (added + changed + removed).
    pub fn len(&self) -> usize {
        self.added.len() + self.changed.len() + self.removed.len()
    }

    /// Map each set through `f`, preserving partition membership.
    pub fn map<U>(self, mut f: impl FnMut(T) -> U) -> Delta<U> {
        Delta {
            added: self.added.into_iter().map(&mut f).collect(),
            changed: self.changed.into_iter().map(&mut f).collect(),
            removed: self.removed.into_iter().map(&mut f).collect(),
        }
    }
}

/// Outcome of one poll against a cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PollOutcome<T> {
    /// Upstream reports the same mark; do not advance anything.
    Unchanged {
        /// The cursor that was confirmed current.
        cursor: Cursor,
    },
    /// Upstream advanced; apply `delta` then persist `cursor`.
    Advanced {
        /// The new high-water mark (must differ from the prior mark, or be a
        /// first observation).
        cursor: Cursor,
        /// Work to apply. May be empty (clock bump only).
        delta: Delta<T>,
    },
}

impl<T> PollOutcome<T> {
    /// True when this outcome requires a catalog write.
    pub fn needs_apply(&self) -> bool {
        match self {
            PollOutcome::Unchanged { .. } => false,
            PollOutcome::Advanced { delta, .. } => !delta.is_empty(),
        }
    }
}

/// Content-addressed identity of a projectable unit (IR entry, vector point,
/// pack member). Two keys with the same `(domain, id)` collide; the
/// [`payload`](ContentKey::payload) hash decides whether a sink rewrite is
/// required.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ContentKey {
    /// Projection domain (`"ir-entry"`, `"vector-point"`, …).
    pub domain: SmolStr,
    /// Stable identity within the domain (intro id, symbol id, …).
    pub id: SmolStr,
    /// BLAKE3 of the payload bytes the sink would write.
    pub payload: Blake3Hash,
}

impl ContentKey {
    /// Build a key for `domain`/`id` over `payload_bytes`.
    pub fn hash(domain: impl Into<SmolStr>, id: impl Into<SmolStr>, payload_bytes: &[u8]) -> Self {
        Self {
            domain: domain.into(),
            id: id.into(),
            payload: blake3::hash(payload_bytes),
        }
    }
}

/// Diff two ordered maps of [`ContentKey`] keyed by `id` within one domain.
///
/// Used by IR `GenerationRoot` fault-in and by the vector delta projector:
/// unchanged payload hashes become neither added nor changed.
pub fn content_delta(
    prior: &[ContentKey],
    next: &[ContentKey],
) -> Delta<ContentKey> {
    use std::collections::BTreeMap;

    let prior_map: BTreeMap<&SmolStr, &ContentKey> =
        prior.iter().map(|k| (&k.id, k)).collect();
    let next_map: BTreeMap<&SmolStr, &ContentKey> = next.iter().map(|k| (&k.id, k)).collect();

    let mut delta = Delta::empty();
    for (id, key) in &next_map {
        match prior_map.get(id) {
            None => delta.added.push((*key).clone()),
            Some(old) if old.payload != key.payload => delta.changed.push((*key).clone()),
            Some(_) => {}
        }
    }
    for (id, key) in &prior_map {
        if !next_map.contains_key(id) {
            delta.removed.push((*key).clone());
        }
    }
    delta
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_delta_needs_no_apply() {
        let outcome = PollOutcome::<()>::Advanced {
            cursor: Cursor::new("feed", "etag-1"),
            delta: Delta::empty(),
        };
        assert!(!outcome.needs_apply());
    }

    #[test]
    fn content_delta_skips_unchanged_payload() {
        let a = ContentKey::hash("ir", "intro-1", b"body-v1");
        let a2 = ContentKey::hash("ir", "intro-1", b"body-v1");
        let b = ContentKey::hash("ir", "intro-2", b"body-v2");
        let b_changed = ContentKey::hash("ir", "intro-2", b"body-v3");
        let c_gone = ContentKey::hash("ir", "intro-3", b"bye");

        let delta = content_delta(&[a.clone(), b.clone(), c_gone.clone()], &[a2, b_changed]);
        assert_eq!(delta.added.len(), 0);
        assert_eq!(delta.changed.len(), 1);
        assert_eq!(delta.changed[0].id.as_str(), "intro-2");
        assert_eq!(delta.removed.len(), 1);
        assert_eq!(delta.removed[0].id.as_str(), "intro-3");
    }

    /// Differential: mapping then measuring length equals measuring then
    /// counting — catches accidental drops inside `map`.
    #[test]
    fn delta_map_preserves_cardinality() {
        let delta = Delta {
            added: vec![1, 2],
            changed: vec![3],
            removed: vec![4, 5, 6],
        };
        let before = delta.len();
        let mapped = delta.map(|n| n * 10);
        assert_eq!(mapped.len(), before);
        assert_eq!(mapped.added, vec![10, 20]);
    }
}
