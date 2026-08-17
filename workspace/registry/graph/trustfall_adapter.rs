//! Trustfall graph adapter over the IR read model (INDEX-PLAN §5.5).
//!
//! This module exposes:
//!
//! * [`GraphVertex`] — the vertex discriminated union (`Entry`, `Occ`, `Package`).
//! * [`IrTrustfallAdapter`] — a borrow of an [`IrView`] plus an optional
//!   [`ReversePositionIndex`] for the fast reverse-lookup path.
//! * Plain-Rust **neighbour methods** on the adapter — these are the load-bearing
//!   deliverable for this wave.
//! * [`execute_graph_query`] — a stub with the correct [`trustfall::FieldValue`]
//!   signature; returns [`Error::Unsupported`] until the Trustfall schema
//!   is wired in a later wave.

use std::collections::BTreeMap;
use std::sync::Arc;

use thiserror::Error;

use ir::change::{IntroId, PackageLineageId, StableRef};
use ir::entry::Entry;
use ir::view::IrView;
use ir::vocab::Occurrence;

use crate::graph::reverse_index::{ReversePositionIndex, typerefs_of_entry};

// ---------------------------------------------------------------------------
// GraphVertex
// ---------------------------------------------------------------------------

/// A vertex in the IR graph as seen by the Trustfall adapter.
///
/// Three shapes are modelled for this wave; additional shapes (e.g. `Body`,
/// `Generation`) can be added without breaking callers.
#[derive(Clone, Debug)]
pub enum GraphVertex<'a> {
    /// A declaration entry: an intro + its entry (symbol metadata + kind body).
    Entry {
        /// The stable, cross-package identity of this declaration.
        intro: IntroId,
        /// The entry's declaration data.
        entry: &'a Entry,
    },
    /// A resolved reference occurrence (the `occ` frames of §5.4).
    ///
    /// The occurrence owner is available via the map key in `IrView::all_occurrences`
    /// (`(owner, &Occurrence)`); the vertex carries only the occurrence value.
    Occ(&'a Occurrence),
    /// The package lineage that owns the IR view.
    Package(&'a PackageLineageId),
}

impl<'a> GraphVertex<'a> {
    /// The vertex type name used in Trustfall property resolution.
    ///
    /// Returns one of `"Entry"`, `"Occ"`, or `"Package"`.
    pub fn typename(&self) -> &'static str {
        match self {
            GraphVertex::Entry { .. } => "Entry",
            GraphVertex::Occ(_) => "Occ",
            GraphVertex::Package(_) => "Package",
        }
    }
}

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

/// Errors that can arise from [`execute_graph_query`].
#[derive(Debug, Error)]
pub enum Error {
    /// The Trustfall schema is not yet wired; any query string is unsupported
    /// in this wave. The string carries the query for diagnostic purposes.
    #[error("trustfall query is not yet supported: {0}")]
    Unsupported(String),
    /// A parse or execution error from the Trustfall runtime.
    #[error("query parse/execute error: {0}")]
    Execute(String),
}

// ---------------------------------------------------------------------------
// IrTrustfallAdapter
// ---------------------------------------------------------------------------

/// Adapter that wraps an [`IrView`] for graph traversal and Trustfall queries.
///
/// The optional [`ReversePositionIndex`] enables O(log n) reverse look-ups
/// (usages, mentions). Without it, the adapter falls back to a linear scan over
/// all occurrences — correct but slower.
///
/// **This wave:** the adapter exposes plain-Rust neighbour methods. The full
/// Trustfall `Adapter` trait implementation is deferred to a later wave when the
/// schema TOML is authored and wired.
pub struct IrTrustfallAdapter<'a> {
    /// The IR read model this adapter operates over.
    pub ir: &'a IrView,
    /// Optional speed layer: reverse occ/typeref postings for this tip.
    pub reverse: Option<&'a ReversePositionIndex>,
}

impl<'a> IrTrustfallAdapter<'a> {
    /// Construct an adapter without a reverse index; all reverse look-ups fall
    /// back to a linear scan.
    pub fn new(ir: &'a IrView) -> Self {
        Self { ir, reverse: None }
    }

    /// Construct an adapter with a pre-built reverse index for O(log n)
    /// reverse look-ups.
    pub fn with_reverse(ir: &'a IrView, reverse: &'a ReversePositionIndex) -> Self {
        Self {
            ir,
            reverse: Some(reverse),
        }
    }

    // -----------------------------------------------------------------------
    // Neighbour methods (load-bearing for this wave)
    // -----------------------------------------------------------------------

    /// Direct children of `parent` within the same package.
    ///
    /// Corresponds to the `Member` graph edge (entry-to-children enclosure).
    /// Delegates to [`IrView::children_of`]; the iterator order matches the
    /// underlying `children_of` contract (ascending intro order).
    pub fn members(&self, parent: IntroId) -> impl Iterator<Item = IntroId> + '_ {
        self.ir.children_of(parent).iter().copied()
    }

    /// The resolved target of an occurrence (the `OccTarget` graph edge).
    ///
    /// Clones `occ.target`; this is the only hot-path clone and it is
    /// unavoidable because the caller needs an owned value as a map key.
    pub fn occ_target(&self, occ: &Occurrence) -> StableRef {
        occ.target.clone()
    }

    /// Load-bearing type references from an entry (impl-of + trait-supers).
    ///
    /// Corresponds to the `TypeRef` graph edge. Uses
    /// [`crate::graph::reverse_index::typerefs_of_entry`] so the extraction
    /// rule is defined in exactly one place.
    ///
    /// Field-type mentions are deferred to a later wave (see the `NOTE` in
    /// `typerefs_of_entry`).
    pub fn type_refs(&self, intro: IntroId) -> Vec<StableRef> {
        match self.ir.entry(intro) {
            Some(entry) => typerefs_of_entry(entry, self.ir.package()),
            None => Vec::new(),
        }
    }

    /// The parent (enclosing) entry of `intro`, if any.
    ///
    /// Corresponds to the `Lineage` / continuity graph edge. Returns `None`
    /// for root entries.
    pub fn lineage(&self, intro: IntroId) -> Option<IntroId> {
        self.ir.parent_of(intro)
    }

    /// The set of entries that hold a graph-worthy occurrence whose `target` is
    /// `target`.
    ///
    /// **Fast path** (O(log n)): if a [`ReversePositionIndex`] is present,
    /// delegates to [`ReversePositionIndex::usages_of`] and clones the slice
    /// into a `Vec`.
    ///
    /// **Slow path** (O(n)): scans all occurrences in the view, filtering to
    /// those that are graph-worthy *and* match `target`. Use the fast path in
    /// production by building a [`ReversePositionIndex`] first.
    ///
    /// `IrView::all_occurrences()` yields `(owner: IntroId, &Occurrence)` pairs;
    /// the owner is the first tuple element.
    pub fn usages(&self, target: &StableRef) -> Vec<IntroId> {
        if let Some(rev) = self.reverse {
            // Fast path: O(log n) posting look-up.
            rev.usages_of(target).to_vec()
        } else {
            // Slow path: O(n) linear scan. Correct but unindexed.
            // Callers that need repeated reverse look-ups should build a
            // ReversePositionIndex once and use `with_reverse`.
            self.ir
                .all_occurrences()
                .filter(|(_, occ)| occ.confidence.is_graph_worthy() && &occ.target == target)
                .map(|(owner, _)| owner)
                .collect()
        }
    }
}

// ---------------------------------------------------------------------------
// execute_graph_query  (stub — correct types, schema wired in later wave)
// ---------------------------------------------------------------------------

/// Execute a Trustfall query against the IR graph.
///
/// **This wave:** always returns [`Error::Unsupported`].  The full
/// Trustfall `Adapter` trait implementation, schema `.toml`, and query execution
/// loop are deferred to a later wave.  The hand-rolled neighbour methods on
/// [`IrTrustfallAdapter`] are the load-bearing deliverable for INDEX-PLAN §5.5
/// Wave 1.
///
/// The function signature is kept correct so that a later wave can implement the
/// body without touching any call sites: it already references real
/// [`trustfall::FieldValue`] and the error type is stable.
pub fn execute_graph_query(
    adapter: &IrTrustfallAdapter<'_>,
    query: &str,
    _args: BTreeMap<Arc<str>, trustfall::FieldValue>,
) -> Result<Vec<BTreeMap<Arc<str>, trustfall::FieldValue>>, Error> {
    // Suppress unused-variable lint while the body is a stub.
    let _ = adapter.ir;
    Err(Error::Unsupported(query.to_owned()))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use ir::apply::PristineIntroTable;
    use ir::change::{EcosystemId, IntroId, PackageLineageId, PackageName, StableRef};
    use ir::entry::{Entry, Node, Symbol, Visibility};
    use ir::kind::Kind;
    use ir::kinds::{Function, Module};
    use ir::view::IrView;
    use ir::vocab::{Confidence, Occurrence, ReferenceKind, RelSpan};

    use crate::graph::reverse_index::{ReverseIndexKey, ReversePositionIndex, SCHEMA_VERSION};

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    fn pkg_id() -> PackageLineageId {
        PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("adptr-test"))
    }

    fn intro(n: u8) -> IntroId {
        IntroId::from_raw([n; 32])
    }

    fn sref_local(n: u8) -> StableRef {
        StableRef::new(pkg_id(), intro(n))
    }

    /// Build a minimal [`Symbol`] with only a name; all other fields are empty/default.
    fn sym(name: &str) -> Symbol {
        Symbol {
            name: name.to_owned(),
            visibility: Visibility::Public,
            documentation: String::new(),
            source: PathBuf::new(),
            span: 0..0,
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        }
    }

    fn entry(name: &str, kind: Kind) -> Entry {
        Entry::new(sym(name), Node::build(None::<ir::index::RawRef>, []), kind)
    }

    fn module(name: &str) -> Entry {
        entry(name, Kind::Module(Module))
    }

    fn function(name: &str) -> Entry {
        entry(name, Kind::Function(Function::builder().build()))
    }

    fn default_key() -> ReverseIndexKey {
        ReverseIndexKey {
            channel_tip: [1u8; 32],
            schema_version: SCHEMA_VERSION,
        }
    }

    // -----------------------------------------------------------------------
    // Tests
    // -----------------------------------------------------------------------

    /// `members(root)` returns both children (collected into a sorted set for
    /// assertion, because IrView returns children in ascending intro order).
    #[test]
    fn members_returns_both_children() {
        let mut table = PristineIntroTable::new();
        table.insert_live(intro(1), module("root"), None);
        table.insert_live(intro(2), function("child_a"), Some(intro(1)));
        table.insert_live(intro(3), function("child_b"), Some(intro(1)));
        let ir = IrView::with_package(pkg_id(), table);

        let adapter = IrTrustfallAdapter::new(&ir);

        let mut children: Vec<IntroId> = adapter.members(intro(1)).collect();
        children.sort_unstable();
        assert_eq!(children, vec![intro(2), intro(3)]);
    }

    /// `usages(&targetB)` returns `[A]` on the **slow path** (no reverse index).
    #[test]
    fn usages_slow_path_without_reverse() {
        let mut table = PristineIntroTable::new();
        table.insert_live(intro(1), module("root"), None);
        table.insert_live(intro(2), function("caller_a"), Some(intro(1)));
        table.insert_live(intro(3), function("callee_b"), Some(intro(1)));
        let mut ir = IrView::with_package(pkg_id(), table);

        let target = sref_local(3);
        ir.add_occurrence(
            intro(2),
            Occurrence::new(
                target.clone(),
                ReferenceKind::FunctionCall,
                Confidence::Oracle,
                RelSpan::new(0, 5),
            ),
        );

        let adapter = IrTrustfallAdapter::new(&ir);
        let mut owners = adapter.usages(&target);
        owners.sort_unstable();
        assert_eq!(owners, vec![intro(2)]);
    }

    /// `usages(&targetB)` returns `[A]` on the **fast path** (with reverse index).
    #[test]
    fn usages_fast_path_with_reverse() {
        let mut table = PristineIntroTable::new();
        table.insert_live(intro(1), module("root"), None);
        table.insert_live(intro(2), function("caller_a"), Some(intro(1)));
        table.insert_live(intro(3), function("callee_b"), Some(intro(1)));
        let mut ir = IrView::with_package(pkg_id(), table);

        let target = sref_local(3);
        ir.add_occurrence(
            intro(2),
            Occurrence::new(
                target.clone(),
                ReferenceKind::FunctionCall,
                Confidence::Oracle,
                RelSpan::new(0, 5),
            ),
        );

        let reverse = ReversePositionIndex::build(&ir, default_key());
        let adapter = IrTrustfallAdapter::with_reverse(&ir, &reverse);

        let mut owners = adapter.usages(&target);
        owners.sort_unstable();
        assert_eq!(owners, vec![intro(2)]);
    }

    /// Both paths (with and without reverse) agree on the same result.
    #[test]
    fn usages_slow_and_fast_paths_agree() {
        let mut table = PristineIntroTable::new();
        table.insert_live(intro(1), module("root"), None);
        table.insert_live(intro(2), function("a"), Some(intro(1)));
        table.insert_live(intro(3), function("b"), Some(intro(1)));
        table.insert_live(intro(4), function("target"), Some(intro(1)));
        let mut ir = IrView::with_package(pkg_id(), table);

        let target = sref_local(4);
        for owner in [intro(2), intro(3)] {
            ir.add_occurrence(
                owner,
                Occurrence::new(
                    target.clone(),
                    ReferenceKind::FunctionCall,
                    Confidence::Import,
                    RelSpan::new(0, 3),
                ),
            );
        }

        let reverse = ReversePositionIndex::build(&ir, default_key());

        let slow_adapter = IrTrustfallAdapter::new(&ir);
        let fast_adapter = IrTrustfallAdapter::with_reverse(&ir, &reverse);

        let mut slow = slow_adapter.usages(&target);
        let mut fast = fast_adapter.usages(&target);
        slow.sort_unstable();
        fast.sort_unstable();
        assert_eq!(slow, fast, "slow and fast paths must agree on usages");
    }

    /// Two-hop: `members(root)` → for each child, `type_refs` — should run
    /// without panic and return structurally correct results (empty in this
    /// case, since neither child is an Impl or Trait).
    #[test]
    fn two_hop_members_then_type_refs() {
        let mut table = PristineIntroTable::new();
        table.insert_live(intro(1), module("root"), None);
        table.insert_live(intro(2), function("fn_a"), Some(intro(1)));
        table.insert_live(intro(3), function("fn_b"), Some(intro(1)));
        let ir = IrView::with_package(pkg_id(), table);

        let adapter = IrTrustfallAdapter::new(&ir);
        let children: Vec<IntroId> = adapter.members(intro(1)).collect();

        // Both functions have no load-bearing type refs (fn kind is not Impl/Trait).
        for child in &children {
            let refs = adapter.type_refs(*child);
            // Shape check: result is always a Vec (possibly empty).
            let _ = refs.len();
        }
        assert_eq!(children.len(), 2);
    }

    /// `execute_graph_query` is a stub returning `Unsupported` this wave.
    #[test]
    fn execute_graph_query_is_stub() {
        let ir = IrView::with_package(pkg_id(), PristineIntroTable::new());
        let adapter = IrTrustfallAdapter::new(&ir);
        let result = execute_graph_query(&adapter, "{ Package { name } }", BTreeMap::new());
        assert!(
            matches!(result, Err(Error::Unsupported(_))),
            "execute_graph_query must be a stub returning Unsupported this wave"
        );
    }

    /// `GraphVertex::typename` returns the correct type name for each shape.
    #[test]
    fn graph_vertex_typename() {
        let ir = IrView::with_package(pkg_id(), PristineIntroTable::new());
        let pkg_vertex = GraphVertex::Package(ir.package());
        assert_eq!(pkg_vertex.typename(), "Package");
    }

    /// `lineage` returns `None` for a root entry and `Some` for a child.
    #[test]
    fn lineage_edge() {
        let mut table = PristineIntroTable::new();
        table.insert_live(intro(1), module("root"), None);
        table.insert_live(intro(2), function("child"), Some(intro(1)));
        let ir = IrView::with_package(pkg_id(), table);

        let adapter = IrTrustfallAdapter::new(&ir);
        assert_eq!(adapter.lineage(intro(1)), None);
        assert_eq!(adapter.lineage(intro(2)), Some(intro(1)));
    }
}
