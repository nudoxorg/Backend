//! Arena-local entry types: [`Entry`], [`EntryInner`], [`Node`], [`EntryArena`],
//! and the [`StringInterner`] that backs all in-arena string storage.
//!
//! # Design rationale
//!
//! Every [`Entry`] combines three orthogonal pieces:
//! 1. **[`Symbol`]** — the language-agnostic face (name, visibility, docs, span…).
//! 2. **[`Node`]** — the tree topology: optional parent + ordered children list.
//! 3. **[`EntryInner`]** — the kind body: either a rich in-memory [`Kind`] or a
//!    forwarding reference to another entry's [`crate::index::RawEntryIdx`].
//!
//! Strings are stored as compact [`StrId`] handles through [`StringInterner`];
//! the interner is owned by [`EntryArena`] so it can be consulted alongside
//! entries.

use std::collections::HashMap;
use std::collections::HashSet;

use crate::index::{ArenaIdx, RawEntryIdx, StrId};
use crate::kind::Kind;
use crate::symbol::Symbol;

// ---------------------------------------------------------------------------
// Type alias
// ---------------------------------------------------------------------------

/// Shorthand for boxed slices used in arena structures.
type List<T> = Box<[T]>;

// ---------------------------------------------------------------------------
// Node
// ---------------------------------------------------------------------------

/// Tree topology of an arena entry: its parent (if any) and ordered children.
///
/// The parent is `None` for root-level entries (e.g. top-level modules).
/// Children are stored in declaration order.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Node {
    /// The parent entry, or `None` for root entries.
    pub parent: Option<RawEntryIdx>,
    /// Ordered list of direct children.
    pub children: List<RawEntryIdx>,
}

impl Node {
    /// Construct a leaf node with the given parent and no children.
    #[inline]
    pub fn leaf(parent: Option<RawEntryIdx>) -> Self {
        Self { parent, children: Box::new([]) }
    }

    /// Construct a node with a parent and children.
    #[inline]
    pub fn with_children(parent: Option<RawEntryIdx>, children: Vec<RawEntryIdx>) -> Self {
        Self { parent, children: children.into_boxed_slice() }
    }
}

// ---------------------------------------------------------------------------
// EntryInner
// ---------------------------------------------------------------------------

/// The kind body of an arena entry.
///
/// - `Owned(Kind)` — a fully described entry with a rich in-memory kind body.
/// - `Reference(RawEntryIdx)` — a forwarding alias / re-export that defers all
///   type information to the target entry.
///
/// Cross-package references are represented via [`crate::wire::ReferencePayload`]
/// at the payload level; within-arena references use this variant.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum EntryInner {
    /// An entry with a complete kind body.
    Owned(Kind),
    /// A re-export or alias that points to another arena entry.
    Reference(RawEntryIdx),
}

// ---------------------------------------------------------------------------
// Entry
// ---------------------------------------------------------------------------

/// One arena slot: symbol metadata + tree position + kind body.
///
/// Produced by the [`crate::builder::EntryBuilder`] and stored in
/// [`EntryArena`]. Arena indices (`ArenaIdx`) are valid only for the arena
/// that owns this entry; cross-session identity is carried by
/// [`nudox_change::IntroId`].
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Entry {
    /// Language-agnostic symbol metadata.
    pub sym: Symbol,
    /// Tree topology (parent / children).
    pub node: Node,
    /// Kind body.
    pub kind: EntryInner,
}

// ---------------------------------------------------------------------------
// StringInterner
// ---------------------------------------------------------------------------

/// A deduplicated string pool for a single [`EntryArena`].
///
/// Strings are stored once; duplicate inserts return the existing [`StrId`].
/// IDs are 0-indexed (position in `strings`).
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct StringInterner {
    strings: Vec<String>,
    map: HashMap<String, StrId>,
}

impl StringInterner {
    /// Construct an empty interner.
    pub fn new() -> Self {
        Self::default()
    }

    /// Intern `s`, returning an existing [`StrId`] if already present.
    pub fn intern(&mut self, s: &str) -> StrId {
        if let Some(&id) = self.map.get(s) {
            return id;
        }
        let id = StrId(u32::try_from(self.strings.len()).expect("string interner overflow"));
        self.strings.push(s.to_owned());
        self.map.insert(s.to_owned(), id);
        id
    }

    /// Resolve a [`StrId`] back to a `&str`, or `None` if the ID is invalid.
    #[inline]
    pub fn resolve(&self, id: StrId) -> Option<&str> {
        self.strings.get(id.0 as usize).map(String::as_str)
    }

    /// Number of interned strings.
    #[inline]
    pub fn len(&self) -> usize {
        self.strings.len()
    }

    /// True if no strings have been interned.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.strings.is_empty()
    }
}

// ---------------------------------------------------------------------------
// EntryArena
// ---------------------------------------------------------------------------

/// A contiguous arena of [`Entry`] values for one package, paired with the
/// string interner that backs all `StrId` handles inside those entries.
///
/// Entries are appended in declaration order via [`EntryArena::push`] and
/// addressed by [`ArenaIdx`]. The arena never removes entries once pushed;
/// logical deletion is tracked at the [`crate::apply::PristineIntroTable`]
/// level.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct EntryArena {
    entries: Vec<Entry>,
    /// The shared string pool for all entries in this arena.
    pub strings: StringInterner,
}

impl EntryArena {
    /// Construct an empty arena.
    pub fn new() -> Self {
        Self::default()
    }

    /// Append an entry and return its index.
    pub fn push(&mut self, entry: Entry) -> ArenaIdx {
        let idx = ArenaIdx(u32::try_from(self.entries.len()).expect("arena index overflow"));
        self.entries.push(entry);
        idx
    }

    /// Look up an entry by index.
    #[inline]
    pub fn get(&self, idx: ArenaIdx) -> Option<&Entry> {
        self.entries.get(idx.0 as usize)
    }

    /// Mutable look-up of an entry by index.
    #[inline]
    pub fn get_mut(&mut self, idx: ArenaIdx) -> Option<&mut Entry> {
        self.entries.get_mut(idx.0 as usize)
    }

    /// Number of entries in the arena.
    #[inline]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True if the arena has no entries.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Visit every entry in the subtree rooted at `root` in **pre-order**
    /// (root, then each child's subtree depth-first).
    ///
    /// Cycles (which should not appear in a well-formed arena) are detected via a
    /// `visited` set; any already-visited node is silently skipped.
    pub fn walk_preorder<F: FnMut(ArenaIdx, &Entry)>(&self, root: ArenaIdx, f: &mut F) {
        let mut visited = HashSet::new();
        self.walk_preorder_inner(root, f, &mut visited);
    }

    fn walk_preorder_inner<F: FnMut(ArenaIdx, &Entry)>(
        &self,
        idx: ArenaIdx,
        f: &mut F,
        visited: &mut HashSet<u32>,
    ) {
        if !visited.insert(idx.0) {
            return; // cycle guard
        }
        if let Some(entry) = self.get(idx) {
            f(idx, entry);
            // Clone to avoid borrow conflict with `self`.
            let children: Vec<RawEntryIdx> = entry.node.children.to_vec();
            for child in children {
                self.walk_preorder_inner(child.arena_idx, f, visited);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::PackageIdx;
    use crate::kind::Kind;
    use crate::symbol::{ByteSpan, Visibility};

    fn make_entry(arena: &mut EntryArena, name: &str, parent: Option<RawEntryIdx>) -> ArenaIdx {
        let sym = Symbol {
            name: arena.strings.intern(name),
            visibility: Visibility::Public,
            documentation: None,
            source_path: arena.strings.intern("src/lib.rs"),
            span: ByteSpan::ZERO,
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
        };
        let node = Node::leaf(parent);
        let entry = Entry { sym, node, kind: EntryInner::Owned(Kind::Module) };
        arena.push(entry)
    }

    #[test]
    fn interner_deduplicates() {
        let mut interner = StringInterner::new();
        let a = interner.intern("hello");
        let b = interner.intern("hello");
        assert_eq!(a, b);
        assert_eq!(interner.len(), 1);
    }

    #[test]
    fn arena_push_and_get() {
        let mut arena = EntryArena::new();
        let idx = make_entry(&mut arena, "root", None);
        assert_eq!(idx, ArenaIdx(0));
        assert!(arena.get(idx).is_some());
        assert!(arena.get(ArenaIdx(999)).is_none());
    }

    #[test]
    fn walk_preorder_visits_all() {
        let mut arena = EntryArena::new();
        let root = make_entry(&mut arena, "root", None);
        // We need to patch children manually for this test.
        let pkg_idx = PackageIdx(0);
        let child_idx = make_entry(&mut arena, "child", Some(crate::index::RawEntryIdx::new(pkg_idx, root)));
        // Patch root's children.
        arena.entries[root.0 as usize].node.children =
            vec![crate::index::RawEntryIdx::new(pkg_idx, child_idx)].into_boxed_slice();

        let mut visited = Vec::new();
        arena.walk_preorder(root, &mut |idx, _entry| {
            visited.push(idx);
        });
        assert_eq!(visited, vec![root, child_idx]);
    }

    #[test]
    fn walk_preorder_cycle_safe() {
        let mut arena = EntryArena::new();
        let root = make_entry(&mut arena, "root", None);
        let pkg_idx = PackageIdx(0);
        // Create a self-cycle.
        arena.entries[root.0 as usize].node.children =
            vec![crate::index::RawEntryIdx::new(pkg_idx, root)].into_boxed_slice();

        let mut count = 0usize;
        arena.walk_preorder(root, &mut |_, _| { count += 1; });
        // Root visited exactly once despite the cycle.
        assert_eq!(count, 1);
    }
}
