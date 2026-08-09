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
///
/// # Variant-to-schema mapping
///
/// | Variant | Schema type | KindDiscriminant |
/// |---------|-------------|------------------|
/// | `Package` | `Package` | — |
/// | `Function` | `Function` | `Function` |
/// | `Record` | `Record` | `Record` |
/// | `Trait` | `Trait` | `Trait` |
/// | `Impl` | `Impl` | `Impl` |
/// | `Enum` | `Enum` | `Enum` |
/// | `Field` | `Field` | `Field` |
/// | `Const` | `Const` | `Const` |
/// | `Alias` | `Alias` | `Alias` |
/// | `Static` | `Static` | `Static` |
/// | `Variant` | `Variant` | `Variant` |
/// | `Module` | `Module` | `Module` |
/// | `Reexport` | `Reexport` | `Reexport` |
/// | `Param` | `Param` | `Param` |
/// | `OtherSymbol` | `OtherSymbol` | `None` / `Reference` |
/// | `Occurrence` | `Occurrence` | — |
/// | `SourceLocation` | `SourceLocation` | — |
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
    /// A static variable declaration.  Previously collapsed into `OtherSymbol`;
    /// promoted to its own variant so that `... on Static { }` coercions work.
    Static(SymbolVertex),
    /// An enum variant (unit, tuple, or struct form).  Previously collapsed
    /// into `OtherSymbol`; promoted so that `... on Variant { }` coercions work.
    Variant(SymbolVertex),
    /// A module or namespace.  Previously collapsed into `OtherSymbol`;
    /// promoted so that `... on Module { }` coercions work.
    Module(SymbolVertex),
    /// A re-export declaration.  Previously collapsed into `OtherSymbol`;
    /// promoted so that `... on Reexport { }` coercions work.
    Reexport(SymbolVertex),
    /// A type or value parameter.  Previously collapsed into `OtherSymbol`;
    /// promoted so that `... on Param { }` coercions work.
    Param(SymbolVertex),
    /// Catch-all for `None` (reference entries) and any future discriminant
    /// the schema has no named type for yet.  See schema comment on `OtherSymbol`.
    OtherSymbol(SymbolVertex),
    Occurrence(OccurrenceVertex),
    /// Where a symbol's declaration was written, as
    /// [`nudox_ir::entry::SourceLocation`] knows it.
    ///
    /// Carries the owning [`SymbolVertex`] rather than a copy of the location,
    /// for the same reason `SymbolVertex` carries an `IntroId` rather than an
    /// `Entry`: the vertex must be `'static`, and the location is an O(1)
    /// `entry(intro).location()` read away.
    ///
    /// It is a vertex rather than nine scalar fields on every symbol type
    /// because the IR value is a *sum* — `Declared | BytesOnly | Unlocated` —
    /// and flattening a sum into sibling nullable columns is precisely how the
    /// `Unlocated` reason would have been lost.
    SourceLocation(SymbolVertex),
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Extract the [`SymbolVertex`] from any symbol-typed [`Vertex`] variant.
///
/// Returns `None` for `Package` and `Occurrence`, which are not symbols.
/// Used in the adapter's resolvers to share property/neighbor logic across
/// all fourteen concrete symbol types.
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
        | Vertex::Static(sv)
        | Vertex::Variant(sv)
        | Vertex::Module(sv)
        | Vertex::Reexport(sv)
        | Vertex::Param(sv)
        | Vertex::OtherSymbol(sv) => Some(sv),
        // `SourceLocation` deliberately does *not* answer here even though it
        // wraps a `SymbolVertex`. It is a distinct schema type with its own
        // properties; letting it fall through to the Symbol-interface resolver
        // would make `location { name }` silently return the owner's name.
        Vertex::Package(_) | Vertex::Occurrence(_) | Vertex::SourceLocation(_) => None,
    }
}
