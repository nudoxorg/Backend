//! Sorted, deduplicated reverse-posting lists.
//!
//! # What is a posting list?
//!
//! A posting list maps one key (`K`) to a sorted, deduplicated set of values
//! (`V`). It is the standard data structure for reverse indexes: given a target
//! symbol, answer "which entries reference it?" in O(log n) lookup + O(k)
//! iteration where k is the posting count.
//!
//! # Sort + dedup discipline
//!
//! Posting lists are finalized (sorted and deduplicated) once, at build time,
//! then made immutable. This matches the prior art in
//! `workspace/registry/graph/reverse_index.rs` and keeps query-time
//! allocations to zero — `get` returns `&[V]` directly.
//!
//! # Why `BTreeMap` for `K`
//!
//! `K = StableRef` and `K = KindDiscriminant` both implement `Ord`. A
//! `BTreeMap` gives us a sorted key space, which makes `PostingList` amenable
//! to range scans if a future query layer needs them, and keeps iteration
//! deterministic without an extra sort pass.

use std::collections::BTreeMap;

// ---------------------------------------------------------------------------
// PostingListBuilder
// ---------------------------------------------------------------------------

/// Accumulates postings before finalizing into a [`PostingList`].
///
/// Use this during index construction to push `(key, value)` pairs in any
/// order; call [`finish`] once when the IR walk is complete.
///
/// [`finish`]: PostingListBuilder::finish
pub struct PostingListBuilder<K: Ord, V: Ord> {
    /// Raw, unsorted accumulator. We sort+dedup at `finish` time rather than
    /// maintaining a sorted invariant during insertion, because insertions
    /// during IR walks are random-order and sorting once is cheaper than
    /// repeated binary-search inserts.
    raw: BTreeMap<K, Vec<V>>,
}

impl<K: Ord, V: Ord> PostingListBuilder<K, V> {
    /// Create an empty builder.
    pub fn new() -> Self {
        Self { raw: BTreeMap::new() }
    }

    /// Add one `(key, value)` pair.
    pub fn push(&mut self, key: K, value: V) {
        self.raw.entry(key).or_default().push(value);
    }

    /// Sort and deduplicate all posting lists, producing a finalized
    /// [`PostingList`].
    pub fn finish(mut self) -> PostingList<K, V> {
        for list in self.raw.values_mut() {
            list.sort_unstable();
            list.dedup();
        }
        PostingList { inner: self.raw }
    }
}

impl<K: Ord, V: Ord> Default for PostingListBuilder<K, V> {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// PostingList
// ---------------------------------------------------------------------------

/// An immutable reverse-posting index: `K → sorted, deduplicated &[V]`.
///
/// Built via [`PostingListBuilder::finish`]. All posting lists are sorted and
/// deduplicated at construction; thereafter `get` returns `&[V]` with no
/// allocation.
#[derive(Debug, Default)]
pub struct PostingList<K: Ord, V: Ord> {
    inner: BTreeMap<K, Vec<V>>,
}

impl<K: Ord, V: Ord> PostingList<K, V> {
    /// Construct an empty (read-only) posting list.
    ///
    /// Useful for the case where an index section has no entries at all.
    pub fn empty() -> Self {
        Self { inner: BTreeMap::new() }
    }

    /// Return the sorted, deduplicated postings for `key`.
    ///
    /// Returns `&[]` when `key` is not present — callers need not check for
    /// `None`.
    pub fn get(&self, key: &K) -> &[V] {
        self.inner.get(key).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Iterate all `(key, postings)` pairs in key order.
    pub fn iter(&self) -> impl Iterator<Item = (&K, &[V])> {
        self.inner.iter().map(|(k, v)| (k, v.as_slice()))
    }

    /// Number of distinct keys in this index.
    pub fn key_count(&self) -> usize {
        self.inner.len()
    }

    /// Total number of (key, value) pairs across all postings.
    pub fn total_postings(&self) -> usize {
        self.inner.values().map(Vec::len).sum()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use nudox_ir::change::{EcosystemId, IntroId, PackageLineageId, PackageName, StableRef};

    fn intro(n: u8) -> IntroId {
        IntroId::from_raw([n; 32])
    }

    fn sref(n: u8) -> StableRef {
        let lineage = PackageLineageId::new(
            EcosystemId::new("cargo"),
            PackageName::new("test"),
        );
        StableRef::new(lineage, intro(n))
    }

    #[test]
    fn empty_posting_returns_empty_slice() {
        let list: PostingList<StableRef, IntroId> = PostingList::empty();
        assert_eq!(list.get(&sref(1)), &[] as &[IntroId]);
    }

    #[test]
    fn build_and_lookup() {
        let mut b: PostingListBuilder<StableRef, IntroId> = PostingListBuilder::new();
        b.push(sref(1), intro(10));
        b.push(sref(1), intro(20));
        b.push(sref(2), intro(30));

        let list = b.finish();
        assert_eq!(list.get(&sref(1)), &[intro(10), intro(20)]);
        assert_eq!(list.get(&sref(2)), &[intro(30)]);
        assert_eq!(list.get(&sref(99)), &[] as &[IntroId]);
    }

    #[test]
    fn deduplication_on_finish() {
        let mut b: PostingListBuilder<StableRef, IntroId> = PostingListBuilder::new();
        // Push the same value three times.
        b.push(sref(1), intro(5));
        b.push(sref(1), intro(5));
        b.push(sref(1), intro(5));

        let list = b.finish();
        // Only one posting should survive dedup.
        assert_eq!(list.get(&sref(1)), &[intro(5)]);
    }

    #[test]
    fn sort_order_is_deterministic_regardless_of_insertion_order() {
        let mut b1: PostingListBuilder<StableRef, IntroId> = PostingListBuilder::new();
        b1.push(sref(1), intro(30));
        b1.push(sref(1), intro(10));
        b1.push(sref(1), intro(20));

        let mut b2: PostingListBuilder<StableRef, IntroId> = PostingListBuilder::new();
        b2.push(sref(1), intro(20));
        b2.push(sref(1), intro(30));
        b2.push(sref(1), intro(10));

        let l1 = b1.finish();
        let l2 = b2.finish();
        assert_eq!(l1.get(&sref(1)), l2.get(&sref(1)));
        // Sorted order: intro(10) < intro(20) < intro(30).
        assert_eq!(l1.get(&sref(1)), &[intro(10), intro(20), intro(30)]);
    }

    #[test]
    fn total_postings_counts_correctly() {
        let mut b: PostingListBuilder<StableRef, IntroId> = PostingListBuilder::new();
        b.push(sref(1), intro(1));
        b.push(sref(1), intro(2));
        b.push(sref(2), intro(3));
        let list = b.finish();
        assert_eq!(list.key_count(), 2);
        assert_eq!(list.total_postings(), 3);
    }
}
