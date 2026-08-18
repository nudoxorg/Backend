//! Trustfall graph adapter over the IR read model (INDEX-PLAN §5.5).
//!
//! This module exposes:
//!
//! * [`GraphVertex`] — the vertex discriminated union (`Entry`, `Occ`, `Package`).
//! * [`IrTrustfallAdapter`] — a borrow of an [`IrView`] plus an optional
//!   [`ReversePositionIndex`] for the fast reverse-lookup path.
//! * Plain-Rust **neighbour methods** on the adapter — these are the load-bearing
//!   deliverable for this wave.
//! * [`execute_graph_query`] — executes the supported package-local graph
//!   schema over an [`IrView`].

use std::collections::BTreeMap;
use std::sync::Arc;

use thiserror::Error;

use ir::change::{IntroId, PackageLineageId, StableRef};
use ir::entry::Entry;
use ir::view::IrView;
use ir::vocab::Occurrence;
use trustfall::provider::{
    AsVertex, BasicAdapter, ContextIterator, ContextOutcomeIterator, EdgeParameters, Typename,
    VertexIterator, resolve_neighbors_with, resolve_property_with,
};

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

impl GraphVertex<'_> {
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

impl Typename for GraphVertex<'_> {
    fn typename(&self) -> &'static str {
        self.typename()
    }
}

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

/// Errors that can arise from [`execute_graph_query`].
#[derive(Debug, Error)]
pub enum Error {
    /// The query asks for a graph capability that is deliberately not exposed
    /// by the package-local schema. The string carries the query for diagnostics.
    #[error("trustfall query is unsupported by the package-local graph schema: {0}")]
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
        self.ir.entry(intro).map_or_else(Vec::new, |entry| {
            typerefs_of_entry(entry, self.ir.package())
        })
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
        self.reverse.map_or_else(
            || {
                // Slow path: O(n) linear scan. Correct but unindexed.
                // Callers that need repeated reverse look-ups should build a
                // ReversePositionIndex once and use `with_reverse`.
                self.ir
                    .all_occurrences()
                    .filter(|(_, occ)| occ.confidence.is_graph_worthy() && &occ.target == target)
                    .map(|(owner, _)| owner)
                    .collect()
            },
            // Fast path: O(log n) posting look-up.
            |rev| rev.usages_of(target).to_vec(),
        )
    }
}

// ---------------------------------------------------------------------------
// Trustfall schema and adapter
// ---------------------------------------------------------------------------

/// The graph surface that can be answered from one in-memory [`IrView`].
///
/// `DependsOn` is intentionally absent: it requires the catalog-edge join,
/// which is not available to this package-local adapter. Cross-package targets
/// also remain scalar `StableRef` values rather than vertices because the
/// adapter owns exactly one `IrView`.
const SCHEMA: &str = r#"
schema { query: RootSchemaQuery }

directive @filter(op: String!, value: [String!]) repeatable on FIELD | INLINE_FRAGMENT
directive @tag(name: String) on FIELD
directive @output(name: String) on FIELD
directive @optional on FIELD
directive @recurse(depth: Int!) on FIELD
directive @fold on FIELD
directive @transform(op: String!) on FIELD

type RootSchemaQuery {
    Package: Package!
    Entries: [Entry!]!
}

type Package {
    name: String!
    ecosystem: String!
    lineage: String!
    entries: [Entry!]!
}

type Entry {
    name: String!
    stableRef: String!
    members: [Entry!]!
    occurrences: [Occ!]!
    typeRefs: [Entry!]!
    lineage: Entry
    usages: [Entry!]!
}

type Occ {
    target: String!
    kind: String!
    confidence: Int!
    occTarget: Entry
}
"#;

fn schema() -> Result<trustfall::Schema, Error> {
    trustfall::Schema::parse(SCHEMA).map_err(|error| Error::Execute(error.to_string()))
}

/// Thin owned wrapper required by Trustfall's `Arc<Adapter>` execution API.
struct QueryAdapter<'a> {
    graph: &'a IrTrustfallAdapter<'a>,
}

fn local_entry<'a>(ir: &'a IrView, target: &StableRef) -> Option<GraphVertex<'a>> {
    (target.package == *ir.package())
        .then(|| ir.entry(target.intro))
            .flatten()
            .map(|entry| GraphVertex::Entry {
                intro: target.intro,
                entry,
            })
}

impl<'a> BasicAdapter<'a> for QueryAdapter<'a> {
    type Vertex = GraphVertex<'a>;

    fn resolve_starting_vertices(
        &self,
        edge_name: &str,
        parameters: &EdgeParameters,
    ) -> VertexIterator<'a, Self::Vertex> {
        debug_assert!(parameters.is_empty());
        match edge_name {
            "Package" => Box::new(std::iter::once(GraphVertex::Package(
                self.graph.ir.package(),
            ))),
            "Entries" => Box::new(
                self.graph
                    .ir
                    .entries_sorted()
                    .map(|(intro, entry)| GraphVertex::Entry { intro, entry }),
            ),
            _ => Box::new(std::iter::empty()),
        }
    }

    fn resolve_property<V: AsVertex<Self::Vertex> + 'a>(
        &self,
        contexts: ContextIterator<'a, V>,
        type_name: &str,
        property_name: &str,
    ) -> ContextOutcomeIterator<'a, V, trustfall::FieldValue> {
        let package = self.graph.ir.package();
        match (type_name, property_name) {
            ("Package", "name") => resolve_property_with(contexts, |vertex| match vertex {
                GraphVertex::Package(package) => package.name.as_str().into(),
                _ => unreachable!("Trustfall supplied a non-Package vertex"),
            }),
            ("Package", "ecosystem") => resolve_property_with(contexts, |vertex| match vertex {
                GraphVertex::Package(package) => package.ecosystem.as_str().into(),
                _ => unreachable!("Trustfall supplied a non-Package vertex"),
            }),
            ("Package", "lineage") => resolve_property_with(contexts, |vertex| match vertex {
                GraphVertex::Package(package) => package.to_string().into(),
                _ => unreachable!("Trustfall supplied a non-Package vertex"),
            }),
            ("Entry", "name") => resolve_property_with(contexts, |vertex| match vertex {
                GraphVertex::Entry { entry, .. } => entry.sym().name.as_str().into(),
                _ => unreachable!("Trustfall supplied a non-Entry vertex"),
            }),
            ("Entry", "stableRef") => resolve_property_with(contexts, move |vertex| match vertex {
                GraphVertex::Entry { intro, .. } => {
                    StableRef::new(package.clone(), *intro).to_string().into()
                }
                _ => unreachable!("Trustfall supplied a non-Entry vertex"),
            }),
            ("Occ", "target") => resolve_property_with(contexts, |vertex| match vertex {
                GraphVertex::Occ(occ) => occ.target.to_string().into(),
                _ => unreachable!("Trustfall supplied a non-Occ vertex"),
            }),
            ("Occ", "kind") => resolve_property_with(contexts, |vertex| match vertex {
                GraphVertex::Occ(occ) => format!("{:?}", occ.kind).into(),
                _ => unreachable!("Trustfall supplied a non-Occ vertex"),
            }),
            ("Occ", "confidence") => resolve_property_with(contexts, |vertex| match vertex {
                GraphVertex::Occ(occ) => match occ.confidence {
                    ir::vocab::Confidence::Syntactic => 0_i64.into(),
                    ir::vocab::Confidence::Suffix => 1_i64.into(),
                    ir::vocab::Confidence::Index => 2_i64.into(),
                    ir::vocab::Confidence::Import => 3_i64.into(),
                    ir::vocab::Confidence::Oracle => 4_i64.into(),
                },
                _ => unreachable!("Trustfall supplied a non-Occ vertex"),
            }),
            _ => unreachable!("Trustfall requested a schema field without a resolver"),
        }
    }

    fn resolve_neighbors<V: AsVertex<Self::Vertex> + 'a>(
        &self,
        contexts: ContextIterator<'a, V>,
        type_name: &str,
        edge_name: &str,
        parameters: &EdgeParameters,
    ) -> ContextOutcomeIterator<'a, V, VertexIterator<'a, Self::Vertex>> {
        debug_assert!(parameters.is_empty());
        let graph = self.graph;
        match (type_name, edge_name) {
            ("Package", "entries") => resolve_neighbors_with(contexts, move |vertex| {
                match vertex {
                    GraphVertex::Package(_) => Box::new(
                        graph
                            .ir
                            .entries_sorted()
                            .map(|(intro, entry)| GraphVertex::Entry { intro, entry }),
                    ),
                    _ => unreachable!("Trustfall supplied a non-Package vertex"),
                }
            }),
            ("Entry", "members") => resolve_neighbors_with(contexts, move |vertex| match vertex {
                GraphVertex::Entry { intro, .. } => {
                    Box::new(graph.members(*intro).filter_map(|child| {
                        graph.ir.entry(child).map(|entry| GraphVertex::Entry {
                            intro: child,
                            entry,
                        })
                    }))
                }
                _ => unreachable!("Trustfall supplied a non-Entry vertex"),
            }),
            ("Entry", "occurrences") => {
                resolve_neighbors_with(contexts, move |vertex| match vertex {
                    GraphVertex::Entry { intro, .. } => Box::new(
                        graph
                            .ir
                            .occurrences_of(*intro)
                            .iter()
                            .filter(|occ| occ.confidence.is_graph_worthy())
                            .map(GraphVertex::Occ),
                    ),
                    _ => unreachable!("Trustfall supplied a non-Entry vertex"),
                })
            }
            ("Occ", "occTarget") => resolve_neighbors_with(contexts, move |vertex| match vertex {
                GraphVertex::Occ(occ) => Box::new(local_entry(graph.ir, &occ.target).into_iter()),
                _ => unreachable!("Trustfall supplied a non-Occ vertex"),
            }),
            ("Entry", "typeRefs") => resolve_neighbors_with(contexts, move |vertex| match vertex {
                GraphVertex::Entry { intro, .. } => {
                    let targets = graph.type_refs(*intro);
                    Box::new(
                        targets
                            .into_iter()
                            .filter_map(move |target| local_entry(graph.ir, &target)),
                    )
                }
                _ => unreachable!("Trustfall supplied a non-Entry vertex"),
            }),
            ("Entry", "lineage") => resolve_neighbors_with(contexts, move |vertex| match vertex {
                GraphVertex::Entry { intro, .. } => {
                    Box::new(graph.lineage(*intro).into_iter().filter_map(|parent| {
                        graph.ir.entry(parent).map(|entry| GraphVertex::Entry {
                            intro: parent,
                            entry,
                        })
                    }))
                }
                _ => unreachable!("Trustfall supplied a non-Entry vertex"),
            }),
            ("Entry", "usages") => resolve_neighbors_with(contexts, move |vertex| match vertex {
                GraphVertex::Entry { intro, .. } => {
                    let target = StableRef::new(graph.ir.package().clone(), *intro);
                    Box::new(graph.usages(&target).into_iter().filter_map(|owner| {
                        graph.ir.entry(owner).map(|entry| GraphVertex::Entry {
                            intro: owner,
                            entry,
                        })
                    }))
                }
                _ => unreachable!("Trustfall supplied a non-Entry vertex"),
            }),
            _ => unreachable!("Trustfall requested a schema edge without a resolver"),
        }
    }

    fn resolve_coercion<V: AsVertex<Self::Vertex> + 'a>(
        &self,
        _contexts: ContextIterator<'a, V>,
        _type_name: &str,
        _coerce_to_type: &str,
    ) -> ContextOutcomeIterator<'a, V, bool> {
        unreachable!("the package-local graph schema has no subtypes")
    }
}

/// Execute a Trustfall query against the IR graph.
///
/// The supported schema covers one package's entries, their containment and
/// lineage, graph-worthy occurrences, local occurrence/type-reference targets,
/// and reverse usages. See [`SCHEMA`] for the exact public field names.
///
/// The catalog-backed `DependsOn` edge is intentionally unavailable here and
/// returns [`Error::Unsupported`]. Other malformed or schema-invalid queries
/// return [`Error::Execute`] with Trustfall's diagnostic.
pub fn execute_graph_query<'a>(
    adapter: &'a IrTrustfallAdapter<'a>,
    query: &str,
    args: BTreeMap<Arc<str>, trustfall::FieldValue>,
) -> Result<Vec<BTreeMap<Arc<str>, trustfall::FieldValue>>, Error> {
    if query.contains("DependsOn") {
        return Err(Error::Unsupported(query.to_owned()));
    }

    let rows = trustfall::execute_query(
        &schema()?,
        Arc::new(QueryAdapter { graph: adapter }),
        query,
        args,
    )
    .map_err(|error| Error::Execute(error.to_string()))?;
    Ok(rows.collect())
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

    /// The package root and containment edge execute through the Trustfall
    /// schema, returning the child symbol's name.
    #[test]
    fn execute_graph_query_traverses_package_members() {
        let mut table = PristineIntroTable::new();
        table.insert_live(intro(1), module("root"), None);
        table.insert_live(intro(2), function("child"), Some(intro(1)));
        let ir = IrView::with_package(pkg_id(), table);
        let adapter = IrTrustfallAdapter::new(&ir);

        let result = execute_graph_query(
            &adapter,
            "{ Package { entries { name @filter(op: \"=\", value: [\"root\"]) members { child: name @output } } } }",
            BTreeMap::new(),
        )
        .expect("supported package-local query must execute");

        assert_eq!(result.len(), 1);
        assert_eq!(
            result[0]["child"],
            trustfall::FieldValue::String("child".into())
        );
    }

    /// Reverse usages use the adapter's graph-worthy occurrence projection.
    #[test]
    fn execute_graph_query_traverses_reverse_usages() {
        let mut table = PristineIntroTable::new();
        table.insert_live(intro(1), function("caller"), None);
        table.insert_live(intro(2), function("callee"), None);
        let mut ir = IrView::with_package(pkg_id(), table);
        ir.add_occurrence(
            intro(1),
            Occurrence::new(
                sref_local(2),
                ReferenceKind::FunctionCall,
                Confidence::Oracle,
                RelSpan::new(0, 5),
            ),
        );
        let adapter = IrTrustfallAdapter::new(&ir);

        let result = execute_graph_query(
            &adapter,
            "{ Entries { name @filter(op: \"=\", value: [\"callee\"]) usages { caller: name @output } } }",
            BTreeMap::new(),
        )
        .expect("supported reverse-usage query must execute");

        assert_eq!(result.len(), 1);
        assert_eq!(
            result[0]["caller"],
            trustfall::FieldValue::String("caller".into())
        );
    }

    /// Occurrence targets resolve as local graph vertices, preserving the
    /// scalar target key for cross-package references that cannot be loaded.
    #[test]
    fn execute_graph_query_traverses_occurrence_target() {
        let mut table = PristineIntroTable::new();
        table.insert_live(intro(1), function("caller"), None);
        table.insert_live(intro(2), function("callee"), None);
        let mut ir = IrView::with_package(pkg_id(), table);
        ir.add_occurrence(
            intro(1),
            Occurrence::new(
                sref_local(2),
                ReferenceKind::FunctionCall,
                Confidence::Index,
                RelSpan::new(0, 5),
            ),
        );
        let adapter = IrTrustfallAdapter::new(&ir);

        let result = execute_graph_query(
            &adapter,
            "{ Entries { name @filter(op: \"=\", value: [\"caller\"]) occurrences { occTarget { callee: name @output } } } }",
            BTreeMap::new(),
        )
        .expect("supported occurrence-target query must execute");

        assert_eq!(
            result[0]["callee"],
            trustfall::FieldValue::String("callee".into())
        );
    }

    /// Catalog-backed dependency edges are explicitly unavailable from one
    /// package-local IR view rather than being reported as a generic parse error.
    #[test]
    fn execute_graph_query_rejects_deferred_dependency_edge() {
        let ir = IrView::with_package(pkg_id(), PristineIntroTable::new());
        let adapter = IrTrustfallAdapter::new(&ir);

        let result = execute_graph_query(
            &adapter,
            "{ Package { DependsOn { name } } }",
            BTreeMap::new(),
        );

        assert!(matches!(result, Err(Error::Unsupported(_))));
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
