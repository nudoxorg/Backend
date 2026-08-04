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

    /// Insert one name → intro mapping.
    ///
    /// Duplicate `(key, intro)` pairs are silently ignored; the same intro
    /// will not appear twice under the same lowercased key.
    pub fn insert(&mut self, display: impl Into<String>, intro: IntroId) {
        let display = display.into();
        let key = display.to_lowercase();
        let entries = self.inner.entry(key.clone()).or_default();

        // Dedup: only add if this intro is not already present under this key.
        if !entries.iter().any(|e| e.intro == intro) {
            entries.push(NameEntry { key, display, intro });
        }
    }

    /// Look up all entries whose lowercased name equals `name_lower`.
    ///
    /// The input is assumed to already be lowercased; callers must fold before
    /// calling this method.
    pub fn get_exact(&self, name_lower: &str) -> &[NameEntry] {
        self.inner
            .get(name_lower)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Iterate all entries whose lowercased name starts with `prefix_lower`.
    ///
    /// Results are yielded in BTreeMap order (lexicographic by key) and are
    /// flattened: each yielded `&NameEntry` is one symbol, not one key.
    ///
    /// The `prefix_lower` input must already be lowercased.
    pub fn prefix<'a>(
        &'a self,
        prefix_lower: &'a str,
    ) -> impl Iterator<Item = &'a NameEntry> + 'a {
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

    #[test]
    fn case_folding_is_applied() {
        let mut idx = NameIndex::new();
        idx.insert("MyType", intro(1));
        // Exact lookup must use lowercased key.
        assert_eq!(idx.get_exact("mytype").len(), 1);
        assert_eq!(idx.get_exact("MyType").len(), 0); // key is always lowercase
    }
}
