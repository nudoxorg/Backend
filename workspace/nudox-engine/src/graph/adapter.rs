//! [`CorpusAdapter`] — the Trustfall `AsyncAdapter` over the IR corpus.
//!
//! # Error contract (LR-6)
//!
//! No `unwrap` or `expect` in resolution paths. Every fallible step returns
//! `Result<_, Error>`. Errors propagate via the stream items and are
//! collected by the trustfall engine, which terminates the query on first
//! error.
//!
//! # Async corpus access
//!
//! [`Corpus`] accessors (`packages()`, `package()`, `entry()`) are `async`.
//! `AsyncAdapter` methods return `Pin<Box<dyn Stream>>` — not futures —
//! so async corpus calls are wrapped in `futures::stream::once(async { … })
//! .flat_map(…)` pipelines that poll lazily inside the async executor.
//!
//! # Filter pushdown
//!
//! `resolve_starting_vertices` does not decide anything itself: it hands the
//! engine's hints to [`crate::graph::plan`], which returns a [`SymbolPlan`] or
//! [`PackagePlan`] naming the access path, and then executes that plan. See
//! that module for why the decision is a value rather than a ladder of
//! `if let`s.
//!
//! # One corpus round-trip per resolution, not one per row
//!
//! `Corpus::packages()` takes a read lock over the whole corpus map and
//! allocates a fresh `Vec` with one `Arc` clone per loaded package — on every
//! call. The reverse edges (`usages`, `mentions`, `implementors`) each used to
//! call it *inside* their per-vertex resolver closure, so a query over `T`
//! source symbols against a corpus of `P` packages paid `T` lock acquisitions
//! and `T × P` `Arc` clones to read a table that cannot change during the
//! query. [`CorpusMemo`] collapses that to one, and records what it did in an
//! [`AdapterProbe`] so the collapse is testable rather than asserted.
//!
//! Every reverse edge added since — `implementedBy`, `subtypes`, `returnedBy`,
//! `acceptedBy`, `heldBy` — goes through the same [`posting_neighbors`], so it
//! inherits the memo rather than being a fresh opportunity to reintroduce the
//! N+1. The forward `signatureTypes` edge reads the entry itself and takes
//! only `CorpusMemo::package`, which memoises misses as well as hits.
//!
//! # Why `AsyncAdapter` directly, not `AsyncBasicAdapter`
//!
//! `AsyncBasicAdapter::resolve_starting_vertices` does not receive a
//! `&ResolveInfo`, so there is no way to inspect filter hints through that
//! interface.  The blanket `impl<T: AsyncBasicAdapter> AsyncAdapter for T`
//! drops `_resolve_info` on the floor.  We implement `AsyncAdapter` directly
//! so that resolution sees the hints and can push filters down to the
//! appropriate indexes.

use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use crate::store::{
    corpus::Corpus,
    package::{PackageView, TypePosition, TypeRef, typerefs_of_entry},
};
use futures::{Stream, StreamExt as _, lock::Mutex, stream};
use nudox_ir::{
    change::{IntroId, PackageLineageId, StableRef},
    entry::{Deprecation, SourceLocation, Unlocated, Visibility},
    index::{RawRef, Ref},
    kind::{Kind, KindDiscriminant},
    kinds::{FnModifier, Receiver, Type},
    package::KeyTier,
};
use thiserror::Error;
use trustfall::{
    FieldValue,
    provider::{
        AsVertex, AsyncAdapter, ContextOutcomeStream, ContextStream, EdgeParameters,
        ResolveEdgeInfo, ResolveInfo, Typename as _, VertexInfo as _, VertexStream, async_helpers,
    },
};

use crate::graph::{
    edge_empty::{self, EdgeEmptyLog},
    plan::{PackagePlan, SymbolPlan, plan_packages, plan_symbols},
    probe::{AdapterProbe, StoreProbe},
    vertex::{OccurrenceVertex, SymbolVertex, Vertex},
};

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

/// Errors that the [`CorpusAdapter`] may emit during query execution.
///
/// `#[non_exhaustive]` so that new variants can be added in minor releases
/// without breaking downstream `match` arms.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
    #[error("package not loaded: {0}")]
    PackageNotLoaded(PackageLineageId),

    #[error("symbol not found: {0:?}")]
    SymbolNotFound(IntroId),

    #[error("invalid stable-ref key: {0:?}")]
    InvalidKey(String),

    /// A `Packages { lineage @filter(…) }` operand that is not
    /// `"ecosystem:name"`.
    ///
    /// Distinct from [`Error::InvalidKey`] because the two formats fail
    /// for different reasons and a caller fixes them differently: a lineage
    /// has no `#introhex` half, so reporting "invalid stable-ref key" for
    /// `"cargo:memchr"` would send the reader looking for a missing hex
    /// suffix that should not be there.
    #[error("invalid package lineage {0:?}: expected \"ecosystem:name\"")]
    InvalidLineage(String),

    #[error("unknown edge '{edge}' on type '{ty}'")]
    UnknownEdge { ty: String, edge: String },

    #[error("unknown property '{prop}' on type '{ty}'")]
    UnknownProperty { ty: String, prop: String },

    #[error("missing required parameter '{param}' on edge '{edge}'")]
    MissingParameter { param: String, edge: String },

    #[error("invalid kind string '{0}'")]
    InvalidKind(String),
}

// ---------------------------------------------------------------------------
// CorpusMemo
// ---------------------------------------------------------------------------

/// The corpus round-trips made while resolving one entrypoint or one edge,
/// memoised and counted.
///
/// # Why a memo is correct here
///
/// A query's result set is defined against the corpus as it stood when the
/// query began; `Corpus::packages()` already snapshots (it clones the map's
/// values under one read lock). Serving every resolution in a query from one
/// snapshot is therefore *more* consistent than re-reading, not less: without
/// it, a concurrent `Corpus::insert` could make `usages` see a package that
/// `mentions` did not, inside a single row.
///
/// # Negative caching
///
/// [`CorpusMemo::package`] caches `None` as well as `Some`. `Occurrence.target`
/// is the reason: an occurrence pointing into a package that is not loaded is
/// the common case on a partially-loaded corpus, and re-probing the map once
/// per occurrence to be told "still missing" is the same N+1 in a different
/// hat.
struct CorpusMemo {
    corpus: Corpus,
    probe: Option<Arc<AdapterProbe>>,
    empty_edge_log: Option<Arc<EdgeEmptyLog>>,
    all: Mutex<Option<Arc<[Arc<PackageView>]>>>,
    by_lineage: Mutex<HashMap<PackageLineageId, Option<Arc<PackageView>>>>,
}

impl CorpusMemo {
    fn new(
        corpus: Corpus,
        probe: Option<Arc<AdapterProbe>>,
        empty_edge_log: Option<Arc<EdgeEmptyLog>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            corpus,
            probe,
            empty_edge_log,
            all: Mutex::new(None),
            by_lineage: Mutex::new(HashMap::new()),
        })
    }

    /// Record a store round-trip, if this adapter was built with a probe.
    fn record(&self, which: StoreProbe) {
        if let Some(p) = &self.probe {
            p.record(which);
        }
    }

    /// Every loaded package, read from the corpus at most once.
    ///
    /// Returns `Arc<[…]>` rather than `Vec<…>` so that the second and later
    /// callers pay one atomic increment instead of re-cloning one `Arc` per
    /// package.
    async fn all_packages(self: Arc<Self>) -> Arc<[Arc<PackageView>]> {
        let mut slot = self.all.lock().await;
        if let Some(cached) = slot.as_ref() {
            return Arc::clone(cached);
        }
        self.record(StoreProbe::CorpusList);
        let fresh: Arc<[Arc<PackageView>]> = self.corpus.packages().await.into();
        *slot = Some(Arc::clone(&fresh));
        fresh
    }

    /// One package by lineage, read from the corpus at most once per lineage.
    async fn package(self: Arc<Self>, id: PackageLineageId) -> Option<Arc<PackageView>> {
        let mut seen = self.by_lineage.lock().await;
        if let Some(cached) = seen.get(&id) {
            return cached.clone();
        }
        self.record(StoreProbe::CorpusLookup);
        let found = self.corpus.package(&id).await;
        seen.insert(id, found.clone());
        found
    }
}

// ---------------------------------------------------------------------------
// CorpusAdapter
// ---------------------------------------------------------------------------

/// The Trustfall async adapter over the local IR corpus.
///
/// Cheap to clone — holds only an `Arc`-backed [`Corpus`] and an optional
/// `Arc<AdapterProbe>` shared with a test that wants to assert on how much
/// work the store was asked to do.
#[derive(Clone)]
pub struct CorpusAdapter {
    corpus: Corpus,
    probe: Option<Arc<AdapterProbe>>,
    empty_edge_log: Option<Arc<EdgeEmptyLog>>,
}

impl CorpusAdapter {
    /// Construct an adapter wrapping the given corpus.
    pub fn new(corpus: Corpus) -> Self {
        Self {
            corpus,
            probe: None,
            empty_edge_log: None,
        }
    }

    /// Construct an adapter that records every store round-trip into `probe`.
    ///
    /// An optimisation in this crate is a claim about the *shape* of the work
    /// — one index probe rather than a corpus walk, one package list rather
    /// than one per row — and the rows a query returns are identical either
    /// way. This is how a test asserts on the claim instead of on the rows.
    pub fn new_with_probe(corpus: Corpus, probe: Arc<AdapterProbe>) -> Self {
        Self {
            corpus,
            probe: Some(probe),
            empty_edge_log: None,
        }
    }

    /// Record why a covered reverse edge's posting list was empty.
    ///
    /// The log is shared with the query driver, which reads it after the
    /// result stream ends. Queries that do not ask for a coverage note leave
    /// this unset, and those resolutions stay on the plain neighbor stream.
    pub fn with_empty_edge_log(mut self, log: Arc<EdgeEmptyLog>) -> Self {
        self.empty_edge_log = Some(log);
        self
    }

    /// Borrow the underlying corpus handle.
    pub fn corpus(&self) -> &Corpus {
        &self.corpus
    }

    /// A fresh memo for one resolution.
    ///
    /// Scoped per resolver call rather than per adapter: an adapter outlives
    /// any one query, and caching the package list across queries would make
    /// a corpus insert invisible to the next one.
    fn memo(&self) -> Arc<CorpusMemo> {
        CorpusMemo::new(
            self.corpus.clone(),
            self.probe.clone(),
            self.empty_edge_log.clone(),
        )
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// A one-item stream carrying an error, for the arms that can only fail.
fn error_stream<'v>(e: Error) -> VertexStream<'v, Result<Vertex, Error>> {
    Box::pin(stream::once(async move { Err(e) }))
}

/// Construct the concrete [`Vertex`] variant for a symbol intro in a package.
pub(crate) fn vertex_for_intro(package: Arc<PackageView>, intro: IntroId) -> Result<Vertex, Error> {
    // Read the discriminant out *before* constructing the vertex: `entry`
    // borrows `package`, and `SymbolVertex` takes it by value. `discriminant()`
    // yields a `Copy` value, so the borrow ends on this statement.
    let discriminant = package
        .view()
        .entry(intro)
        .ok_or(Error::SymbolNotFound(intro))?
        .kind()
        .discriminant();

    let sv = SymbolVertex { package, intro };
    Ok(match discriminant {
        Some(KindDiscriminant::Function) => Vertex::Function(sv),
        Some(KindDiscriminant::Record) => Vertex::Record(sv),
        Some(KindDiscriminant::Trait) => Vertex::Trait(sv),
        Some(KindDiscriminant::Impl) => Vertex::Impl(sv),
        Some(KindDiscriminant::Enum) => Vertex::Enum(sv),
        Some(KindDiscriminant::Field) => Vertex::Field(sv),
        Some(KindDiscriminant::Const) => Vertex::Const(sv),
        Some(KindDiscriminant::Alias) => Vertex::Alias(sv),
        // The five formerly-collapsed kinds each now have their own variant so
        // that `... on Variant { }` / `... on Static { }` etc. type-coercions
        // in Trustfall queries work correctly.  A vertex type in the SDL that
        // the adapter never emits would silently return nothing for any query
        // that coerces to it — the exact failure we are preventing.
        Some(KindDiscriminant::Static) => Vertex::Static(sv),
        Some(KindDiscriminant::Variant) => Vertex::Variant(sv),
        Some(KindDiscriminant::Module) => Vertex::Module(sv),
        Some(KindDiscriminant::Reexport) => Vertex::Reexport(sv),
        Some(KindDiscriminant::Param) => Vertex::Param(sv),
        // `None` covers reference entries that carry no KindDiscriminant.
        // Any future discriminant the adapter does not recognise also lands here
        // so that queries written against the Symbol interface keep working.
        _ => Vertex::OtherSymbol(sv),
    })
}

/// The name a `Visibility` is published under in the schema.
///
/// Written out rather than `format!("{v:?}")` because these six strings are a
/// wire contract: an agent filtering `visibility @filter(op: "=", value:
/// ["$v"])` is matching against them, and a rename in `nudox-ir`'s `Debug`
/// output must not silently change what a query means.
fn visibility_name(v: Visibility) -> &'static str {
    match v {
        Visibility::Public => "Public",
        Visibility::Private => "Private",
        Visibility::Protected => "Protected",
        Visibility::Internal => "Internal",
        Visibility::Package => "Package",
        Visibility::Crate => "Crate",
    }
}

/// The name an `Unlocated` reason is published under in the schema.
///
/// Same contract as [`visibility_name`], and load-bearing for a different
/// reason: this string is the *only* thing distinguishing "we have no idea
/// where this is" from "this symbol has no source location because it does
/// not exist in source". Flattening the two to a null `file` is the defect
/// this field exists to prevent.
fn unlocated_name(u: Unlocated) -> &'static str {
    match u {
        Unlocated::Synthesized => "Synthesized",
        Unlocated::MacroExpanded => "MacroExpanded",
        Unlocated::ProducerRecordsNoLocation => "ProducerRecordsNoLocation",
        Unlocated::OutsideDocumentedPackage => "OutsideDocumentedPackage",
    }
}

/// The name a [`KeyTier`] is published under in the schema.
///
/// Same wire-contract reasoning as [`visibility_name`]: these four strings are
/// what a `keyTier @filter(op: "=", …)` matches against, so they are written
/// out rather than derived from `Debug`.
///
/// # Why `Unrecorded` is a fourth string and not a null
///
/// `None` here means the `PackageView` carries no [`SealReport`] — a
/// hand-built table, or an `IrView` reconstituted from a snapshot this process
/// never sealed. That is a different fact from `Structural`, and collapsing
/// the two is precisely the defect the whole key-provenance path exists to
/// remove: a caller must not be told "your key is content-derived" by a corpus
/// that never checked. Publishing it as a null instead would make the field
/// nullable and let a careless reader's `?? "Structural"` do the same damage.
///
/// [`SealReport`]: nudox_ir::package::SealReport
fn key_tier_name(tier: Option<KeyTier>) -> &'static str {
    match tier {
        Some(KeyTier::Structural) => "Structural",
        Some(KeyTier::Span) => "Span",
        Some(KeyTier::Ordinal) => "Ordinal",
        None => "Unrecorded",
    }
}

/// Resolve the shared Symbol interface properties from a [`SymbolVertex`].
fn symbol_property(sv: &SymbolVertex, property_name: &str) -> Result<FieldValue, Error> {
    let view = sv.package.view();
    let entry = view
        .entry(sv.intro)
        .ok_or(Error::SymbolNotFound(sv.intro))?;
    let sym = entry.sym();

    Ok(match property_name {
        "key" => FieldValue::String(sv.stable_ref().to_string().into()),
        // The two staleness fields. See `key_tier_name` for why `Unrecorded`
        // is a value here rather than a null, and the schema comment above
        // `interface Symbol` for what a caller does with each tier.
        "keyTier" => FieldValue::String(key_tier_name(sv.package.key_tier(sv.intro)).into()),
        // Null, not `false`: "we do not know how this key was minted" is
        // not "this key is fragile". Reporting the unknown as fragile
        // would make every fixture-backed corpus look unusable; reporting
        // it as sound would be the silent repair this field exists to
        // prevent.
        "keyIsContentDerived" => sv
            .package
            .key_tier(sv.intro)
            .map_or(FieldValue::Null, |tier| {
                FieldValue::Boolean(tier.is_content_derived())
            }),
        "name" => FieldValue::String(sym.name.clone().into()),
        "signature" => FieldValue::String(
            crate::chunk::signature::tokens_to_text(&crate::chunk::signature::tokens(
                entry,
                &sv.package,
            ))
            .into(),
        ),
        "path" => sv
            .package
            .indexes()
            .path_of(sv.intro)
            .map_or(FieldValue::Null, |p| FieldValue::String(p.as_ref().into())),
        "kind" => {
            let kind_str = entry
                .kind()
                .discriminant()
                .map_or_else(|| "Reference".to_string(), |d| format!("{d:?}"));
            FieldValue::String(kind_str.into())
        }
        "isPublic" => FieldValue::Boolean(matches!(sym.visibility, Visibility::Public)),
        // The IR distinguishes six visibilities; `isPublic` answers only
        // "is it Public", which makes a `pub(crate)` item and a genuinely
        // private one indistinguishable through the graph. Both fields are
        // published: `isPublic` because it is the existing contract, and
        // `visibility` because it is the faithful one.
        "visibility" => FieldValue::String(visibility_name(sym.visibility).into()),
        "documentation" => FieldValue::String(sym.documentation.clone().into()),
        "isDeprecated" => FieldValue::Boolean(sym.deprecation.is_some()),
        "deprecationNote" => match &sym.deprecation {
            Some(Deprecation { note: Some(n), .. }) => FieldValue::String(n.clone().into()),
            _ => FieldValue::Null,
        },
        "deprecationSince" => match &sym.deprecation {
            Some(Deprecation { since: Some(s), .. }) => FieldValue::String(s.clone().into()),
            _ => FieldValue::Null,
        },
        _ => {
            return Err(Error::UnknownProperty {
                ty: "Symbol".to_string(),
                prop: property_name.to_string(),
            });
        }
    })
}

/// Resolve a `SourceLocation` vertex's properties.
///
/// The three IR variants are projected without collapsing: `kind` always says
/// which one this is, and every other field is null exactly where the variant
/// has nothing to say. A consumer that reads only `file` still cannot tell
/// `Unlocated(MacroExpanded)` from `Unlocated(Synthesized)` — which is why it
/// must read `kind` and `unlocatedReason`, and why they are the two fields the
/// schema documents as load-bearing.
fn location_property(loc: &SourceLocation, property_name: &str) -> Result<FieldValue, Error> {
    let lines = loc.lines();
    Ok(match property_name {
        "kind" => FieldValue::String(
            match loc {
                SourceLocation::Declared { .. } => "Declared",
                SourceLocation::BytesOnly { .. } => "BytesOnly",
                SourceLocation::Unlocated(_) => "Unlocated",
            }
            .into(),
        ),
        "file" => loc
            .file()
            .map_or(FieldValue::Null, |f| FieldValue::String(f.as_str().into())),
        "byteStart" => loc.bytes().map_or(FieldValue::Null, |b| {
            FieldValue::Int64(b.as_range().start as i64)
        }),
        "byteEnd" => loc.bytes().map_or(FieldValue::Null, |b| {
            FieldValue::Int64(b.as_range().end as i64)
        }),
        "startLine" => lines.map_or(FieldValue::Null, |(s, _)| {
            FieldValue::Int64(i64::from(s.line()))
        }),
        "startColumn" => lines.map_or(FieldValue::Null, |(s, _)| {
            FieldValue::Int64(i64::from(s.column()))
        }),
        "endLine" => lines.map_or(FieldValue::Null, |(_, e)| {
            FieldValue::Int64(i64::from(e.line()))
        }),
        "endColumn" => lines.map_or(FieldValue::Null, |(_, e)| {
            FieldValue::Int64(i64::from(e.column()))
        }),
        "unlocatedReason" => match loc {
            SourceLocation::Unlocated(reason) => FieldValue::String(unlocated_name(*reason).into()),
            SourceLocation::Declared { .. } | SourceLocation::BytesOnly { .. } => FieldValue::Null,
        },
        _ => {
            return Err(Error::UnknownProperty {
                ty: "SourceLocation".to_string(),
                prop: property_name.to_string(),
            });
        }
    })
}

/// The one rendered-type property each vertex type publishes, if it has one.
///
/// A function rather than a `match` inlined into the resolver so that the
/// schema's naming rule is stated once and checkably: a property whose value
/// is **rendered type text** ends in `Str`, and one whose value is a
/// **`StableRef` key** (`Impl.ofTrait`, `Trait.supertraits`) does not. The two
/// are both `String` on the wire and a caller that mistakes one for the other
/// gets silence from `get_symbol`, so the distinction has to survive in the
/// name.
fn rendered_type_property(type_name: &str) -> Option<&'static str> {
    Some(match type_name {
        "Field" | "Const" | "Static" | "Param" => "typeStr",
        "Alias" => "targetStr",
        "Impl" => "selfTypeStr",
        _ => return None,
    })
}

/// Render a [`Type`] as the text a developer would have written, resolving
/// same-package nominals through `package`'s entry table.
///
/// # Why this is not `format!("{ty:?}")`
///
/// It used to be, on `Field.typeStr`, and that shipped
/// `Nominal(Intro(intro:3f1a9c2b…))` and
/// `Primitive(Integer { signed: true, width: Fixed(32) })` to MCP clients as
/// if they were rendered types. Five more type-valued fields (`Alias.target`,
/// `Impl.self_ty`, `Const.ty`, `Static.ty`, `Param.ty`) were left off this
/// schema entirely rather than replicate that, so the `Debug` dump was
/// costing the graph five answers as well as being wrong about one.
///
/// # Which renderer this is, and which one it is not
///
/// There are exactly **two** type renderers in this system, and they are not
/// interchangeable:
///
/// * [`nudox_ir::render`] — **this one**. A `Type` with no surrounding entry,
///   rendered to *one scalar string* for a program to read. Every
///   `Type::Unknown` reason renders as a distinct, marked token
///   (`?unannotated`, `?unresolved(Context)`, `?external(click.core.Context)`),
///   because a consumer that receives only a string has no other channel to
///   recover the reason from, and `?`-prefixing is what stops a hole from
///   being mistaken for a type the source wrote.
/// * `crate::chunk::signature::tokens` — the LR-4 renderer for a
///   *declaration signature*: typed [`SigToken`]s carrying per-nominal link
///   targets, built from an `Entry` and resolved against a `PackageView`. It
///   renders an unresolved `Context` as the bare word `Context`, and an
///   unannotated parameter as `?`, because it is read by a human looking at a
///   page and the name is the useful half.
///
/// The two disagree *on purpose* about the unknown lattice, which is why
/// neither can serve the other's caller. What LR-4 forbids — and what this
/// function exists to remove — is a **third** rendering path, and `Debug` in
/// any of them.
///
/// [`SigToken`]: https://docs.rs/nudox-engine
fn type_str(ty: &Type, package: &PackageView) -> String {
    // `render_with` falls back to its own marked placeholder when the resolver
    // returns `None`, so a missing entry degrades to `?ref(3f1a9c2b)` rather
    // than to a fabricated name. `Ref::Foreign` is deliberately not handled
    // here: the renderer already prints the producer's own spelling off
    // `ForeignKey::display`, which is a better answer than anything this
    // package's table could supply for a symbol that is not in it.
    let resolve = |r: &RawRef| match r {
        Ref::Intro(id) => package
            .view()
            .entry(*id)
            .map(|entry| entry.sym().name.clone()),
        Ref::Foreign { .. } | Ref::Local(_) => None,
    };
    ty.render_with(&resolve).to_string()
}

/// Extract the `StableRef` string of a `Type::Nominal` (or `Type::Apply`'s
/// base), relative to the given package lineage. Returns `None` for
/// non-nominal types.
fn type_to_stable_ref_str(ty: &Type, package: &PackageLineageId) -> Option<String> {
    match ty {
        Type::Nominal(raw_ref) => match raw_ref {
            Ref::Intro(id) => Some(StableRef::new(package.clone(), *id).to_string()),
            // A linked cross-package reference renders as its canonical
            // `ecosystem:name#introhex`. An unlinked one renders its
            // producer-canonical path (`core::clone::Clone`) rather than
            // `None`: a query for `Impl.ofTrait` should see *which* trait,
            // even when the trait's package is not in the corpus. Returning
            // `None` there is how the previous version silently shortened
            // every `Trait.supertraits` list instead of failing.
            Ref::Foreign { key, target } => Some(
                target
                    .as_ref()
                    .map_or_else(|| key.path.to_string(), std::string::ToString::to_string),
            ),
            Ref::Local(_) => None,
        },
        Type::Apply { base, .. } => type_to_stable_ref_str(base, package),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Plan execution
// ---------------------------------------------------------------------------

/// Run a [`SymbolPlan`] against the corpus.
///
/// Every arm is lazy per package: the package list is pulled once, and each
/// package's intro list is materialised only when the consumer has drained the
/// previous package's. A query that stops after one row therefore enumerates
/// one package, which is what `PackageEnumerated` in the probe measures.
fn run_symbol_plan<'v>(
    memo: Arc<CorpusMemo>,
    plan: SymbolPlan,
) -> VertexStream<'v, Result<Vertex, Error>> {
    match plan {
        SymbolPlan::Empty => Box::pin(stream::empty()),

        // O(keys) `Corpus::package` probes, deduplicated by lineage. No
        // package list is read at all.
        SymbolPlan::Keys(refs) => Box::pin(stream::iter(refs).then(move |sr| {
            let memo = Arc::clone(&memo);
            async move {
                let pkg = Arc::clone(&memo)
                    .package(sr.package.clone())
                    .await
                    .ok_or_else(|| Error::PackageNotLoaded(sr.package.clone()))?;
                vertex_for_intro(pkg, sr.intro)
            }
        })),

        SymbolPlan::Names(names) => per_package(memo, move |pkg, memo| {
            names
                .iter()
                .flat_map(|name| {
                    memo.record(StoreProbe::IndexProbe);
                    pkg.indexes()
                        .by_name
                        .get_exact(name)
                        .iter()
                        .map(|e| e.intro)
                        .collect::<Vec<_>>()
                })
                .collect()
        }),

        SymbolPlan::Kinds(discs) => per_package(memo, move |pkg, memo| {
            discs
                .iter()
                .flat_map(|disc| {
                    memo.record(StoreProbe::IndexProbe);
                    pkg.indexes().by_kind.get(disc).cloned().unwrap_or_default()
                })
                .collect()
        }),

        SymbolPlan::FullScan => {
            tracing::debug!(
                "Symbols entrypoint: no key/name/kind constraint and no type coercion — \
                 falling back to O(total symbols) full enumeration"
            );
            per_package(memo, |pkg, memo| {
                memo.record(StoreProbe::PackageEnumerated);
                // `entries_sorted`, not `entries`: the latter is `HashMap`
                // order, which is not stable across process launches. Every
                // projection a user can observe must be deterministic, and
                // this stream is the row order of every unfiltered `Symbols`
                // query.
                pkg.view().entries_sorted().map(|(id, _)| id).collect()
            })
        }
    }
}

/// Build a symbol stream by asking each package, in corpus order, for the
/// intros it contributes — pulling the next package only when the previous
/// one's rows have been consumed.
fn per_package<'v, F>(memo: Arc<CorpusMemo>, select: F) -> VertexStream<'v, Result<Vertex, Error>>
where
    F: Fn(&Arc<PackageView>, &CorpusMemo) -> Vec<IntroId> + 'v,
{
    // `Arc`, not a borrow: the inner `flat_map` closure outlives the outer
    // one (it is stored in the returned stream), so `select` has to be owned
    // by every level that calls it.
    let select = Arc::new(select);
    Box::pin(
        stream::once(Arc::clone(&memo).all_packages()).flat_map(move |pkgs| {
            let memo = Arc::clone(&memo);
            let select = Arc::clone(&select);
            stream::iter(pkgs.to_vec()).flat_map(move |pkg| {
                let intros = select(&pkg, &memo);
                stream::iter(
                    intros
                        .into_iter()
                        .map(move |intro| vertex_for_intro(Arc::clone(&pkg), intro)),
                )
            })
        }),
    )
}

/// Run a [`PackagePlan`] against the corpus.
fn run_package_plan<'v>(
    memo: Arc<CorpusMemo>,
    plan: PackagePlan,
) -> VertexStream<'v, Result<Vertex, Error>> {
    match plan {
        PackagePlan::Empty => Box::pin(stream::empty()),

        // A lineage that is not loaded yields no row rather than an error:
        // `Packages` enumerates what is present, and asking it for a package
        // that has not been indexed yet is a legitimate "not here", not a
        // malformed query. (`Symbols { key … }` is the opposite case and does
        // error — there the caller named a specific symbol.)
        PackagePlan::Lineages(ids) => Box::pin(
            stream::iter(ids)
                .then(move |id| {
                    let memo = Arc::clone(&memo);
                    async move { memo.package(id).await }
                })
                .filter_map(|found| async move { found.map(|p| Ok(Vertex::Package(p))) }),
        ),

        PackagePlan::All => Box::pin(
            stream::once(memo.all_packages())
                .flat_map(|pkgs| stream::iter(pkgs.to_vec()).map(|p| Ok(Vertex::Package(p)))),
        ),
    }
}

// ---------------------------------------------------------------------------
// Reverse-edge posting lists
// ---------------------------------------------------------------------------

/// Which per-package posting list a reverse-edge traversal reads.
///
/// `usages`, `mentions`, `Trait.implementors` and the five signature-position
/// edges differ only in this choice and in an error string; they are one
/// function, so an optimisation to the traversal cannot land on some of them
/// and miss the rest.
#[derive(Debug, Clone, Copy)]
enum Posting {
    /// Resolved-reference postings: body calls/reads plus declaration type use.
    Usages,
    /// Relational type references only — the trait an impl implements, a
    /// supertrait bound, a base class. This is the `mentions` edge, and it is
    /// deliberately narrower than "every type reference": see
    /// [`crate::store::package::PackageIndexes::mentions_of`].
    Mentions,
    /// One syntactic position of the type-reference index.
    ///
    /// Every reverse edge added for signature questions is this variant with a
    /// different [`TypePosition`], which is why none of them can answer a
    /// question a different position was asked.
    At(TypePosition),
}

impl Posting {
    fn probe(self, pkg: &PackageView, target: &StableRef) -> Vec<IntroId> {
        match self {
            Posting::Usages => pkg.indexes().usages_of(target).to_vec(),
            Posting::Mentions => pkg.indexes().mentions_of(target),
            Posting::At(position) => pkg.indexes().type_refs_in(target, position),
        }
    }
}

/// Resolve a reverse edge that fans out over every package's posting list.
///
/// The package list is read from [`CorpusMemo`], so `N` source vertices cost
/// one `Corpus::packages()` call between them rather than `N`.
fn posting_neighbors<'v, V: AsVertex<Vertex> + 'v>(
    contexts: ContextStream<'v, V>,
    memo: Arc<CorpusMemo>,
    posting: Posting,
    edge: &'static str,
) -> ContextOutcomeStream<'v, V, VertexStream<'v, Result<Vertex, Error>>> {
    async_helpers::try_resolve_neighbors_with(contexts, move |vertex| {
        let Some(sv) = crate::graph::vertex::as_symbol_vertex(vertex) else {
            return error_stream(Error::UnknownEdge {
                ty: "Symbol".to_string(),
                edge: edge.to_string(),
            });
        };
        let target = sv.stable_ref();
        let neighbors = per_package(Arc::clone(&memo), move |pkg, memo| {
            memo.record(StoreProbe::IndexProbe);
            posting.probe(pkg, &target)
        });
        // Covered edges explain an empty posting for *this* symbol. The
        // neighbor stream stays lazy; the note is recorded only once that
        // stream has been polled to the end without yielding a vertex.
        let Some(log) = memo
            .empty_edge_log
            .clone()
            .filter(|_| edge_empty::is_covered_edge(edge))
        else {
            return neighbors;
        };
        let symbol_name = sv
            .package
            .view()
            .entry(sv.intro)
            .map(|entry| entry.sym().name.clone())
            .unwrap_or_default();
        let symbol_pkg = Arc::clone(&sv.package);
        let memo = Arc::clone(&memo);
        note_if_empty(neighbors, move || async move {
            let packages = memo.all_packages().await;
            log.record(edge_empty::diagnose_empty_edge(
                edge,
                &symbol_name,
                symbol_pkg.as_ref(),
                packages.as_ref(),
            ));
        })
    })
}

/// Poll `inner` through, and run `on_empty` when it ends without a single item.
///
/// `on_empty` classifies the symbol whose posting list was just scanned. It
/// runs only after that scan, so the package list it reads is the one the
/// scan already cached.
fn note_if_empty<'v, Fut>(
    inner: VertexStream<'v, Result<Vertex, Error>>,
    on_empty: impl FnOnce() -> Fut + 'v,
) -> VertexStream<'v, Result<Vertex, Error>>
where
    Fut: Future<Output = ()> + 'v,
{
    Box::pin(NoteIfEmpty {
        inner,
        yielded: false,
        make_finish: Some(Box::new(move || {
            Box::pin(on_empty()) as Pin<Box<dyn Future<Output = ()> + 'v>>
        })),
        finish: None,
    })
}

struct NoteIfEmpty<'v> {
    inner: VertexStream<'v, Result<Vertex, Error>>,
    yielded: bool,
    make_finish: Option<Box<dyn FnOnce() -> Pin<Box<dyn Future<Output = ()> + 'v>> + 'v>>,
    finish: Option<Pin<Box<dyn Future<Output = ()> + 'v>>>,
}

impl<'v> Stream for NoteIfEmpty<'v> {
    type Item = Result<Vertex, Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.finish.is_none() {
            match this.inner.as_mut().poll_next(cx) {
                Poll::Ready(Some(item)) => {
                    this.yielded = true;
                    return Poll::Ready(Some(item));
                }
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None) if this.yielded => return Poll::Ready(None),
                Poll::Ready(None) => {
                    if let Some(make) = this.make_finish.take() {
                        this.finish = Some(make());
                    }
                }
            }
        }
        if let Some(finish) = this.finish.as_mut() {
            match finish.as_mut().poll(cx) {
                Poll::Pending => Poll::Pending,
                Poll::Ready(()) => {
                    this.finish = None;
                    Poll::Ready(None)
                }
            }
        } else {
            Poll::Ready(None)
        }
    }
}

// ---------------------------------------------------------------------------
// AsyncAdapter impl
// ---------------------------------------------------------------------------

impl<'vertex> AsyncAdapter<'vertex> for CorpusAdapter {
    type Vertex = Vertex;
    type Error = Error;

    // -----------------------------------------------------------------------
    // Starting vertices
    // -----------------------------------------------------------------------

    fn resolve_starting_vertices(
        &self,
        edge_name: &Arc<str>,
        _parameters: &EdgeParameters,
        resolve_info: &ResolveInfo,
    ) -> VertexStream<'vertex, Result<Vertex, Error>> {
        let memo = self.memo();

        match edge_name.as_ref() {
            "Packages" => {
                match plan_packages(resolve_info.statically_required_property("lineage")) {
                    Err(e) => error_stream(e),
                    Ok(plan) => {
                        tracing::debug!(?plan, "Packages entrypoint plan");
                        run_package_plan(memo, plan)
                    }
                }
            }

            "Symbols" => {
                let plan = plan_symbols(
                    resolve_info.statically_required_property("key"),
                    resolve_info.statically_required_property("name"),
                    resolve_info.statically_required_property("kind"),
                    resolve_info.coerced_to_type().map(AsRef::as_ref),
                );
                match plan {
                    Err(e) => error_stream(e),
                    Ok(plan) => {
                        tracing::debug!(?plan, "Symbols entrypoint plan");
                        run_symbol_plan(memo, plan)
                    }
                }
            }

            other => error_stream(Error::UnknownEdge {
                ty: "RootSchemaQuery".to_string(),
                edge: other.to_string(),
            }),
        }
    }

    // -----------------------------------------------------------------------
    // Property resolution
    // -----------------------------------------------------------------------

    fn resolve_property<V: AsVertex<Vertex> + 'vertex>(
        &self,
        contexts: ContextStream<'vertex, V>,
        type_name: &Arc<str>,
        property_name: &Arc<str>,
        _resolve_info: &ResolveInfo,
    ) -> ContextOutcomeStream<'vertex, V, Result<FieldValue, Error>> {
        let type_name = type_name.as_ref();
        let property_name = property_name.as_ref();

        // Handle `__typename` specially: the engine's resolver cannot provide
        // it; it is a virtual property derived from `Typename::typename()`.
        if property_name == "__typename" {
            return Box::pin(contexts.map(|ctx| match ctx.active_vertex::<Vertex>() {
                None => (ctx, Ok(FieldValue::Null)),
                Some(vertex) => {
                    let value: FieldValue = vertex.typename().into();
                    (ctx, Ok(value))
                }
            }));
        }

        match type_name {
            // ── Package properties ─────────────────────────────────────────
            "Package" => {
                let prop = property_name.to_string();
                async_helpers::try_resolve_property_with(contexts, move |vertex| {
                    let pkg = vertex.as_package().ok_or_else(|| Error::UnknownProperty {
                        ty: "Package".to_string(),
                        prop: prop.clone(),
                    })?;
                    Ok(match prop.as_str() {
                        "lineage" => FieldValue::String(pkg.lineage().to_string().into()),
                        "name" => {
                            FieldValue::String(pkg.lineage().name.as_str().to_string().into())
                        }
                        "ecosystem" => {
                            FieldValue::String(pkg.lineage().ecosystem.as_str().to_string().into())
                        }
                        _ => {
                            return Err(Error::UnknownProperty {
                                ty: "Package".to_string(),
                                prop: prop.clone(),
                            });
                        }
                    })
                })
            }

            // ── SourceLocation properties ──────────────────────────────────
            "SourceLocation" => {
                let prop = property_name.to_string();
                async_helpers::try_resolve_property_with(contexts, move |vertex| {
                    let sv = vertex
                        .as_source_location()
                        .ok_or_else(|| Error::UnknownProperty {
                            ty: "SourceLocation".to_string(),
                            prop: prop.clone(),
                        })?;
                    let view = sv.package.view();
                    let entry = view
                        .entry(sv.intro)
                        .ok_or(Error::SymbolNotFound(sv.intro))?;
                    location_property(entry.location(), &prop)
                })
            }

            // ── Occurrence properties ──────────────────────────────────────
            "Occurrence" => {
                let prop = property_name.to_string();
                async_helpers::try_resolve_property_with(contexts, move |vertex| {
                    let ov = vertex
                        .as_occurrence()
                        .ok_or_else(|| Error::UnknownProperty {
                            ty: "Occurrence".to_string(),
                            prop: prop.clone(),
                        })?;
                    let occs = ov.owner.package.view().occurrences_of(ov.owner.intro);
                    let occ = occs
                        .get(ov.occ_index)
                        .ok_or(Error::SymbolNotFound(ov.owner.intro))?;
                    Ok(match prop.as_str() {
                        "targetKey" => FieldValue::String(occ.target.to_string().into()),
                        "referenceKind" => FieldValue::String(format!("{:?}", occ.kind).into()),
                        "confidence" => FieldValue::String(format!("{:?}", occ.confidence).into()),
                        "spanStart" => FieldValue::Int64(i64::from(occ.span.start)),
                        "spanEnd" => FieldValue::Int64(i64::from(occ.span.end)),
                        _ => {
                            return Err(Error::UnknownProperty {
                                ty: "Occurrence".to_string(),
                                prop: prop.clone(),
                            });
                        }
                    })
                })
            }

            // ── Function-specific properties ───────────────────────────────
            "Function" if property_name == "isAsync" || property_name == "receiverKind" => {
                let prop = property_name.to_string();
                async_helpers::try_resolve_property_with(contexts, move |vertex| {
                    let sv = symbol_vertex_from(vertex, "Function")?;
                    let entry = sv
                        .package
                        .view()
                        .entry(sv.intro)
                        .ok_or(Error::SymbolNotFound(sv.intro))?;
                    match entry.kind().as_owned_kind() {
                        Some(Kind::Function(f)) => Ok(match prop.as_str() {
                            "isAsync" => {
                                FieldValue::Boolean(f.modifiers.contains(&FnModifier::Async))
                            }
                            "receiverKind" => {
                                let s = match f.receiver {
                                    None => "none",
                                    Some(Receiver::Owned) => "Owned",
                                    Some(Receiver::SharedRef) => "SharedRef",
                                    Some(Receiver::MutRef) => "MutRef",
                                    Some(Receiver::Arbitrary) => "Arbitrary",
                                };
                                FieldValue::String(s.into())
                            }
                            _ => unreachable!("already matched above"),
                        }),
                        _ => Ok(match prop.as_str() {
                            "isAsync" => FieldValue::Boolean(false),
                            "receiverKind" => FieldValue::String("none".into()),
                            _ => unreachable!(),
                        }),
                    }
                })
            }

            // ── The six rendered-type properties ───────────────────────────
            //
            // One arm, one renderer. `Field.typeStr` was the only one of these
            // that existed, it was a `Debug` dump, and the other five were left
            // off the schema *because* copying that dump was the only way to
            // add them. Resolving all six through [`type_str`] is what makes a
            // seventh cost nothing and makes a second renderer impossible to
            // add here by accident.
            //
            // Every one of them is nullable, and the null is load-bearing:
            // `Field::ty`, `Param::ty` and `Alias::target` are `Option<Type>`
            // in the IR, and "the producer recorded no type at all" is a
            // different fact from `Type::Unknown(Unannotated)`, which renders
            // as `?unannotated`. The old code returned the string `"?"` for
            // both, which is exactly the conflation the unknown lattice was
            // split to remove.
            "Field" | "Const" | "Static" | "Param" | "Alias" | "Impl"
                if rendered_type_property(type_name) == Some(property_name) =>
            {
                let ty_name = type_name.to_string();
                async_helpers::try_resolve_property_with(contexts, move |vertex| {
                    let sv = symbol_vertex_from(vertex, &ty_name)?;
                    let entry = sv
                        .package
                        .view()
                        .entry(sv.intro)
                        .ok_or(Error::SymbolNotFound(sv.intro))?;
                    // A reference entry (`as_owned_kind() == None`) carries no
                    // kind body and therefore no type; that is a null, not a
                    // rendering.
                    let ty: Option<&Type> = match entry.kind().as_owned_kind() {
                        Some(Kind::Field(f)) => f.ty.as_ref(),
                        Some(Kind::Const(c)) => Some(&c.ty),
                        Some(Kind::Static(s)) => Some(&s.ty),
                        Some(Kind::Param(p)) => p.ty.as_ref(),
                        Some(Kind::Alias(a)) => a.target.as_ref(),
                        Some(Kind::Impl(i)) => Some(&i.self_ty),
                        _ => None,
                    };
                    Ok(ty.map_or(FieldValue::Null, |ty| {
                        FieldValue::String(type_str(ty, &sv.package).into())
                    }))
                })
            }

            // ── Trait-specific properties ──────────────────────────────────
            "Trait" if property_name == "supertraits" => {
                async_helpers::try_resolve_property_with(contexts, |vertex| {
                    let sv = symbol_vertex_from(vertex, "Trait")?;
                    let entry = sv
                        .package
                        .view()
                        .entry(sv.intro)
                        .ok_or(Error::SymbolNotFound(sv.intro))?;
                    let keys: Vec<FieldValue> = match entry.kind().as_owned_kind() {
                        Some(Kind::Trait(t)) => t
                            .supers
                            .iter()
                            .filter_map(|ty| type_to_stable_ref_str(ty, sv.package.lineage()))
                            .map(|s| FieldValue::String(s.into()))
                            .collect(),
                        _ => vec![],
                    };
                    Ok(FieldValue::List(keys.into()))
                })
            }

            // ── Impl-specific properties ───────────────────────────────────
            "Impl" if property_name == "ofTrait" => {
                async_helpers::try_resolve_property_with(contexts, |vertex| {
                    let sv = symbol_vertex_from(vertex, "Impl")?;
                    let entry = sv
                        .package
                        .view()
                        .entry(sv.intro)
                        .ok_or(Error::SymbolNotFound(sv.intro))?;
                    Ok(match entry.kind().as_owned_kind() {
                        Some(Kind::Impl(impl_)) => impl_
                            .of
                            .as_ref()
                            .and_then(|ty| type_to_stable_ref_str(ty, sv.package.lineage()))
                            .map_or(FieldValue::Null, |s| FieldValue::String(s.into())),
                        _ => FieldValue::Null,
                    })
                })
            }

            // ── All symbol-interface properties (shared across concrete types)
            _ => {
                let prop = property_name.to_string();
                // Convert type_name to String to satisfy the 'vertex lifetime
                // bound on the resolver closure (type_name: &str is shorter).
                let ty_for_err = type_name.to_string();
                async_helpers::try_resolve_property_with(contexts, move |vertex| {
                    crate::graph::vertex::as_symbol_vertex(vertex).map_or_else(
                        || {
                            Err(Error::UnknownProperty {
                                ty: ty_for_err.clone(),
                                prop: prop.clone(),
                            })
                        },
                        |sv| symbol_property(sv, &prop),
                    )
                })
            }
        }
    }

    // -----------------------------------------------------------------------
    // Neighbor resolution
    // -----------------------------------------------------------------------

    fn resolve_neighbors<V: AsVertex<Vertex> + 'vertex>(
        &self,
        contexts: ContextStream<'vertex, V>,
        type_name: &Arc<str>,
        edge_name: &Arc<str>,
        _parameters: &EdgeParameters,
        _resolve_info: &ResolveEdgeInfo,
    ) -> ContextOutcomeStream<'vertex, V, VertexStream<'vertex, Result<Vertex, Error>>> {
        let memo = self.memo();
        let type_name = type_name.as_ref();
        let edge_name = edge_name.as_ref();

        match (type_name, edge_name) {
            // ── Package → members ──────────────────────────────────────────
            ("Package", "members") => {
                async_helpers::try_resolve_neighbors_with(contexts, move |vertex| {
                    vertex.as_package().map_or_else(
                        || {
                            error_stream(Error::UnknownEdge {
                                ty: "Package".to_string(),
                                edge: "members".to_string(),
                            })
                        },
                        |pkg| {
                            let pkg = pkg.clone();
                            // `entries_sorted`: `Package.members` is a user-
                            // visible row order, and `entries()` is hash order.
                            // The iterator borrows `pkg.view()`, so it must be
                            // collected before the returned stream outlives it.
                            #[allow(clippy::needless_collect)]
                            let intros: Vec<IntroId> =
                                pkg.view().entries_sorted().map(|(id, _)| id).collect();
                            Box::pin(stream::iter(
                                intros
                                    .into_iter()
                                    .map(move |intro| vertex_for_intro(pkg.clone(), intro)),
                            ))
                        },
                    )
                })
            }

            // ── Symbol → package ───────────────────────────────────────────
            (_, "package") => async_helpers::try_resolve_neighbors_with(contexts, move |vertex| {
                crate::graph::vertex::as_symbol_vertex(vertex).map_or_else(
                    || {
                        error_stream(Error::UnknownEdge {
                            ty: "Symbol".to_string(),
                            edge: "package".to_string(),
                        })
                    },
                    |sv| {
                        let pkg = sv.package.clone();
                        Box::pin(stream::once(async move { Ok(Vertex::Package(pkg)) }))
                    },
                )
            }),

            // ── Symbol → location ──────────────────────────────────────────
            //
            // A non-null edge: every entry has a `SourceLocation`, because the
            // IR has no way to represent an entry without one — the absent
            // case is the `Unlocated` variant, which is a *value*, not a null.
            // Modelling it as `location: SourceLocation!` rather than an
            // optional edge is what forces a reader to confront the reason.
            (_, "location") => async_helpers::try_resolve_neighbors_with(contexts, move |vertex| {
                crate::graph::vertex::as_symbol_vertex(vertex).map_or_else(
                    || {
                        error_stream(Error::UnknownEdge {
                            ty: "Symbol".to_string(),
                            edge: "location".to_string(),
                        })
                    },
                    |sv| {
                        let sv = sv.clone();
                        Box::pin(stream::once(async move { Ok(Vertex::SourceLocation(sv)) }))
                    },
                )
            }),

            // ── Symbol → members (children) ────────────────────────────────
            (_, "members") => async_helpers::try_resolve_neighbors_with(contexts, move |vertex| {
                crate::graph::vertex::as_symbol_vertex(vertex).map_or_else(
                    || {
                        error_stream(Error::UnknownEdge {
                            ty: "Symbol".to_string(),
                            edge: "members".to_string(),
                        })
                    },
                    |sv| {
                        let children: Vec<IntroId> =
                            sv.package.view().children_of(sv.intro).to_vec();
                        let pkg = sv.package.clone();
                        Box::pin(stream::iter(
                            children
                                .into_iter()
                                .map(move |child| vertex_for_intro(pkg.clone(), child)),
                        ))
                    },
                )
            }),

            // ── Symbol → parent ────────────────────────────────────────────
            (_, "parent") => async_helpers::try_resolve_neighbors_with(contexts, move |vertex| {
                crate::graph::vertex::as_symbol_vertex(vertex).map_or_else(
                    || {
                        error_stream(Error::UnknownEdge {
                            ty: "Symbol".to_string(),
                            edge: "parent".to_string(),
                        })
                    },
                    |sv| match sv.package.view().parent_of(sv.intro) {
                        None => Box::pin(stream::empty()),
                        Some(parent_intro) => {
                            let pkg = sv.package.clone();
                            Box::pin(stream::once(
                                async move { vertex_for_intro(pkg, parent_intro) },
                            ))
                        }
                    },
                )
            }),

            // ── Symbol → usages (resolved references across all packages) ──
            (_, "usages") => posting_neighbors(contexts, memo, Posting::Usages, "usages"),

            // ── Symbol → mentions (relational type references) ─────────────
            (_, "mentions") => posting_neighbors(contexts, memo, Posting::Mentions, "mentions"),

            // ── Trait → implementors ───────────────────────────────────────
            //
            // Exactly the `ImplementedTrait` position: an impl's `of` type is
            // a reference to the trait it implements, and nothing else is.
            //
            // This used to read the whole `mentions` list, which also carries
            // supertrait bounds — so `Display.implementors` reported every
            // `trait Foo: Display` as an implementor of `Display`, which is
            // not one. Naming the position removes the false positives rather
            // than filtering them out downstream.
            ("Trait", "implementors") => posting_neighbors(
                contexts,
                memo,
                Posting::At(TypePosition::ImplementedTrait),
                "implementors",
            ),

            // ── Symbol → the five signature-position reverse edges ─────────
            //
            // Each names one [`TypePosition`], because the questions they
            // answer have different answers: a function that *takes* `Y` is
            // not a function that *returns* it, and an impl *for* `Y` is not
            // an implementation *of* `Y`. Serving them from one undifferentiated
            // list is the conflation `mentions` would have become had these
            // references been added to it.
            (_, "implementedBy") => posting_neighbors(
                contexts,
                memo,
                Posting::At(TypePosition::ImplSelf),
                "implementedBy",
            ),
            (_, "subtypes") => posting_neighbors(
                contexts,
                memo,
                Posting::At(TypePosition::SuperType),
                "subtypes",
            ),
            (_, "returnedBy") => posting_neighbors(
                contexts,
                memo,
                Posting::At(TypePosition::Return),
                "returnedBy",
            ),
            (_, "acceptedBy") => posting_neighbors(
                contexts,
                memo,
                Posting::At(TypePosition::Parameter),
                "acceptedBy",
            ),
            (_, "heldBy") => posting_neighbors(
                contexts,
                memo,
                Posting::At(TypePosition::FieldType),
                "heldBy",
            ),

            // ── Symbol → signatureTypes (the forward direction) ────────────
            //
            // The only forward type edge in the schema, and the one the name
            // `mentions` has always misleadingly suggested. It reads the
            // declaration's own kind — no posting list is involved, because
            // the answer is written on the entry — so it is proportional to
            // the signature and costs the corpus one lookup per *distinct
            // target package*, memoised across the whole edge.
            //
            // Derived from the same `typerefs_of_entry` that builds the
            // reverse index, so the two directions cannot disagree about what
            // a signature references.
            (_, "signatureTypes") => {
                async_helpers::try_resolve_neighbors_with(contexts, move |vertex| {
                    let Some(sv) = crate::graph::vertex::as_symbol_vertex(vertex) else {
                        return error_stream(Error::UnknownEdge {
                            ty: "Symbol".to_string(),
                            edge: "signatureTypes".to_string(),
                        });
                    };
                    // Deduplicate by target: a type named in two positions
                    // (`impl Point` on `Point`, `fn f(a: T, b: T)`) is one
                    // neighbor, not two. `typerefs_of_entry` returns
                    // `(position, target)` order, so this keeps the first
                    // position's ordering and is deterministic.
                    let mut targets: Vec<StableRef> =
                        typerefs_of_entry(sv.package.view(), sv.intro)
                            .into_iter()
                            .map(|TypeRef { target, .. }| target)
                            .collect();
                    targets.sort_unstable();
                    targets.dedup();

                    let memo = Arc::clone(&memo);
                    Box::pin(
                        stream::iter(targets)
                            .then(move |sr| {
                                let memo = Arc::clone(&memo);
                                async move {
                                    // A target whose package is not loaded
                                    // yields no neighbor rather than an error,
                                    // matching `Occurrence.target`: a
                                    // partially-loaded corpus is the normal
                                    // case, not a malformed query.
                                    let Some(pkg) = memo.package(sr.package.clone()).await else {
                                        return Ok(None);
                                    };
                                    Ok(Some(vertex_for_intro(pkg, sr.intro)?))
                                }
                            })
                            .flat_map(|result| match result {
                                Err(e) => error_stream(e),
                                Ok(None) => Box::pin(stream::empty())
                                    as VertexStream<'vertex, Result<Vertex, Error>>,
                                Ok(Some(v)) => Box::pin(stream::once(async move { Ok(v) })),
                            }),
                    )
                })
            }

            // ── Symbol → occurrencesOf ─────────────────────────────────────
            (_, "occurrencesOf") => {
                async_helpers::try_resolve_neighbors_with(contexts, move |vertex| {
                    crate::graph::vertex::as_symbol_vertex(vertex).map_or_else(
                        || {
                            error_stream(Error::UnknownEdge {
                                ty: "Symbol".to_string(),
                                edge: "occurrencesOf".to_string(),
                            })
                        },
                        |sv| {
                            let count = sv.package.view().occurrences_of(sv.intro).len();
                            let sv_clone = sv.clone();
                            Box::pin(stream::iter((0..count).map(move |idx| {
                                Ok(Vertex::Occurrence(OccurrenceVertex {
                                    owner: sv_clone.clone(),
                                    occ_index: idx,
                                }))
                            })))
                        },
                    )
                })
            }

            // ── Occurrence → owner ─────────────────────────────────────────
            //
            // `Occurrence.spanStart`/`spanEnd` are offsets *relative to the
            // owning entry's span start* (`nudox_ir::vocab::RelSpan`). Without
            // this edge the graph published two numbers a caller could not
            // turn into a position, because the base they are relative to was
            // unreachable: `owner { location { byteStart } }` is that base.
            ("Occurrence", "owner") => {
                async_helpers::try_resolve_neighbors_with(contexts, move |vertex| {
                    vertex.as_occurrence().map_or_else(
                        || {
                            error_stream(Error::UnknownEdge {
                                ty: "Occurrence".to_string(),
                                edge: "owner".to_string(),
                            })
                        },
                        |ov| {
                            let owner = ov.owner.clone();
                            Box::pin(stream::once(async move {
                                vertex_for_intro(owner.package, owner.intro)
                            }))
                        },
                    )
                })
            }

            // ── Occurrence → target ────────────────────────────────────────
            //
            // Resolves the `targetKey` string into a full `Symbol` vertex so
            // that a single query can traverse:
            //
            //   occurrencesOf { target { name kind } }
            //
            // without a second round-trip query filtered by key.  The edge is
            // optional in the SDL — when the target's package is not loaded in
            // the corpus yet we yield zero neighbors rather than an error, so
            // queries stay robust against partially-loaded corpora.
            //
            // Cost: one `Corpus::package` probe per *distinct target package*,
            // not per occurrence — `CorpusMemo` caches the misses as well as
            // the hits, which is what makes a partially-loaded corpus cheap
            // rather than the worst case.
            ("Occurrence", "target") => {
                async_helpers::try_resolve_neighbors_with(contexts, move |vertex| {
                    let Some(ov) = vertex.as_occurrence() else {
                        return error_stream(Error::UnknownEdge {
                            ty: "Occurrence".to_string(),
                            edge: "target".to_string(),
                        });
                    };
                    // Read the target StableRef out of the occurrence slice.
                    // `occurrences_of` borrows `ov.owner.package`, so we must
                    // clone the target before `ov` is moved into the async block.
                    let target_sr = {
                        let occs = ov.owner.package.view().occurrences_of(ov.owner.intro);
                        match occs.get(ov.occ_index) {
                            Some(occ) => occ.target.clone(),
                            None => {
                                // Occurrence index is out of range — this is a
                                // programming error (the index is computed from the
                                // same slice), but we return empty rather than
                                // panicking so the query terminates cleanly.
                                return Box::pin(stream::empty());
                            }
                        }
                    };
                    let memo = Arc::clone(&memo);
                    Box::pin(
                        stream::once(async move {
                            let Some(pkg) = memo.package(target_sr.package.clone()).await else {
                                return Ok(None);
                            };
                            // Build the vertex.  A missing intro in a loaded package
                            // is a corpus inconsistency; surface it as an error so
                            // the caller can diagnose it rather than silently losing
                            // the row.
                            Ok(Some(vertex_for_intro(pkg, target_sr.intro)?))
                        })
                        // `stream::once` yields `Result<Option<Vertex>, Error>`.
                        // We must flatten the `Option` into a zero-or-one element
                        // stream without losing the `Result` wrapper.
                        .flat_map(|result| match result {
                            Err(e) => error_stream(e),
                            Ok(None) => Box::pin(stream::empty())
                                as VertexStream<'vertex, Result<Vertex, Error>>,
                            Ok(Some(v)) => Box::pin(stream::once(async move { Ok(v) })),
                        }),
                    )
                })
            }

            (ty, edge) => {
                let ty_owned = ty.to_string();
                let edge_owned = edge.to_string();
                async_helpers::try_resolve_neighbors_with(
                    contexts,
                    move |_vertex| -> VertexStream<'vertex, Result<Vertex, Error>> {
                        error_stream(Error::UnknownEdge {
                            ty: ty_owned.clone(),
                            edge: edge_owned.clone(),
                        })
                    },
                )
            }
        }
    }

    // -----------------------------------------------------------------------
    // Coercion
    // -----------------------------------------------------------------------

    fn resolve_coercion<V: AsVertex<Vertex> + 'vertex>(
        &self,
        contexts: ContextStream<'vertex, V>,
        _type_name: &Arc<str>,
        coerce_to_type: &Arc<str>,
        _resolve_info: &ResolveInfo,
    ) -> ContextOutcomeStream<'vertex, V, Result<bool, Error>> {
        let target = coerce_to_type.to_string();
        async_helpers::try_resolve_coercion_with(contexts, move |vertex| {
            Ok(vertex.typename() == target.as_str())
        })
    }
}

// ---------------------------------------------------------------------------
// Internal vertex extraction helpers
// ---------------------------------------------------------------------------

/// Extract a `SymbolVertex` from a generic vertex, returning an error if the
/// vertex is not a symbol type.
fn symbol_vertex_from<'v>(vertex: &'v Vertex, ty: &str) -> Result<&'v SymbolVertex, Error> {
    crate::graph::vertex::as_symbol_vertex(vertex).ok_or_else(|| Error::UnknownEdge {
        ty: ty.to_string(),
        edge: String::new(),
    })
}
