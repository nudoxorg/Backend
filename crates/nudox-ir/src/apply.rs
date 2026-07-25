//! [`PristineIntroTable`] — the materialized IR of one package at a channel
//! tip.
//!
//! This is a pure **container**: a map of live [`Entry`] values keyed by
//! [`IntroId`], plus parent/children tracking. It is *not* a change engine —
//! libpijul (via `nudox-ir-vcs`) owns changes, dependencies, apply, unrecord,
//! and all provenance. A symbol absent from the table simply has no file at the
//! channel tip; there are no tombstones here.
//!
//! A future `seal` pass will populate this table from a libpijul output tree
//! and hand it to `nudox-ir-archive` for snapshotting.

use std::collections::HashMap;

use crate::{change::IntroId, entry::Entry};

// ---------------------------------------------------------------------------
// StoredEntry (private)
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct StoredEntry {
    entry: Entry,
    parent: Option<IntroId>,
}

// ---------------------------------------------------------------------------
// PristineIntroTable
// ---------------------------------------------------------------------------

/// The in-memory materialized IR of a single package channel: a map
/// `IntroId → Entry`, the parent edges, and the children index.
///
/// # Invariants
///
/// * `children` is always in sync with `map`: every `IntroId` that appears as a
///   `parent` in some `StoredEntry` has a corresponding entry in `children`
///   whose vec contains the child's `IntroId`. The root's entry is absent from
///   `children` if no child declared it as a parent.
/// * `len()` equals the number of entries inserted (overwriting an existing key
///   is an upsert — not separately counted).
#[derive(Debug, Default)]
pub struct PristineIntroTable {
    map: HashMap<IntroId, StoredEntry>,
    children: HashMap<IntroId, Vec<IntroId>>,
}

impl PristineIntroTable {
    /// Construct an empty table.
    pub fn new() -> Self {
        Self::default()
    }

    // -----------------------------------------------------------------------
    // Builders
    // -----------------------------------------------------------------------

    /// Insert a live entry keyed by its `IntroId`, recording its parent
    /// `IntroId`.
    ///
    /// The children index for `parent` (when `Some`) is updated immediately so
    /// that [`children_of`](Self::children_of) is always consistent.
    ///
    /// Calling this with an `intro` that is already present replaces the stored
    /// entry and updates the parent edge; the old child link is *not* removed
    /// from the previous parent's vec (idempotent insertions of the same parent
    /// are the common case — the `seal` pass inserts each intro exactly once).
    pub fn insert_live(&mut self, intro: IntroId, entry: Entry, parent: Option<IntroId>) {
        self.map.insert(intro, StoredEntry { entry, parent });
        if let Some(p) = parent {
            self.children.entry(p).or_default().push(intro);
        }
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

    /// Iterate over all live intros and their entries.
    pub fn iter(&self) -> impl Iterator<Item = (IntroId, &Entry)> {
        self.map.iter().map(|(id, s)| (*id, &s.entry))
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
        test_helpers::{entry, n, sym},
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

        let parent_entry = entry(sym("root"), n::root([]), Module);
        let child_entry = entry(sym("child"), n::root([]), Module);

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

    #[test]
    fn empty_table() {
        let t = PristineIntroTable::new();
        assert!(t.is_empty());
        assert_eq!(t.len(), 0);
        assert!(!t.contains(intro(1)));
        assert_eq!(t.iter().count(), 0);
    }
}
