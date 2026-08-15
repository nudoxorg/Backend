//! Case-folded name index for prefix and fuzzy symbol search.
//!
//! # Design invariants
//!
//! * **Lowercased keys, original values.** Every entry is stored under its
//!   Unicode-lowercased name so that `"Vec"` and `"vec"` hit the same bucket.
//!   The original (display) name is kept alongside so that `NameEntry::display`
//!   can be shown in the UI without re-querying the `IrView`.
//!
//! * **Multiple entries per key.** Two types named `Foo` in different modules
//!   (e.g. `io::Error` and `fmt::Error`) both land under the key `"foo"`.
//!   The value is a `Vec<NameEntry>` sorted by `IntroId` for determinism.
//!
//!   That sortedness is maintained by [`NameIndex::insert`] itself (an ordered
//!   insert at the binary-searched position), not by a sort pass some caller
//!   has to remember to run. It used to be the latter — except no such pass
//!   existed, so the sentence above was false and bucket order was whatever
//!   order the caller's `HashMap` walk happened to produce, which varies per
//!   process. A documented invariant that only holds if every builder
//!   cooperates is not an invariant; this one now holds after any sequence of
//!   `insert` calls, so no builder can get it wrong.
//!
//! * **Prefix lookup in O(k log n).** `prefix` iterates the underlying
//!   `BTreeMap` from the first matching key, which is a standard range scan.
//!   This avoids a trie crate dependency while keeping the operation cheap
//!   enough for the GUI's search-as-you-type path.
//!
//! * **Fuzzy-ready.** The lowercased key is the canonical form; callers doing
//!   fuzzy matching compute their edit distance against `entry.key` rather than
//!   `entry.display`, avoiding repeated lowercasing in a hot loop.

use std::collections::BTreeMap;

use nudox_ir::change::IntroId;

// ---------------------------------------------------------------------------
// NameEntry
// ---------------------------------------------------------------------------

/// One symbol registered in the [`NameIndex`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameEntry {
    /// The case-folded key used for prefix/fuzzy matching.
    ///
    /// Invariant: `key == display.to_lowercase()`.
    pub key: String,
    /// The original symbol name as declared in source (for display only).
    pub display: String,
    /// The introduction that owns this name.
    pub intro: IntroId,
}

// ---------------------------------------------------------------------------
// NameIndex
// ---------------------------------------------------------------------------

/// A case-folded map from lowercased symbol names to their introductions.
///
/// Built once from a sealed `IrView`; thereafter read-only.
#[derive(Debug, Default)]
pub struct NameIndex {
    /// BTreeMap key = lowercased name; value = sorted, deduplicated list of
    /// entries that share that lowercased name.
    ///
    /// A `BTreeMap` is used instead of a `HashMap` because `prefix` relies on
    /// the sorted key order for its range scan.
    inner: BTreeMap<String, Vec<NameEntry>>,
}

impl NameIndex {
    /// Construct an empty index.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert one name → intro mapping, keeping the bucket sorted by
    /// [`IntroId`].
    ///
    /// Duplicate `(key, intro)` pairs are silently ignored; the same intro
    /// will not appear twice under the same lowercased key.
    ///
    /// The ordered insert is what makes the type's "sorted by `IntroId`"
    /// invariant unconditional: the resulting bucket is a pure function of the
    /// *set* of inserted intros, never of the order they arrived in. It also
    /// costs less than the linear dedup scan it replaced — `binary_search_by_key`
    /// answers "is this intro already here?" and "where does it go?" at once.
    pub fn insert(&mut self, display: impl Into<String>, intro: IntroId) {
        let display = display.into();
        let key = display.to_lowercase();
        let entries = self.inner.entry(key.clone()).or_default();

        // `Err(pos)` means the intro is absent and `pos` is its sorted home;
        // `Ok(_)` means it is already present, which is the dedup case.
        if let Err(pos) = entries.binary_search_by_key(&intro, |e| e.intro) {
            entries.insert(
                pos,
                NameEntry {
                    key,
                    display,
                    intro,
                },
            );
        }
    }

    /// Look up all entries whose lowercased name equals `name_lower`.
    ///
    /// The input is assumed to already be lowercased; callers must fold before
    /// calling this method.
    pub fn get_exact(&self, name_lower: &str) -> &[NameEntry] {
        self.inner.get(name_lower).map_or(&[], Vec::as_slice)
    }

    /// Iterate all entries whose lowercased name starts with `prefix_lower`.
    ///
    /// Results are yielded in BTreeMap order (lexicographic by key), then by
    /// [`IntroId`] within a key, and are flattened: each yielded `&NameEntry`
    /// is one symbol, not one key. Both levels of that order are total and
    /// content-derived, so a caller that truncates the stream (search's
    /// per-query `limit`) cuts the same set every run.
    ///
    /// The `prefix_lower` input must already be lowercased.
    pub fn prefix<'a>(&'a self, prefix_lower: &'a str) -> impl Iterator<Item = &'a NameEntry> + 'a {
        // BTreeMap::range gives us everything from `prefix_lower` onwards in
        // O(log n) to find the start, then O(k) to iterate the matching keys.
        // We stop as soon as the key no longer starts with our prefix.
        self.inner
            .range(prefix_lower.to_owned()..)
            .take_while(move |(key, _)| key.starts_with(prefix_lower))
            .flat_map(|(_, entries)| entries.iter())
    }

    /// Total number of unique (key, intro) pairs stored.
    pub fn len(&self) -> usize {
        self.inner.values().map(Vec::len).sum()
    }

    /// True if no names have been inserted.
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn intro(n: u8) -> IntroId {
        IntroId::from_raw([n; 32])
    }

    #[test]
    fn insert_and_exact_lookup() {
        let mut idx = NameIndex::new();
        idx.insert("Vec", intro(1));
        idx.insert("vec", intro(2)); // different display, same key — different intro

        let hits = idx.get_exact("vec");
        assert_eq!(hits.len(), 2);
        assert!(hits.iter().any(|e| e.display == "Vec"));
        assert!(hits.iter().any(|e| e.display == "vec"));
    }

    #[test]
    fn insert_deduplicates_same_intro_under_same_key() {
        let mut idx = NameIndex::new();
        idx.insert("Foo", intro(1));
        idx.insert("Foo", intro(1)); // duplicate
        assert_eq!(idx.get_exact("foo").len(), 1);
    }

    #[test]
    fn prefix_scan_matches_and_stops() {
        let mut idx = NameIndex::new();
        idx.insert("HashMap", intro(1));
        idx.insert("HashSet", intro(2));
        idx.insert("BTreeMap", intro(3));

        let results: Vec<_> = idx.prefix("hash").collect();
        assert_eq!(results.len(), 2);
        // BTreeMap order: "hashmap" before "hashset".
        assert_eq!(results[0].key, "hashmap");
        assert_eq!(results[1].key, "hashset");
    }

    #[test]
    fn prefix_empty_on_no_match() {
        let mut idx = NameIndex::new();
        idx.insert("String", intro(1));
        assert_eq!(idx.prefix("zzz").count(), 0);
    }

    #[test]
    fn len_counts_entries_not_keys() {
        let mut idx = NameIndex::new();
        idx.insert("Error", intro(1)); // key "error"
        idx.insert("Error", intro(2)); // same key, different intro
        idx.insert("Result", intro(3));
        assert_eq!(idx.len(), 3);
    }

    /// A bucket's order is a function of the *set* of intros in it, never of
    /// the order they were inserted.
    ///
    /// The module docs promise "sorted by `IntroId` for determinism". They
    /// promised it before this was true, too: `insert` appended, so a bucket
    /// came out in whatever order its builder walked its source — and the one
    /// builder that matters walks a `HashMap`, whose order changes with the
    /// process. That made the ranking of every same-named symbol a function of
    /// a per-process random seed.
    #[test]
    fn bucket_order_is_independent_of_insertion_order() {
        let ids = [intro(7), intro(3), intro(9), intro(1), intro(5)];

        let mut forward = NameIndex::new();
        for id in ids {
            forward.insert("Error", id);
        }

        let mut reversed = NameIndex::new();
        for id in ids.iter().rev() {
            reversed.insert("Error", *id);
        }

        let order = |idx: &NameIndex| -> Vec<IntroId> {
            idx.get_exact("error").iter().map(|e| e.intro).collect()
        };

        assert_eq!(
            order(&forward),
            vec![intro(1), intro(3), intro(5), intro(7), intro(9)],
            "a bucket must be sorted by IntroId"
        );
        assert_eq!(
            order(&forward),
            order(&reversed),
            "the same set of intros inserted in the opposite order produced a \
             different bucket order"
        );
    }

    /// Sortedness survives interleaved duplicate inserts.
    ///
    /// The dedup path and the ordered-insert path are the same
    /// `binary_search_by_key` call, so a duplicate must be a true no-op rather
    /// than something that shifts a neighbour.
    #[test]
    fn duplicate_inserts_do_not_disturb_bucket_order() {
        let mut idx = NameIndex::new();
        for id in [intro(4), intro(2), intro(4), intro(6), intro(2), intro(2)] {
            idx.insert("Foo", id);
        }
        let got: Vec<IntroId> = idx.get_exact("foo").iter().map(|e| e.intro).collect();
        assert_eq!(got, vec![intro(2), intro(4), intro(6)]);
    }

    /// The prefix scan inherits the bucket order, so a caller that truncates
    /// the stream cuts the same set every run.
    #[test]
    fn prefix_scan_is_ordered_by_key_then_intro() {
        let mut idx = NameIndex::new();
        // Insert in an order that matches neither the key order nor the id order.
        idx.insert("HashSet", intro(9));
        idx.insert("HashMap", intro(8));
        idx.insert("HashMap", intro(2));
        idx.insert("HashSet", intro(1));

        let got: Vec<(String, IntroId)> = idx
            .prefix("hash")
            .map(|e| (e.key.clone(), e.intro))
            .collect();
        assert_eq!(
            got,
            vec![
                ("hashmap".to_owned(), intro(2)),
                ("hashmap".to_owned(), intro(8)),
                ("hashset".to_owned(), intro(1)),
                ("hashset".to_owned(), intro(9)),
            ]
        );
    }

    #[test]
    fn case_folding_is_applied() {
        let mut idx = NameIndex::new();
        idx.insert("MyType", intro(1));
        // Exact lookup must use lowercased key.
        assert_eq!(idx.get_exact("mytype").len(), 1);
        assert_eq!(idx.get_exact("MyType").len(), 0); // key is always lowercase
    }
}
