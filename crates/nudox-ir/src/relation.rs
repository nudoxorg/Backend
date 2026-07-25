//! Extrinsic graph edges between IR entries — the *relations plane*.
//!
//! # What belongs here
//!
//! A [`Relation`] is an **extrinsic** fact: a directed edge discovered by
//! analysing source code that is NOT already represented as containment in the
//! [`PristineIntroTable`]'s parent/children tree. Concretely:
//!
//! * Call edges (`A` calls `B`)
//! * Import edges (`A` imports `B`)
//! * Type-reference edges (`A` references type `B`)
//! * Trait-implementation edges ("A implements B")  — future variant
//! * Re-export targets                               — future variant
//!
//! Fields, params, and variants **do not** belong here: they are *composition*
//! (containment), already represented by the parent/children tree. Do not model
//! structural containment as relations.
//!
//! # Directedness
//!
//! Relations are **directed**. Every [`ReferenceKind`] in the vocabulary is
//! inherently asymmetric — "A calls B" is not the same as "B calls A"; "A
//! imports B" is meaningless when reversed. There are no symmetric reference
//! kinds in the current vocabulary, so an undirected design would be dishonest
//! about the data while adding indirection complexity for no gain. The
//! canonical dedupe key therefore includes the direction: `(from, to, kind)`.
//!
//! # KindDiscriminant omission
//!
//! The old `workspace/ir` `LinkRecord` carried `kind_a` / `kind_b`
//! [`KindDiscriminant`](crate::kind::KindDiscriminant) values — the structural
//! kind (Module, Function, …) of each endpoint. Those fields are **not**
//! present on [`Relation`] for two reasons:
//!
//! 1. For a same-package [`RelEnd::Intro`] endpoint, the kind is always
//!    retrievable from [`PristineIntroTable::get`] at zero additional storage
//!    cost — the table is always in scope when relations are evaluated.
//! 2. For a cross-package [`RelEnd::Foreign`] endpoint, the kind *is* unknown
//!    locally, but storing it here creates a consistency hazard: a relation's
//!    cached discriminant could silently disagree with the actual entry if the
//!    foreign package is updated. Omitting it forces callers to resolve against
//!    the authoritative source when they need structural information.
//!
//! If a future consumer proves that eager caching of the discriminant is a
//! measurable hot path, add it then with an explicit invariant comment.
//!
//! # Dedupe and merge semantics
//!
//! Two [`Relation`] values represent the same **logical edge** when their
//! `(from, to, kind)` triple matches (the [`RelationKey`]). Inserting the same
//! logical edge twice is expected — a tree-sitter pass and an oracle pass may
//! both discover "A calls B". The resolution rule (§5.1 merge rule 3) is:
//!
//! * Keep the **higher** [`Confidence`].
//! * When confidence improves, adopt the winner's [`RelSpan`] (or `None` if the
//!   higher-confidence fact has no span — span-less oracle facts happen).
//!
//! This means insert is idempotent on equal-confidence re-insertion and
//! monotonically improving on higher-confidence re-insertion.
//!
//! # Deterministic iteration order
//!
//! [`RelationSet::iter`] yields relations **sorted by [`RelationKey`]**
//! (lexicographic on `(from, to, kind)` using the canonical byte encoding).
//! This guarantee is not insertion-order; it is a stable total order over the
//! key space. The golden byte-pin harness in `crates/nudox-ir/tests/golden.rs`
//! relies on this — non-determinism would cause flapping.
//!
//! Internally the primary store is an [`IndexMap`] keyed by [`RelationKey`]
//! (which gives O(1) dedup) while sorted iteration is O(n log n) via a
//! temporary sort of the key set. Secondary indices for forward/reverse lookup
//! are plain [`HashMap`]s from endpoint key to `Vec<RelationKey>`.
//!
//! Complexity summary:
//! * `insert_relation`: O(1) amortised (hash lookup + possible vec push on
//!   secondary indices)
//! * `relations_from` / `relations_to`: O(k) where k is the number of edges
//!   touching that endpoint — **no full scan**
//! * `iter_sorted`: O(n log n) for the sort, O(n) iteration thereafter

use std::collections::HashMap;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::{
    change::{IntroId, StableRef},
    vocab::{Confidence, ReferenceKind, RelSpan},
};

// ---------------------------------------------------------------------------
// RelEnd — an endpoint of a relation
// ---------------------------------------------------------------------------

/// One endpoint of a directed [`Relation`].
///
/// An endpoint is either a same-package symbol (identified by its
/// content-addressed [`IntroId`]) or a symbol from another package
/// (identified by its globally stable [`StableRef`]).
///
/// # Canonical key bytes
///
/// Used inside [`RelationKey`]'s BLAKE3 preimage:
/// * `Intro(id)`:   `[0x00]  || id.as_bytes()` (33 bytes)
/// * `Foreign(sr)`: `[0x01]  || sr.canonical_bytes()` (variable)
///
/// The discriminant byte ensures `Intro` and `Foreign` endpoints cannot
/// collide even if their payload bytes happen to match.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum RelEnd {
    /// A symbol introduced in the same package; keyed by its [`IntroId`].
    Intro(IntroId),
    /// A symbol from another package; keyed by its cross-package [`StableRef`].
    Foreign(StableRef),
}

impl RelEnd {
    /// Append canonical, endian-stable bytes to `out` for use in hash
    /// preimages.
    pub fn encode(&self, out: &mut Vec<u8>) {
        match self {
            RelEnd::Intro(id) => {
                out.push(0x00);
                out.extend_from_slice(id.as_bytes());
            }
            RelEnd::Foreign(sr) => {
                out.push(0x01);
                sr.encode(out);
            }
        }
    }

    /// Canonical bytes as an owned buffer (delegates to [`Self::encode`]).
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode(&mut out);
        out
    }
}

// ---------------------------------------------------------------------------
// RelationKey — the dedupe / ordering key
// ---------------------------------------------------------------------------

/// The identity key of a [`Relation`]: `(from, to, kind)`.
///
/// Two relations with the same key are the same **logical edge**; the table
/// deduplicates on this key, keeping the higher-[`Confidence`] fact.
///
/// `RelationKey` is [`Ord`] via lexicographic comparison of its canonical
/// BLAKE3 preimage bytes — this is the total order that
/// [`RelationSet::iter_sorted`] exposes and that downstream
/// snapshot/serialization harnesses pin against.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct RelationKey {
    /// Canonical byte encoding of `(from || to || kind_u8)`.
    ///
    /// Kept as raw bytes rather than hashing so that the key is still readable
    /// in debug output and to avoid the loss of information that hashing would
    /// cause. The bytes are produced by [`RelationKey::from_parts`].
    bytes: Vec<u8>,
}

impl RelationKey {
    /// Build the canonical key from its parts.
    ///
    /// Layout: `from_bytes || to_bytes || [kind_discriminant as u8]`
    pub fn from_parts(from: &RelEnd, to: &RelEnd, kind: ReferenceKind) -> Self {
        let mut bytes = Vec::new();
        from.encode(&mut bytes);
        to.encode(&mut bytes);
        bytes.push(kind as u8);
        Self { bytes }
    }
}

impl PartialOrd for RelationKey {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for RelationKey {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.bytes.cmp(&other.bytes)
    }
}

// ---------------------------------------------------------------------------
// Relation
// ---------------------------------------------------------------------------

/// A directed, extrinsic graph edge between two [`RelEnd`] endpoints.
///
/// See the [module-level docs](self) for the design rationale, directedness
/// decision, `KindDiscriminant` omission, and dedupe semantics.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Relation {
    /// The source endpoint of the directed edge.
    pub from: RelEnd,
    /// The target endpoint of the directed edge.
    pub to: RelEnd,
    /// The category of the reference (what kind of edge this is).
    pub kind: ReferenceKind,
    /// The fidelity of the resolution that produced this edge.
    ///
    /// On duplicate insertion, the higher confidence wins (§5.1 merge rule 3).
    pub confidence: Confidence,
    /// The source-text site of the reference, if known.
    ///
    /// Relative to the `from` entry's span start. `None` for oracle-derived
    /// facts that have no textual anchoring.
    pub span: Option<RelSpan>,
}

impl Relation {
    /// Construct a new relation.
    pub fn new(
        from: RelEnd,
        to: RelEnd,
        kind: ReferenceKind,
        confidence: Confidence,
        span: Option<RelSpan>,
    ) -> Self {
        Self {
            from,
            to,
            kind,
            confidence,
            span,
        }
    }

    /// The dedupe key for this relation: `(from, to, kind)`.
    pub fn key(&self) -> RelationKey {
        RelationKey::from_parts(&self.from, &self.to, self.kind)
    }
}

// ---------------------------------------------------------------------------
// RelationSet — the relation store
// ---------------------------------------------------------------------------

/// The relations store for a single package's [`PristineIntroTable`].
///
/// Stores directed, deduplicated extrinsic edges between [`RelEnd`] endpoints.
///
/// # Invariants
///
/// * Every key in `primary` also appears in exactly one entry of `forward`
///   (keyed by the relation's `from` endpoint canonical bytes) and exactly one
///   entry of `reverse` (keyed by the relation's `to` endpoint canonical
///   bytes).
/// * No two entries in `primary` share the same [`RelationKey`] — the table
///   deduplicates, keeping the highest-[`Confidence`] fact.
/// * `forward` and `reverse` index vecs may contain duplicates only if the same
///   `RelationKey` is re-inserted with higher confidence (the index is not
///   updated on confidence-only upgrades, because the key is unchanged).
#[derive(Debug, Default)]
pub struct RelationSet {
    /// Primary store: `RelationKey → Relation`, in insertion order.
    ///
    /// `IndexMap` gives O(1) key lookup (hash) plus stable slot indices; we do
    /// NOT iterate it directly for deterministic output — see `iter_sorted`.
    primary: IndexMap<RelationKey, Relation>,

    /// Forward index: endpoint canonical bytes → keys of relations *from* that
    /// endpoint.
    forward: HashMap<Vec<u8>, Vec<RelationKey>>,

    /// Reverse index: endpoint canonical bytes → keys of relations *to* that
    /// endpoint.
    reverse: HashMap<Vec<u8>, Vec<RelationKey>>,
}

impl RelationSet {
    /// Construct an empty set.
    pub fn new() -> Self {
        Self::default()
    }

    // -----------------------------------------------------------------------
    // Mutation
    // -----------------------------------------------------------------------

    /// Insert a relation, deduplicating on `(from, to, kind)`.
    ///
    /// If a relation with the same logical key already exists:
    /// * If the new `confidence` is **higher**, replace the stored fact (adopt
    ///   new confidence and new span) — §5.1 merge rule 3.
    /// * If the new `confidence` is **equal or lower**, leave the stored fact
    ///   untouched (already the best known information).
    ///
    /// The secondary indices (`forward`, `reverse`) are only updated on the
    /// first insertion of a given key; confidence-only upgrades do not disturb
    /// them (the key is unchanged, so the index remains valid).
    pub fn insert(&mut self, rel: Relation) {
        let key = rel.key();

        // Check whether a relation with this key already exists.
        if let Some(existing) = self.primary.get_mut(&key) {
            // Only upgrade when the new fact is strictly better.
            if rel.confidence > existing.confidence {
                existing.confidence = rel.confidence;
                existing.span = rel.span;
            }
            // Key-equal, same or lower confidence: no change.
            return;
        }

        // New key: update secondary indices before moving `rel` into primary.
        let from_key = rel.from.canonical_bytes();
        let to_key = rel.to.canonical_bytes();

        self.forward.entry(from_key).or_default().push(key.clone());
        self.reverse.entry(to_key).or_default().push(key.clone());

        self.primary.insert(key, rel);
    }

    // -----------------------------------------------------------------------
    // Read API
    // -----------------------------------------------------------------------

    /// Number of distinct logical edges stored.
    pub fn len(&self) -> usize {
        self.primary.len()
    }

    /// True if there are no stored relations.
    pub fn is_empty(&self) -> bool {
        self.primary.is_empty()
    }

    /// Iterate all relations in **sorted** order (see module-level docs).
    ///
    /// Complexity: O(n log n) to build the sorted key list, then O(n)
    /// iteration. The sort materialises an owned `Vec<RelationKey>` of the
    /// primary map's keys; the returned iterator yields `&Relation` references
    /// that are valid for `'a`. The caller receives a `Vec`-backed iterator,
    /// not a lazy view, so all keys are cloned and all `&Relation` pointers
    /// are collected upfront. This is intentional: the alternative
    /// (`&RelationKey` borrows from a local `Vec`) cannot be returned as
    /// `impl Iterator` because the `Vec` would be dropped at function exit
    /// before the iterator is consumed.
    pub fn iter_sorted<'a>(&'a self) -> impl Iterator<Item = &'a Relation> + 'a {
        // Clone keys so the Vec can be owned and sorted independently of
        // `self.primary`'s internal storage.
        let mut keys: Vec<RelationKey> = self.primary.keys().cloned().collect();
        keys.sort_unstable();
        // Collect the final &'a Relation vec immediately; the local `keys` Vec
        // is consumed before this function returns — no dangling borrow.
        let sorted: Vec<&'a Relation> = keys
            .iter()
            .map(|k| self.primary.get(k).expect("key must exist in primary"))
            .collect();
        sorted.into_iter()
    }

    /// All relations (no ordering guarantee — use
    /// [`iter_sorted`](Self::iter_sorted) when determinism is required).
    pub fn iter_unordered<'a>(&'a self) -> impl Iterator<Item = &'a Relation> + 'a {
        self.primary.values()
    }

    /// Relations whose `from` endpoint matches `end`.
    ///
    /// Complexity: O(k) where k is the fan-out from `end`; **no full scan**,
    /// and no allocation — the posting list is borrowed, not copied.
    pub fn from_endpoint<'a>(&'a self, end: &RelEnd) -> impl Iterator<Item = &'a Relation> + 'a {
        // `get` hands back a reference tied to `'a self`, not to the temporary
        // `key`, so the posting list can be borrowed straight through.
        let posting: &'a [RelationKey] = self
            .forward
            .get(&end.canonical_bytes())
            .map_or(&[], Vec::as_slice);
        posting.iter().filter_map(move |k| self.primary.get(k))
    }

    /// Relations whose `to` endpoint matches `end`.
    ///
    /// Complexity: O(k) where k is the fan-in to `end`; **no full scan**, and
    /// no allocation.
    pub fn to_endpoint<'a>(&'a self, end: &RelEnd) -> impl Iterator<Item = &'a Relation> + 'a {
        let posting: &'a [RelationKey] = self
            .reverse
            .get(&end.canonical_bytes())
            .map_or(&[], Vec::as_slice);
        posting.iter().filter_map(move |k| self.primary.get(k))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        change::{EcosystemId, IntroId, PackageLineageId, PackageName, StableRef},
        vocab::{Confidence, ReferenceKind, RelSpan},
    };

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn intro(byte: u8) -> IntroId {
        IntroId::from_raw([byte; 32])
    }

    fn foreign(byte: u8) -> StableRef {
        let lineage =
            PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("other-crate"));
        StableRef::new(lineage, intro(byte))
    }

    fn rel(
        from: RelEnd,
        to: RelEnd,
        kind: ReferenceKind,
        confidence: Confidence,
        span: Option<RelSpan>,
    ) -> Relation {
        Relation::new(from, to, kind, confidence, span)
    }

    // ── Test 1: insert and read back both directions ──────────────────────────

    #[test]
    fn insert_and_read_both_directions() {
        let mut set = RelationSet::new();

        let from_end = RelEnd::Intro(intro(1));
        let to_end = RelEnd::Intro(intro(2));

        let r = rel(
            from_end.clone(),
            to_end.clone(),
            ReferenceKind::FunctionCall,
            Confidence::Oracle,
            Some(RelSpan::new(0, 10)),
        );
        set.insert(r);

        assert_eq!(set.len(), 1);

        // Forward lookup
        let forward: Vec<&Relation> = set.from_endpoint(&from_end).collect();
        assert_eq!(forward.len(), 1);
        assert_eq!(forward[0].kind, ReferenceKind::FunctionCall);
        assert_eq!(forward[0].confidence, Confidence::Oracle);

        // Reverse lookup
        let reverse: Vec<&Relation> = set.to_endpoint(&to_end).collect();
        assert_eq!(reverse.len(), 1);
        assert_eq!(reverse[0].kind, ReferenceKind::FunctionCall);

        // Endpoints not involved yield nothing.
        assert_eq!(set.from_endpoint(&to_end).count(), 0);
        assert_eq!(set.to_endpoint(&from_end).count(), 0);
    }

    // ── Test 2: dedup — same logical relation inserted twice yields one ────────

    #[test]
    fn dedup_same_relation_twice() {
        let mut set = RelationSet::new();

        let from_end = RelEnd::Intro(intro(1));
        let to_end = RelEnd::Intro(intro(2));

        let r1 = rel(
            from_end.clone(),
            to_end.clone(),
            ReferenceKind::MethodCall,
            Confidence::Syntactic,
            Some(RelSpan::new(5, 15)),
        );
        let r2 = rel(
            from_end.clone(),
            to_end.clone(),
            ReferenceKind::MethodCall,
            Confidence::Syntactic, // same confidence
            Some(RelSpan::new(5, 15)),
        );

        set.insert(r1);
        set.insert(r2);

        // Must still be exactly one entry.
        assert_eq!(set.len(), 1);
        assert_eq!(set.from_endpoint(&from_end).count(), 1);
    }

    // ── Test 3: confidence upgrade replaces stored fact ───────────────────────

    #[test]
    fn dedup_higher_confidence_wins() {
        let mut set = RelationSet::new();

        let from_end = RelEnd::Intro(intro(3));
        let to_end = RelEnd::Intro(intro(4));

        // Insert low-confidence fact first.
        let low = rel(
            from_end.clone(),
            to_end.clone(),
            ReferenceKind::TypeReference,
            Confidence::Syntactic,
            Some(RelSpan::new(0, 5)),
        );
        set.insert(low);

        // Insert high-confidence fact with same key.
        let high = rel(
            from_end.clone(),
            to_end.clone(),
            ReferenceKind::TypeReference,
            Confidence::Oracle,
            None, // oracle fact may have no span
        );
        set.insert(high);

        // Still one entry, but confidence is now Oracle.
        assert_eq!(set.len(), 1);
        let stored: Vec<&Relation> = set.from_endpoint(&from_end).collect();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].confidence, Confidence::Oracle);
        assert_eq!(stored[0].span, None);

        // Now insert a lower confidence after: must not downgrade.
        let lower = rel(
            from_end.clone(),
            to_end.clone(),
            ReferenceKind::TypeReference,
            Confidence::Index,
            Some(RelSpan::new(10, 20)),
        );
        set.insert(lower);
        assert_eq!(set.len(), 1);
        let stored: Vec<&Relation> = set.from_endpoint(&from_end).collect();
        assert_eq!(stored[0].confidence, Confidence::Oracle); // unchanged
    }

    // ── Test 4: determinism — same edges in different insertion orders ─────────

    #[test]
    fn iteration_order_is_deterministic_regardless_of_insertion_order() {
        // Build the same set of three relations twice, in different orders.
        let ends = [
            (
                RelEnd::Intro(intro(10)),
                RelEnd::Intro(intro(20)),
                ReferenceKind::FunctionCall,
            ),
            (
                RelEnd::Intro(intro(30)),
                RelEnd::Intro(intro(40)),
                ReferenceKind::Import,
            ),
            (
                RelEnd::Intro(intro(50)),
                RelEnd::Intro(intro(60)),
                ReferenceKind::TypeReference,
            ),
        ];

        let make_rel = |(from, to, kind): (RelEnd, RelEnd, ReferenceKind)| {
            rel(from, to, kind, Confidence::Index, Some(RelSpan::new(0, 1)))
        };

        let mut set_a = RelationSet::new();
        for triple in ends.iter().cloned() {
            set_a.insert(make_rel(triple));
        }

        let mut set_b = RelationSet::new();
        for triple in ends.iter().cloned().rev() {
            set_b.insert(make_rel(triple));
        }

        // Sorted iteration must produce identical keys.
        let keys_a: Vec<RelationKey> = set_a.iter_sorted().map(|r| r.key()).collect();
        let keys_b: Vec<RelationKey> = set_b.iter_sorted().map(|r| r.key()).collect();
        assert_eq!(
            keys_a, keys_b,
            "sorted iteration must be insertion-order-independent"
        );
    }

    // ── Test 5: Foreign endpoint round-trip ───────────────────────────────────

    #[test]
    fn foreign_endpoint_roundtrip() {
        let mut set = RelationSet::new();

        let from_end = RelEnd::Intro(intro(0xaa));
        let to_end = RelEnd::Foreign(foreign(0xbb));

        let r = rel(
            from_end.clone(),
            to_end.clone(),
            ReferenceKind::Import,
            Confidence::Import,
            None,
        );
        set.insert(r);

        assert_eq!(set.len(), 1);

        let forward: Vec<&Relation> = set.from_endpoint(&from_end).collect();
        assert_eq!(forward.len(), 1);
        assert!(matches!(&forward[0].to, RelEnd::Foreign(_)));

        let reverse: Vec<&Relation> = set.to_endpoint(&to_end).collect();
        assert_eq!(reverse.len(), 1);
        assert!(matches!(&reverse[0].from, RelEnd::Intro(_)));

        // Serde round-trip: encode and decode the relation.
        let json = serde_json::to_string(&forward[0]).expect("serialize Relation");
        let back: Relation = serde_json::from_str(&json).expect("deserialize Relation");
        assert_eq!(
            *forward[0], back,
            "Relation serde round-trip must be identity"
        );
    }

    // ── Test 6: directedness — (A→B) and (B→A) are distinct edges ────────────

    #[test]
    fn directed_relation_ab_and_ba_are_distinct() {
        let mut set = RelationSet::new();

        let end_a = RelEnd::Intro(intro(0x11));
        let end_b = RelEnd::Intro(intro(0x22));

        // A → B
        set.insert(rel(
            end_a.clone(),
            end_b.clone(),
            ReferenceKind::FunctionCall,
            Confidence::Index,
            None,
        ));
        // B → A (reverse direction — same kind)
        set.insert(rel(
            end_b.clone(),
            end_a.clone(),
            ReferenceKind::FunctionCall,
            Confidence::Index,
            None,
        ));

        // Both directions must be stored as separate entries.
        assert_eq!(set.len(), 2);

        // Forward from A → only one result (to B)
        let from_a: Vec<&Relation> = set.from_endpoint(&end_a).collect();
        assert_eq!(from_a.len(), 1);
        assert_eq!(from_a[0].to, end_b);

        // Forward from B → only one result (to A)
        let from_b: Vec<&Relation> = set.from_endpoint(&end_b).collect();
        assert_eq!(from_b.len(), 1);
        assert_eq!(from_b[0].to, end_a);
    }
}
