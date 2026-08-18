//! Exact lookup from a re-export alias path to the declaration(s) it names.
//!
//! # Why this exists — the address scheme's Stage 2
//!
//! `Symbol.aliases` is populated by all seven language producers with the
//! `"::"`-joined public path a re-export makes a declaration reachable
//! under (`chunk::head`'s module docs, L18). It was previously read only by
//! `chunk::head::shortest_alias_module_path`, one entry at a time, to shorten
//! a single symbol's breadcrumb. Inverting it once at package-load time — the
//! same `entries_sorted` pass that already builds [`super::NameIndex`] and
//! the physical-path map in [`crate::store::package::PackageIndexes`] — turns
//! it into a cross-language `alias path → declaration` index for free
//! (docs/MCP-SURFACE-PLAN.md §4.4/§4.10).
//!
//! This is what lets an agent-typed *public* path like `serde::Deserializer`
//! resolve to the *physical* declaration `serde::de::Deserializer` without
//! touching name search or reviving the dead `Reexport` vertex (§4.10,
//! "Option A"): the alias is recorded on the definition's own `Symbol`, so
//! inverting it maps straight to the definition, never through a re-export
//! row.
//!
//! # Why exact, not prefix
//!
//! Unlike [`super::NameIndex`], this index is queried with a *complete*
//! candidate path (every segment of an agent-typed address, canonicalized to
//! `"::"`-joined), so a `BTreeMap` prefix scan would buy nothing a plain
//! `HashMap` doesn't already give in O(1). §4.10's Stage 3 (name-plan +
//! suffix filter) is the stage that wants partial matches; this one answers
//! "is this exact string a known alias" and nothing broader.

use std::collections::HashMap;

use nudox_ir::change::IntroId;

/// Exact `alias path → declarations` lookup, inverted from every indexed
/// declaration's `Symbol.aliases` at package-load time.
///
/// A `Vec` rather than a single `IntroId` per key because nothing prevents
/// two declarations from sharing a rendered alias string in principle (e.g. a
/// crate re-exporting two differently-cfg-gated items under the same public
/// name); callers that want "the" target should treat more than one hit as
/// requiring disambiguation, not silently take the first.
#[derive(Debug, Default)]
pub struct AliasIndex {
    inner: HashMap<String, Vec<IntroId>>,
}

impl AliasIndex {
    /// Construct an empty index.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that `alias_path` (already `"::"`-joined, exactly as the
    /// producer wrote it into `Symbol.aliases`) reaches `intro`.
    ///
    /// Duplicate `(alias_path, intro)` pairs are silently ignored, mirroring
    /// [`super::NameIndex::insert`]'s dedup contract.
    pub fn insert(&mut self, alias_path: impl Into<String>, intro: IntroId) {
        let entries = self.inner.entry(alias_path.into()).or_default();
        if !entries.contains(&intro) {
            entries.push(intro);
            entries.sort_unstable();
        }
    }

    /// The declarations reachable under exactly this alias path, if any.
    pub fn get_exact(&self, alias_path: &str) -> &[IntroId] {
        self.inner.get(alias_path).map_or(&[], Vec::as_slice)
    }

    /// Total number of distinct alias paths recorded.
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// True if no alias has been recorded.
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
        let mut idx = AliasIndex::new();
        idx.insert("serde::Deserializer", intro(1));
        assert_eq!(idx.get_exact("serde::Deserializer"), &[intro(1)]);
        assert!(idx.get_exact("serde::Serializer").is_empty());
    }

    #[test]
    fn duplicate_insert_is_a_no_op() {
        let mut idx = AliasIndex::new();
        idx.insert("a::B", intro(1));
        idx.insert("a::B", intro(1));
        assert_eq!(idx.get_exact("a::B").len(), 1);
    }

    #[test]
    fn two_declarations_can_share_one_alias_path() {
        let mut idx = AliasIndex::new();
        idx.insert("a::B", intro(1));
        idx.insert("a::B", intro(2));
        let mut hits = idx.get_exact("a::B").to_vec();
        hits.sort_unstable();
        assert_eq!(hits, vec![intro(1), intro(2)]);
    }

    #[test]
    fn empty_index_reports_empty() {
        let idx = AliasIndex::new();
        assert!(idx.is_empty());
        assert_eq!(idx.len(), 0);
    }
}
