//! Vertex types for the nudox-graph Trustfall adapter.
//!
//! # `'static` vertex design
//!
//! `AsyncBasicAdapter` requires `Self::Vertex: 'vertex` where `'vertex` is the
//! adapter's query lifetime.  Because all data in [`Vertex`] lives behind
//! `Arc` (never a bare borrow into a lock guard), every variant is `'static`
//! and satisfies any `'vertex` bound.  This lets the async engine store
//! vertices in futures that may outlive a single corpus-lock acquisition.

use std::sync::Arc;

use nudox_ir::change::{IntroId, StableRef};
use nudox_store::package::PackageView;
use trustfall::provider::TrustfallEnumVertex;

// ---------------------------------------------------------------------------
// SymbolVertex
// ---------------------------------------------------------------------------

/// The identity of one symbol within a package.
///
/// Owns `Arc<PackageView>` rather than `&Entry` so that the vertex is
/// `'static` — futures stored by the async adapter may outlive any single
/// borrow of the corpus lock.  The `Arc` is cheap to clone (atomic refcount)
/// and the `IrView::entry(intro)` lookup is O(1) via the internal hash table.
#[derive(Debug, Clone)]
pub struct SymbolVertex {
    pub package: Arc<PackageView>,
    pub intro: IntroId,
}

impl SymbolVertex {
    /// Construct the `StableRef` for this symbol.
    pub fn stable_ref(&self) -> StableRef {
        StableRef::new(self.package.lineage().clone(), self.intro)
    }
}

// ---------------------------------------------------------------------------
// OccurrenceVertex
// ---------------------------------------------------------------------------

/// An occurrence record attached to a specific symbol.
///
/// The full [`nudox_ir::vocab::Occurrence`] is accessed via
/// `owner.package.view().occurrences_of(owner.intro)[occ_index]`.
#[derive(Debug, Clone)]
pub struct OccurrenceVertex {
    /// The symbol that owns this occurrence.
    pub owner: SymbolVertex,
    /// Index into the owner's occurrence slice (stable within one query).
    pub occ_index: usize,
}

// ---------------------------------------------------------------------------
// Vertex
// ---------------------------------------------------------------------------

/// Every node type reachable from a Trustfall query over the local IR corpus.
///
/// # `TrustfallEnumVertex`
///
/// The derive macro generates:
/// * `impl Typename for Vertex` — returns the variant name as `&'static str`.
/// * `as_<variant>()` conversion methods used by trustfall's coercion engine.
///
/// The variant names **must** match the type names declared in `schema.graphql`.
#[derive(Debug, Clone, TrustfallEnumVertex)]
pub enum Vertex {
    Package(Arc<PackageView>),
    Function(SymbolVertex),
    Record(SymbolVertex),
    Trait(SymbolVertex),
    Impl(SymbolVertex),
    Enum(SymbolVertex),
    Field(SymbolVertex),
    Const(SymbolVertex),
    Alias(SymbolVertex),
    OtherSymbol(SymbolVertex),
    Occurrence(OccurrenceVertex),
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Extract the [`SymbolVertex`] from any symbol-typed [`Vertex`] variant.
///
/// Returns `None` for `Package` and `Occurrence`, which are not symbols.
/// Used in the adapter's resolvers to share property/neighbor logic across
/// all nine concrete symbol types.
pub fn as_symbol_vertex(v: &Vertex) -> Option<&SymbolVertex> {
    match v {
        Vertex::Function(sv)
        | Vertex::Record(sv)
        | Vertex::Trait(sv)
        | Vertex::Impl(sv)
        | Vertex::Enum(sv)
        | Vertex::Field(sv)
        | Vertex::Const(sv)
        | Vertex::Alias(sv)
        | Vertex::OtherSymbol(sv) => Some(sv),
        Vertex::Package(_) | Vertex::Occurrence(_) => None,
    }
}
