//! [`PristineIntroTable`] — the materialized IR of one package at a channel
//! tip.
//!
//! This is a pure **container**: a map of live [`Entry`] values keyed by
//! [`IntroId`], plus parent/children tracking and the extrinsic
//! [`RelationSet`]. It is *not* a change engine — libpijul (via
//! `nudox-ir-vcs`) owns changes, dependencies, apply, unrecord, and all
//! provenance. A symbol absent from the table simply has no file at the
//! channel tip; there are no tombstones here.
//!
//! A future `seal` pass will populate this table from a libpijul output tree
//! and hand it to `nudox-ir-archive` for snapshotting.

use std::collections::HashMap;

use crate::{
    change::IntroId,
    entry::{Entry, Symbol},
    relation::{RelEnd, Relation, RelationSet},
};

// ---------------------------------------------------------------------------
// StoredEntry (private)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
struct StoredEntry {
    entry: Entry,
    parent: Option<IntroId>,
}

// ---------------------------------------------------------------------------
// IntroCollision
// ---------------------------------------------------------------------------

/// Two declarations claimed one [`IntroId`].
///
/// This is an identity failure, not an update — which is why
/// [`PristineIntroTable::try_insert_live`] refuses rather than replaces. Every
/// field is here so the caller can *act*: name both declarations, re-key the
/// rejected one, or report the pair to a producer author with file and span.
#[derive(Debug)]
pub struct IntroCollision {
    /// The identity both declarations minted.
    pub intro: IntroId,
    /// The symbol already live under `intro`. Carries name, source path, span
    /// and documentation — enough to find it in the source.
    pub incumbent: Box<Symbol>,
    /// The incumbent's parent edge.
    pub incumbent_parent: Option<IntroId>,
    /// The entry that was **not** inserted, returned intact because this is the
    /// only copy of it.
    pub rejected: Box<Entry>,
    /// The parent the rejected entry would have been filed under.
    pub rejected_parent: Option<IntroId>,
}

impl core::fmt::Display for IntroCollision {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "IntroId {} claimed by both `{}` ({}:{:?}) and `{}` ({}:{:?})",
            self.intro.to_hex(),
            self.incumbent.name,
            self.incumbent.source.display(),
            self.incumbent.span,
            self.rejected.sym().name,
            self.rejected.sym().source.display(),
            self.rejected.sym().span,
        )
    }
}

impl std::error::Error for IntroCollision {}

// ---------------------------------------------------------------------------
// PristineIntroTable
// ---------------------------------------------------------------------------

/// The in-memory materialized IR of a single package channel: a map
/// `IntroId → Entry`, the parent edges, the children index, and the extrinsic
/// [`RelationSet`].
///
/// # Invariants
///
/// * `children` is always in sync with `map`: every `IntroId` that appears as a
///   `parent` in some `StoredEntry` has a corresponding entry in `children`
///   whose vec contains the child's `IntroId`. The root's entry is absent from
///   `children` if no child declared it as a parent. This holds unconditionally
///   because insertion refuses to displace: there is no path that pushes a
///   child edge without also adding the entry.
/// * `len()` equals the number of entries successfully inserted. An `IntroId`
///   is claimed at most once — a second claim is
///   [`IntroCollision`](crate::apply::IntroCollision), never a silent upsert.
/// * `relations` contains only extrinsic edges (calls, imports, type
///   references, …). Structural containment (field/param/variant membership) is
///   represented exclusively by the parent/children tree above; it must never
///   appear in `relations`.
/// * Relations with [`RelEnd::Intro`] endpoints reference `IntroId`s that
///   *should* be present in `map` — this is an advisory invariant, not enforced
///   at runtime (the table receives relations from separate analysis passes
///   that may run before the full entry set is inserted).
/// * [`RelationSet`]'s own invariants (dedup, secondary-index consistency) are
///   maintained by [`RelationSet::insert`]; callers need not enforce them.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PristineIntroTable {
    map: HashMap<IntroId, StoredEntry>,
    children: HashMap<IntroId, Vec<IntroId>>,
    relations: RelationSet,
}

impl PristineIntroTable {
    /// Construct an empty table.
    pub fn new() -> Self {
        Self::default()
    }

    // -----------------------------------------------------------------------
    // Builders
    // -----------------------------------------------------------------------

    /// Insert a live entry the caller **guarantees** is under a fresh
    /// `IntroId`, recording its parent.
    ///
    /// The children index for `parent` (when `Some`) is updated immediately so
    /// that [`children_of`](Self::children_of) is always consistent.
    ///
    /// # Panics
    ///
    /// If `intro` is already live. Two declarations minting one `IntroId` is an
    /// identity failure, never an update: the second entry becomes unreachable
    /// and the first is gone. Use [`try_insert_live`](Self::try_insert_live)
    /// when a collision is a situation you must handle rather than a
    /// precondition you are asserting.
    ///
    /// # Why this panics rather than upserting
    ///
    /// It used to be a bare `HashMap::insert` whose displaced `StoredEntry` was
    /// dropped on the floor, under a doc comment asserting "the `seal` pass
    /// inserts each intro exactly once". That premise was false: 28 real Gson
    /// `Function` entries (~4.3% of the package) vanished between `finish` and
    /// the GUI, silently, and the only channel that could have reported it —
    /// the return value — was `()`.
    #[track_caller]
    pub fn insert_live(&mut self, intro: IntroId, entry: Entry, parent: Option<IntroId>) {
        if let Err(collision) = self.try_insert_live(intro, entry, parent) {
            panic!(
                "IntroId collision in PristineIntroTable: `{}` is already live as {:?} \
                 (at {}:{:?}); the rejected declaration is {:?} (at {}:{:?}). \
                 The caller of `insert_live` guarantees freshness — use \
                 `try_insert_live` if a collision is a case you must handle.",
                collision.intro.to_hex(),
                collision.incumbent.name,
                collision.incumbent.source.display(),
                collision.incumbent.span,
                collision.rejected.sym().name,
                collision.rejected.sym().source.display(),
                collision.rejected.sym().span,
            );
        }
    }

    /// Insert a live entry, **refusing** to displace an existing one.
    ///
    /// # Errors
    ///
    /// [`IntroCollision`] when `intro` is already live. The incumbent is kept
    /// and the rejected entry is handed back **by value** so no caller can lose
    /// it by accident — it is the only copy (`Entry` is moved in).
    ///
    /// Refusing also fixes a second, previously unreported corruption: the old
    /// implementation pushed into `children[parent]` even when it overwrote, so
    /// after a collision `children_of(parent)` held the same `IntroId` twice
    /// while `map` held one entry — silently violating this type's stated
    /// "children is always in sync with map" invariant. The push now happens
    /// only on the success path, so the desync is unrepresentable.
    pub fn try_insert_live(
        &mut self,
        intro: IntroId,
        entry: Entry,
        parent: Option<IntroId>,
    ) -> Result<(), IntroCollision> {
        if let Some(incumbent) = self.map.get(&intro) {
            return Err(IntroCollision {
                intro,
                incumbent: Box::new(incumbent.entry.sym().clone()),
                incumbent_parent: incumbent.parent,
                rejected: Box::new(entry),
                rejected_parent: parent,
            });
        }
        self.map.insert(intro, StoredEntry { entry, parent });
        if let Some(p) = parent {
            self.children.entry(p).or_default().push(intro);
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Relation builders
    // -----------------------------------------------------------------------

    /// Insert an extrinsic relation into the table, deduplicating on
    /// `(from, to, kind)`.
    ///
    /// Delegates to [`RelationSet::insert`]: re-inserting the same logical edge
    /// at equal confidence is a no-op; re-inserting at higher confidence
    /// upgrades the stored fact (§5.1 merge rule 3).
    ///
    /// This method does **not** validate that [`RelEnd::Intro`] endpoints
    /// exist in the entry map — relations may be inserted from a separate
    /// analysis pass before all entries are present.
    pub fn insert_relation(&mut self, rel: Relation) {
        self.relations.insert(rel);
    }

    // -----------------------------------------------------------------------
    // Relation read API
    // -----------------------------------------------------------------------

    /// Iterate all stored relations in deterministic sorted order.
    ///
    /// See [`RelationSet::iter_sorted`] for the ordering guarantee.
    /// Complexity: O(n log n).
    pub fn relations_sorted<'a>(&'a self) -> impl Iterator<Item = &'a Relation> + 'a {
        self.relations.iter_sorted()
    }

    /// Iterate relations in unspecified order (cheaper when determinism is not
    /// required).
    pub fn relations_unordered<'a>(&'a self) -> impl Iterator<Item = &'a Relation> + 'a {
        self.relations.iter_unordered()
    }

    /// Relations whose `from` endpoint matches `end`. O(k), no full scan.
    pub fn relations_from<'a>(&'a self, end: &RelEnd) -> impl Iterator<Item = &'a Relation> + 'a {
        self.relations.from_endpoint(end)
    }

    /// Relations whose `to` endpoint matches `end`. O(k), no full scan.
    pub fn relations_to<'a>(&'a self, end: &RelEnd) -> impl Iterator<Item = &'a Relation> + 'a {
        self.relations.to_endpoint(end)
    }

    /// Number of distinct extrinsic relations stored.
    pub fn relation_count(&self) -> usize {
        self.relations.len()
    }

    // -----------------------------------------------------------------------
    // Read API
    // -----------------------------------------------------------------------

    /// Look up a live entry by its `IntroId`. Returns `None` if absent.
    pub fn get(&self, intro: IntroId) -> Option<&Entry> {
        self.map.get(&intro).map(|s| &s.entry)
    }

    /// Return the parent `IntroId` of this intro, if any.
    ///
    /// Returns `None` when the intro is a root or is not present in the table.
    pub fn parent_of(&self, intro: IntroId) -> Option<IntroId> {
        self.map.get(&intro)?.parent
    }

    /// Return the slice of children `IntroId`s recorded for this parent.
    ///
    /// Returns `&[]` when the intro has no children or is not present.
    pub fn children_of(&self, intro: IntroId) -> &[IntroId] {
        self.children.get(&intro).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Number of live entries.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// True if there are no live entries.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// True if `intro` is present (live) in this table.
    pub fn contains(&self, intro: IntroId) -> bool {
        self.map.contains_key(&intro)
    }

    /// Iterate over all live intros and their entries in **unspecified** order.
    ///
    /// The backing store is a `HashMap`, so the yield order is a function of
    /// std's per-process `RandomState` seed: it differs between two runs of the
    /// same binary over the same table. Anything derived from this iterator that
    /// a user can *see* — a ranked list, a rendered page, a serialized artifact
    /// — must go through [`iter_sorted`](Self::iter_sorted) instead, or the
    /// seed reaches the UI.
    ///
    /// Use this only when the consumer is order-insensitive by construction
    /// (a count, a `HashSet`, a `BTreeMap` being filled, an `any`/`all` predicate).
    pub fn iter(&self) -> impl Iterator<Item = (IntroId, &Entry)> {
        self.map.iter().map(|(id, s)| (*id, &s.entry))
    }

    /// Iterate over all live intros and their entries in deterministic
    /// [`IntroId`] order.
    ///
    /// This is the [`relations_sorted`](Self::relations_sorted) /
    /// [`relations_unordered`](Self::relations_unordered) "pick your guarantee"
    /// pair applied to the entry map: `iter` is cheap and unordered, this one
    /// costs O(n log n) and is reproducible across processes.
    ///
    /// `IntroId` is a content hash of the declaration, so this order is a
    /// property of the *package*, not of the run that built the table — two
    /// processes that lower the same source agree on it.
    pub fn iter_sorted(&self) -> impl Iterator<Item = (IntroId, &Entry)> {
        let mut ids: Vec<IntroId> = self.map.keys().copied().collect();
        ids.sort_unstable();
        ids.into_iter()
            .map(move |id| (id, &self.map[&id].entry))
    }

    /// Alias for [`iter`](Self::iter) — iterate over all live intros and their
    /// entries in unspecified order. Named `live_entries` for compatibility with
    /// callers that use the old `workspace/ir` API.
    pub fn live_entries(&self) -> impl Iterator<Item = (IntroId, &Entry)> {
        self.iter()
    }

    /// True if `intro` is present (live) in this table.
    ///
    /// Alias for [`contains`](Self::contains) — named `is_live` for
    /// compatibility with callers that use the old `workspace/ir` API.
    pub fn is_live(&self, intro: IntroId) -> bool {
        self.contains(intro)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        kinds::Module,
        test_helpers::{entry, node, sym},
    };

    fn intro(byte: u8) -> IntroId {
        IntroId::from_raw([byte; 32])
    }

    #[test]
    fn parent_child_insert_and_read() {
        let mut t = PristineIntroTable::new();

        // Parent: a root module entry (no parent, no children yet).
        let parent_id = intro(1);
        let child_id = intro(2);

        let parent_entry = entry(sym("root"), node::root([]), Module);
        let child_entry = entry(sym("child"), node::root([]), Module);

        t.insert_live(parent_id, parent_entry, None);
        t.insert_live(child_id, child_entry, Some(parent_id));

        // len / contains
        assert_eq!(t.len(), 2);
        assert!(!t.is_empty());
        assert!(t.contains(parent_id));
        assert!(t.contains(child_id));
        assert!(!t.contains(intro(99)));

        // get returns entries
        assert!(t.get(parent_id).is_some());
        assert_eq!(t.get(parent_id).unwrap().sym().name, "root");
        assert!(t.get(child_id).is_some());
        assert_eq!(t.get(child_id).unwrap().sym().name, "child");
        assert!(t.get(intro(99)).is_none());

        // parent_of
        assert_eq!(t.parent_of(parent_id), None);
        assert_eq!(t.parent_of(child_id), Some(parent_id));

        // children_of
        let children = t.children_of(parent_id);
        assert_eq!(children.len(), 1);
        assert_eq!(children[0], child_id);
        assert_eq!(t.children_of(child_id), &[]);
        assert_eq!(t.children_of(intro(99)), &[]);

        // iter yields both entries
        let count = t.iter().count();
        assert_eq!(count, 2);
    }

    /// `iter_sorted` yields ascending `IntroId`s regardless of insertion order,
    /// and `iter` may not.
    ///
    /// The asymmetry is the point of having both. `iter` walks a `HashMap`, so
    /// its order is a per-process seed; anything a user can see must be built
    /// from `iter_sorted` instead. This test pins the guarantee, not the
    /// absence of one — it asserts nothing about `iter`'s order beyond the set
    /// of ids it covers, because `iter` promises nothing about it.
    #[test]
    fn iter_sorted_yields_intro_id_order_whatever_the_insertion_order() {
        let ids = [intro(7), intro(3), intro(9), intro(1), intro(5)];

        let build = |order: &dyn Fn(&mut Vec<IntroId>)| {
            let mut seq: Vec<IntroId> = ids.to_vec();
            order(&mut seq);
            let mut t = PristineIntroTable::new();
            for id in seq {
                t.insert_live(id, entry(sym("same"), node::root([]), Module), None);
            }
            t
        };

        let forward = build(&|_| {});
        let reversed = build(&|v| v.reverse());

        let expect = vec![intro(1), intro(3), intro(5), intro(7), intro(9)];
        assert_eq!(
            forward.iter_sorted().map(|(id, _)| id).collect::<Vec<_>>(),
            expect
        );
        assert_eq!(
            reversed.iter_sorted().map(|(id, _)| id).collect::<Vec<_>>(),
            expect,
            "iter_sorted must not depend on the order entries were inserted in"
        );

        // The sorted walk is over the same entries, not a subset: every id is
        // present and each yields the entry stored under it.
        assert_eq!(forward.iter_sorted().count(), forward.iter().count());
        for (id, e) in forward.iter_sorted() {
            assert_eq!(
                e as *const _,
                forward.get(id).expect("id came from the table") as *const _,
                "iter_sorted paired an id with an entry that is not its own"
            );
        }
    }

    #[test]
    fn empty_table() {
        let t = PristineIntroTable::new();
        assert!(t.is_empty());
        assert_eq!(t.len(), 0);
        assert!(!t.contains(intro(1)));
        assert_eq!(t.iter().count(), 0);
    }
}
