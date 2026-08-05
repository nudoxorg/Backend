//! [`CorpusAdapter`] — the Trustfall `AsyncAdapter` over the IR corpus.
//!
//! # Error contract (LR-6)
//!
//! No `unwrap` or `expect` in resolution paths. Every fallible step returns
//! `Result<_, GraphError>`. Errors propagate via the stream items and are
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
//! `resolve_starting_vertices` for the `Symbols` entrypoint inspects the
//! `ResolveInfo` passed by the Trustfall engine and uses
//! [`VertexInfo::statically_required_property`] to detect equality filters on
//! `key`, `name`, and `kind` before any iteration.  When a usable filter is
//! found, the resolver routes directly to the relevant `nudox-store` index
//! (corpus `entry()`, `NameIndex`, or `by_kind` map) instead of enumerating
//! every symbol.  When no usable filter is present the resolver falls back to
//! full enumeration and emits a `tracing::debug!` so slow queries are
//! diagnosable.
//!
//! # Why `AsyncAdapter` directly, not `AsyncBasicAdapter`
//!
//! `AsyncBasicAdapter::resolve_starting_vertices` does not receive a
//! `&ResolveInfo`, so there is no way to inspect filter hints through that
//! interface.  The blanket `impl<T: AsyncBasicAdapter> AsyncAdapter for T`
//! drops `_resolve_info` on the floor.  We implement `AsyncAdapter` directly
//! so that `Symbols` resolution sees the hints and can push equality filters
//! down to the appropriate indexes.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use futures::{StreamExt as _, stream};
use nudox_ir::{
    change::{EcosystemId, IntroId, PackageLineageId, PackageName, StableRef},
    entry::Visibility,
    index::Ref,
    kind::{Kind, KindDiscriminant},
    kinds::{FnModifier, Receiver, Type},
};
use nudox_store::{corpus::Corpus, package::PackageView};
use thiserror::Error;
use trustfall::FieldValue;
use trustfall::provider::async_helpers;
use trustfall::provider::{
    AsVertex, AsyncAdapter, CandidateValue, ContextOutcomeStream, ContextStream, EdgeParameters,
    ResolveEdgeInfo, ResolveInfo, Typename as _, VertexInfo as _, VertexStream,
};

use crate::vertex::{OccurrenceVertex, SymbolVertex, Vertex};

// ---------------------------------------------------------------------------
// GraphError
// ---------------------------------------------------------------------------

/// Errors that the [`CorpusAdapter`] may emit during query execution.
///
/// `#[non_exhaustive]` so that new variants can be added in minor releases
/// without breaking downstream `match` arms.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum GraphError {
    #[error("package not loaded: {0}")]
    PackageNotLoaded(PackageLineageId),

    #[error("symbol not found: {0:?}")]
    SymbolNotFound(IntroId),

    #[error("invalid stable-ref key: {0:?}")]
    InvalidKey(String),

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
// CorpusAdapter
// ---------------------------------------------------------------------------

/// The Trustfall async adapter over the local IR corpus.
///
/// Cheap to clone — holds only an `Arc`-backed [`Corpus`] and an optional
/// `Arc<AtomicUsize>` probe counter used by pushdown regression tests.
#[derive(Clone)]
pub struct CorpusAdapter {
    corpus: Corpus,
    /// Optional counter incremented each time `packages()` is called on the
    /// full-scan fallback path inside `Symbols` resolution.  Used by tests
    /// to verify that the O(1) pushdown path was taken.
    pub(crate) packages_scanned: Option<Arc<AtomicUsize>>,
}

impl CorpusAdapter {
    /// Construct an adapter wrapping the given corpus.
    pub fn new(corpus: Corpus) -> Self {
        Self {
            corpus,
            packages_scanned: None,
        }
    }

    /// Construct an adapter with a probe counter.
    ///
    /// Every time the `Symbols` entrypoint falls back to a full-scan (i.e. no
    /// usable equality filter was found on `key`, `name`, or `kind`), the
    /// counter is incremented by the number of packages enumerated.  Use this
    /// variant in tests that assert pushdown is happening.
    pub fn new_with_counter(corpus: Corpus, counter: Arc<AtomicUsize>) -> Self {
        Self {
            corpus,
            packages_scanned: Some(counter),
        }
    }

    /// Borrow the underlying corpus handle.
    pub fn corpus(&self) -> &Corpus {
        &self.corpus
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Parse `"ecosystem:name"` into a [`PackageLineageId`].
fn parse_lineage(s: &str) -> Option<PackageLineageId> {
    let (eco, name) = s.split_once(':')?;
    Some(PackageLineageId::new(
        EcosystemId::new(eco),
        PackageName::new(name),
    ))
}

/// Parse `"ecosystem:name#introhex"` into a [`StableRef`].
fn parse_stable_ref(s: &str) -> Option<StableRef> {
    let (pkg_str, intro_hex) = s.split_once('#')?;
    let lineage = parse_lineage(pkg_str)?;
    let bytes = parse_hex_32(intro_hex)?;
    Some(StableRef::new(lineage, IntroId::from_raw(bytes)))
}

/// Decode exactly 64 lowercase hex chars into 32 bytes.
fn parse_hex_32(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
        let hi = (chunk[0] as char).to_digit(16)? as u8;
        let lo = (chunk[1] as char).to_digit(16)? as u8;
        out[i] = (hi << 4) | lo;
    }
    Some(out)
}

/// Match a kind-discriminant name string to the enum variant.
fn kind_disc_from_str(s: &str) -> Option<KindDiscriminant> {
    match s {
        "Module" => Some(KindDiscriminant::Module),
        "Record" => Some(KindDiscriminant::Record),
        "Field" => Some(KindDiscriminant::Field),
        "Function" => Some(KindDiscriminant::Function),
        "Alias" => Some(KindDiscriminant::Alias),
        "Trait" => Some(KindDiscriminant::Trait),
        "Impl" => Some(KindDiscriminant::Impl),
        "Enum" => Some(KindDiscriminant::Enum),
        "Variant" => Some(KindDiscriminant::Variant),
        "Const" => Some(KindDiscriminant::Const),
        "Static" => Some(KindDiscriminant::Static),
        "Reexport" => Some(KindDiscriminant::Reexport),
        "Param" => Some(KindDiscriminant::Param),
        _ => None,
    }
}

/// Construct the concrete [`Vertex`] variant for a symbol intro in a package.
pub(crate) fn vertex_for_intro(
    package: Arc<PackageView>,
    intro: IntroId,
) -> Result<Vertex, GraphError> {
    // Read the discriminant out *before* constructing the vertex: `entry`
    // borrows `package`, and `SymbolVertex` takes it by value. `discriminant()`
    // yields a `Copy` value, so the borrow ends on this statement.
    let discriminant = package
        .view()
        .entry(intro)
        .ok_or(GraphError::SymbolNotFound(intro))?
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

/// Resolve the shared Symbol interface properties from a [`SymbolVertex`].
fn symbol_property(sv: &SymbolVertex, property_name: &str) -> Result<FieldValue, GraphError> {
    let view = sv.package.view();
    let entry = view
        .entry(sv.intro)
        .ok_or(GraphError::SymbolNotFound(sv.intro))?;
    let sym = entry.sym();

    Ok(match property_name {
        "key" => FieldValue::String(sv.stable_ref().to_string().into()),
        "name" => FieldValue::String(sym.name.clone().into()),
        "path" => sv
            .package
            .indexes()
            .path_of(sv.intro)
            .map(|p| FieldValue::String(p.as_ref().into()))
            .unwrap_or(FieldValue::Null),
        "kind" => {
            let kind_str = entry
                .kind()
                .discriminant()
                .map(|d| format!("{d:?}"))
                .unwrap_or_else(|| "Reference".to_string());
            FieldValue::String(kind_str.into())
        }
        "isPublic" => FieldValue::Boolean(matches!(sym.visibility, Visibility::Public)),
        "documentation" => FieldValue::String(sym.documentation.clone().into()),
        "isDeprecated" => FieldValue::Boolean(sym.deprecation.is_some()),
        _ => {
            return Err(GraphError::UnknownProperty {
                ty: "Symbol".to_string(),
                prop: property_name.to_string(),
            });
        }
    })
}

/// Extract the `StableRef` string of a `Type::Nominal` (or `Type::Apply`'s
/// base), relative to the given package lineage. Returns `None` for
/// non-nominal types.
fn type_to_stable_ref_str(ty: &Type, package: &PackageLineageId) -> Option<String> {
    match ty {
        Type::Nominal(raw_ref) => match raw_ref {
            Ref::Intro(id) => Some(StableRef::new(package.clone(), *id).to_string()),
            Ref::Foreign(sr) => Some(sr.to_string()),
            Ref::Local(_) => None,
        },
        Type::Apply { base, .. } => type_to_stable_ref_str(base, package),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// AsyncAdapter impl
// ---------------------------------------------------------------------------

impl<'vertex> AsyncAdapter<'vertex> for CorpusAdapter {
    type Vertex = Vertex;
    type Error = GraphError;

    // -----------------------------------------------------------------------
    // Starting vertices
    //
    // `Symbols` inspects `resolve_info` for statically-known filter values
    // before deciding which corpus index to use.
    // -----------------------------------------------------------------------

    fn resolve_starting_vertices(
        &self,
        edge_name: &Arc<str>,
        _parameters: &EdgeParameters,
        resolve_info: &ResolveInfo,
    ) -> VertexStream<'vertex, Result<Vertex, GraphError>> {
        let corpus = self.corpus.clone();
        let counter = self.packages_scanned.clone();

        match edge_name.as_ref() {
            // ── Packages ────────────────────────────────────────────────────
            //
            // Cost: O(packages) — always an index scan over the package map.
            // Add `@filter(op: "=", value: ["$lineage"])` on `lineage` to
            // narrow, or traverse `Packages → members` instead of `Symbols`
            // when a single-package filter is needed.
            "Packages" => Box::pin(
                stream::once(async move { corpus.packages().await }).flat_map(|pkgs| {
                    stream::iter(pkgs.into_iter().map(|p| Ok(Vertex::Package(p))))
                }),
            ),

            // ── Symbols ──────────────────────────────────────────────────────
            //
            // Pushdown priority (first matching branch wins):
            //   1. Equality on `key`  → O(1) via `Corpus::entry`.
            //   2. Equality on `name` → O(packages × log n) via `NameIndex::get_exact`.
            //   3. Equality on `kind` → O(packages × 1) via `by_kind` map.
            //   4. No usable filter   → O(total symbols) full scan.
            //      A `tracing::debug!` is emitted so slow queries are diagnosable.
            //
            // API used: `ResolveInfo::statically_required_property` from
            // `/tmp/tf-probe/trustfall_core/src/interpreter/hints/vertex_info.rs:65`
            // returns `Some(CandidateValue::Single(v))` when a single equality
            // filter with a query-variable operand is present on the property.
            "Symbols" => {
                // ── Pushdown 1: equality on `key` ──────────────────────────
                //
                // `statically_required_property("key")` returns
                // `Some(CandidateValue::Single(...))` when the query has
                // `@filter(op: "=", value: ["$key"])` and `$key` is bound.
                // We parse the StableRef, look up the package, and yield the
                // single matching vertex — bypassing `packages()` entirely.
                if let Some(CandidateValue::Single(FieldValue::String(key_val))) =
                    resolve_info.statically_required_property("key")
                {
                    let key_str = key_val.to_string();
                    return Box::pin(stream::once(async move {
                        let sr = parse_stable_ref(&key_str)
                            .ok_or_else(|| GraphError::InvalidKey(key_str.clone()))?;
                        let pkg = corpus
                            .package(&sr.package)
                            .await
                            .ok_or_else(|| GraphError::PackageNotLoaded(sr.package.clone()))?;
                        vertex_for_intro(pkg, sr.intro)
                    }));
                }

                // ── Pushdown 2: equality on `name` ─────────────────────────
                //
                // Routes to `NameIndex::get_exact` per package instead of
                // iterating every entry.
                if let Some(CandidateValue::Single(FieldValue::String(name_val))) =
                    resolve_info.statically_required_property("name")
                {
                    let name_lower = name_val.to_lowercase();
                    return Box::pin(
                        stream::once(async move { corpus.packages().await }).flat_map(
                            move |pkgs| {
                                let name = name_lower.clone();
                                let pairs: Vec<(Arc<PackageView>, IntroId)> = pkgs
                                    .into_iter()
                                    .flat_map(|pkg| {
                                        let intros: Vec<IntroId> = pkg
                                            .indexes()
                                            .by_name
                                            .get_exact(&name)
                                            .iter()
                                            .map(|e| e.intro)
                                            .collect();
                                        intros.into_iter().map(move |i| (pkg.clone(), i))
                                    })
                                    .collect();
                                stream::iter(
                                    pairs
                                        .into_iter()
                                        .map(|(pkg, intro)| vertex_for_intro(pkg, intro)),
                                )
                            },
                        ),
                    );
                }

                // ── Pushdown 3: equality on `kind` ─────────────────────────
                //
                // Routes to `by_kind` map per package: O(packages) lookups
                // instead of O(total symbols) entry scans.
                if let Some(CandidateValue::Single(FieldValue::String(kind_val))) =
                    resolve_info.statically_required_property("kind")
                {
                    let kind_str = kind_val.to_string();
                    return Box::pin(
                        stream::once(async move {
                            let disc = kind_disc_from_str(&kind_str)
                                .ok_or_else(|| GraphError::InvalidKind(kind_str.clone()))?;
                            Ok((corpus.packages().await, disc))
                        })
                        .flat_map(|result| match result {
                            Err(e) => Box::pin(stream::once(async move { Err(e) }))
                                as VertexStream<'_, Result<Vertex, GraphError>>,
                            Ok((pkgs, disc)) => Box::pin({
                                let pairs: Vec<(Arc<PackageView>, IntroId)> = pkgs
                                    .into_iter()
                                    .flat_map(|pkg| {
                                        let intros: Vec<IntroId> = pkg
                                            .indexes()
                                            .by_kind
                                            .get(&disc)
                                            .cloned()
                                            .unwrap_or_default();
                                        intros.into_iter().map(move |i| (pkg.clone(), i))
                                    })
                                    .collect();
                                stream::iter(
                                    pairs
                                        .into_iter()
                                        .map(|(pkg, intro)| vertex_for_intro(pkg, intro)),
                                )
                            }),
                        }),
                    );
                }

                // ── Fallback: full enumeration ──────────────────────────────
                //
                // No equality filter on `key`, `name`, or `kind` was found.
                // Emit a diagnostic log so slow queries are identifiable.
                tracing::debug!(
                    "Symbols entrypoint: no equality filter on `key`, `name`, or `kind` — \
                     falling back to O(total symbols) full enumeration"
                );
                Box::pin(
                    stream::once(async move { corpus.packages().await }).flat_map(move |pkgs| {
                        // Increment the probe counter by the number of packages
                        // being enumerated.  Only the full-scan path increments
                        // this; pushdown paths bypass `packages()` entirely.
                        if let Some(ref c) = counter {
                            c.fetch_add(pkgs.len(), Ordering::Relaxed);
                        }
                        let pairs: Vec<(Arc<PackageView>, IntroId)> = pkgs
                            .into_iter()
                            .flat_map(|pkg| {
                                let intros: Vec<IntroId> =
                                    pkg.view().entries().map(|(id, _)| id).collect();
                                intros.into_iter().map(move |i| (pkg.clone(), i))
                            })
                            .collect();
                        stream::iter(
                            pairs
                                .into_iter()
                                .map(|(pkg, intro)| vertex_for_intro(pkg, intro)),
                        )
                    }),
                )
            }

            other => {
                let msg = other.to_string();
                Box::pin(stream::once(async move {
                    Err(GraphError::UnknownEdge {
                        ty: "RootSchemaQuery".to_string(),
                        edge: msg,
                    })
                }))
            }
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
    ) -> ContextOutcomeStream<'vertex, V, Result<FieldValue, GraphError>> {
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
                    let pkg = vertex
                        .as_package()
                        .ok_or_else(|| GraphError::UnknownProperty {
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
                            return Err(GraphError::UnknownProperty {
                                ty: "Package".to_string(),
                                prop: prop.clone(),
                            });
                        }
                    })
                })
            }

            // ── Occurrence properties ──────────────────────────────────────
            "Occurrence" => {
                let prop = property_name.to_string();
                async_helpers::try_resolve_property_with(contexts, move |vertex| {
                    let ov = vertex
                        .as_occurrence()
                        .ok_or_else(|| GraphError::UnknownProperty {
                            ty: "Occurrence".to_string(),
                            prop: prop.clone(),
                        })?;
                    let occs = ov.owner.package.view().occurrences_of(ov.owner.intro);
                    let occ = occs
                        .get(ov.occ_index)
                        .ok_or_else(|| GraphError::SymbolNotFound(ov.owner.intro))?;
                    Ok(match prop.as_str() {
                        "targetKey" => FieldValue::String(occ.target.to_string().into()),
                        "referenceKind" => FieldValue::String(format!("{:?}", occ.kind).into()),
                        "confidence" => FieldValue::String(format!("{:?}", occ.confidence).into()),
                        "spanStart" => FieldValue::Int64(occ.span.start as i64),
                        "spanEnd" => FieldValue::Int64(occ.span.end as i64),
                        _ => {
                            return Err(GraphError::UnknownProperty {
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
                        .ok_or(GraphError::SymbolNotFound(sv.intro))?;
                    match entry.kind().as_owned_kind() {
                        Some(Kind::Function(f)) => Ok(match prop.as_str() {
                            "isAsync" => FieldValue::Boolean(
                                f.modifiers.iter().any(|m| *m == FnModifier::Async),
                            ),
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

            // ── Field-specific properties ──────────────────────────────────
            "Field" if property_name == "typeStr" => {
                async_helpers::try_resolve_property_with(contexts, |vertex| {
                    let sv = symbol_vertex_from(vertex, "Field")?;
                    let entry = sv
                        .package
                        .view()
                        .entry(sv.intro)
                        .ok_or(GraphError::SymbolNotFound(sv.intro))?;
                    let type_str = match entry.kind().as_owned_kind() {
                        Some(Kind::Field(f)) => {
                            f.ty.as_ref()
                                .map(|t| format!("{t:?}"))
                                .unwrap_or_else(|| "?".to_string())
                        }
                        _ => "?".to_string(),
                    };
                    Ok(FieldValue::String(type_str.into()))
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
                        .ok_or(GraphError::SymbolNotFound(sv.intro))?;
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
                        .ok_or(GraphError::SymbolNotFound(sv.intro))?;
                    Ok(match entry.kind().as_owned_kind() {
                        Some(Kind::Impl(impl_)) => impl_
                            .of
                            .as_ref()
                            .and_then(|ty| type_to_stable_ref_str(ty, sv.package.lineage()))
                            .map(|s| FieldValue::String(s.into()))
                            .unwrap_or(FieldValue::Null),
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
                    match extract_symbol_vertex(vertex) {
                        Some(sv) => symbol_property(sv, &prop),
                        None => Err(GraphError::UnknownProperty {
                            ty: ty_for_err.clone(),
                            prop: prop.clone(),
                        }),
                    }
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
    ) -> ContextOutcomeStream<'vertex, V, VertexStream<'vertex, Result<Vertex, GraphError>>> {
        let corpus = self.corpus.clone();
        let type_name = type_name.as_ref();
        let edge_name = edge_name.as_ref();

        match (type_name, edge_name) {
            // ── Package → members ──────────────────────────────────────────
            ("Package", "members") => {
                async_helpers::try_resolve_neighbors_with(contexts, move |vertex| {
                    match vertex.as_package() {
                        None => {
                            let e = GraphError::UnknownEdge {
                                ty: "Package".to_string(),
                                edge: "members".to_string(),
                            };
                            Box::pin(stream::once(async move { Err(e) }))
                                as VertexStream<'vertex, Result<Vertex, GraphError>>
                        }
                        Some(pkg) => {
                            let pkg = pkg.clone();
                            let intros: Vec<IntroId> =
                                pkg.view().entries().map(|(id, _)| id).collect();
                            Box::pin(stream::iter(
                                intros
                                    .into_iter()
                                    .map(move |intro| vertex_for_intro(pkg.clone(), intro)),
                            ))
                        }
                    }
                })
            }

            // ── Symbol → package ───────────────────────────────────────────
            (_, "package") => async_helpers::try_resolve_neighbors_with(contexts, move |vertex| {
                match extract_symbol_vertex(vertex) {
                    None => {
                        let e = GraphError::UnknownEdge {
                            ty: "Symbol".to_string(),
                            edge: "package".to_string(),
                        };
                        Box::pin(stream::once(async move { Err(e) }))
                            as VertexStream<'vertex, Result<Vertex, GraphError>>
                    }
                    Some(sv) => {
                        let pkg = sv.package.clone();
                        Box::pin(stream::once(async move { Ok(Vertex::Package(pkg)) }))
                    }
                }
            }),

            // ── Symbol → members (children) ────────────────────────────────
            (_, "members") => async_helpers::try_resolve_neighbors_with(contexts, move |vertex| {
                match extract_symbol_vertex(vertex) {
                    None => {
                        let e = GraphError::UnknownEdge {
                            ty: "Symbol".to_string(),
                            edge: "members".to_string(),
                        };
                        Box::pin(stream::once(async move { Err(e) }))
                            as VertexStream<'vertex, Result<Vertex, GraphError>>
                    }
                    Some(sv) => {
                        let children: Vec<IntroId> =
                            sv.package.view().children_of(sv.intro).to_vec();
                        let pkg = sv.package.clone();
                        Box::pin(stream::iter(
                            children
                                .into_iter()
                                .map(move |child| vertex_for_intro(pkg.clone(), child)),
                        ))
                    }
                }
            }),

            // ── Symbol → parent ────────────────────────────────────────────
            (_, "parent") => async_helpers::try_resolve_neighbors_with(contexts, move |vertex| {
                match extract_symbol_vertex(vertex) {
                    None => {
                        let e = GraphError::UnknownEdge {
                            ty: "Symbol".to_string(),
                            edge: "parent".to_string(),
                        };
                        Box::pin(stream::once(async move { Err(e) }))
                            as VertexStream<'vertex, Result<Vertex, GraphError>>
                    }
                    Some(sv) => match sv.package.view().parent_of(sv.intro) {
                        None => Box::pin(stream::empty()),
                        Some(parent_intro) => {
                            let pkg = sv.package.clone();
                            Box::pin(stream::once(
                                async move { vertex_for_intro(pkg, parent_intro) },
                            ))
                        }
                    },
                }
            }),

            // ── Symbol → usages (reverse occurrences across all packages) ──
            (_, "usages") => {
                let corpus_clone = corpus.clone();
                async_helpers::try_resolve_neighbors_with(contexts, move |vertex| {
                    match extract_symbol_vertex(vertex) {
                        None => {
                            let e = GraphError::UnknownEdge {
                                ty: "Symbol".to_string(),
                                edge: "usages".to_string(),
                            };
                            Box::pin(stream::once(async move { Err(e) }))
                                as VertexStream<'vertex, Result<Vertex, GraphError>>
                        }
                        Some(sv) => {
                            let target = sv.stable_ref();
                            let corpus2 = corpus_clone.clone();
                            Box::pin(
                                stream::once(async move { corpus2.packages().await }).flat_map(
                                    move |pkgs| {
                                        let target = target.clone();
                                        let pairs: Vec<(Arc<PackageView>, IntroId)> = pkgs
                                            .into_iter()
                                            .flat_map(|pkg| {
                                                let intros: Vec<IntroId> =
                                                    pkg.indexes().usages_of(&target).to_vec();
                                                intros.into_iter().map(move |i| (pkg.clone(), i))
                                            })
                                            .collect();
                                        stream::iter(
                                            pairs
                                                .into_iter()
                                                .map(|(pkg, intro)| vertex_for_intro(pkg, intro)),
                                        )
                                    },
                                ),
                            )
                        }
                    }
                })
            }

            // ── Symbol → mentions (type references across all packages) ────
            (_, "mentions") => {
                let corpus_clone = corpus.clone();
                async_helpers::try_resolve_neighbors_with(contexts, move |vertex| {
                    match extract_symbol_vertex(vertex) {
                        None => {
                            let e = GraphError::UnknownEdge {
                                ty: "Symbol".to_string(),
                                edge: "mentions".to_string(),
                            };
                            Box::pin(stream::once(async move { Err(e) }))
                                as VertexStream<'vertex, Result<Vertex, GraphError>>
                        }
                        Some(sv) => {
                            let target = sv.stable_ref();
                            let corpus2 = corpus_clone.clone();
                            Box::pin(
                                stream::once(async move { corpus2.packages().await }).flat_map(
                                    move |pkgs| {
                                        let target = target.clone();
                                        let pairs: Vec<(Arc<PackageView>, IntroId)> = pkgs
                                            .into_iter()
                                            .flat_map(|pkg| {
                                                let intros: Vec<IntroId> =
                                                    pkg.indexes().mentions_of(&target).to_vec();
                                                intros.into_iter().map(move |i| (pkg.clone(), i))
                                            })
                                            .collect();
                                        stream::iter(
                                            pairs
                                                .into_iter()
                                                .map(|(pkg, intro)| vertex_for_intro(pkg, intro)),
                                        )
                                    },
                                ),
                            )
                        }
                    }
                })
            }

            // ── Symbol → occurrencesOf ─────────────────────────────────────
            (_, "occurrencesOf") => {
                async_helpers::try_resolve_neighbors_with(contexts, move |vertex| {
                    match extract_symbol_vertex(vertex) {
                        None => {
                            let e = GraphError::UnknownEdge {
                                ty: "Symbol".to_string(),
                                edge: "occurrencesOf".to_string(),
                            };
                            Box::pin(stream::once(async move { Err(e) }))
                                as VertexStream<'vertex, Result<Vertex, GraphError>>
                        }
                        Some(sv) => {
                            let count = sv.package.view().occurrences_of(sv.intro).len();
                            let sv_clone = sv.clone();
                            Box::pin(stream::iter((0..count).map(move |idx| {
                                Ok(Vertex::Occurrence(OccurrenceVertex {
                                    owner: sv_clone.clone(),
                                    occ_index: idx,
                                }))
                            })))
                        }
                    }
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
            // Cost: O(1) `Corpus::entry` lookup per occurrence — identical to
            // the `Symbols` pushdown path on `key` equality.
            ("Occurrence", "target") => {
                let corpus_clone = corpus.clone();
                async_helpers::try_resolve_neighbors_with(contexts, move |vertex| {
                    let ov = match vertex.as_occurrence() {
                        Some(ov) => ov,
                        None => {
                            let e = GraphError::UnknownEdge {
                                ty: "Occurrence".to_string(),
                                edge: "target".to_string(),
                            };
                            return Box::pin(stream::once(async move { Err(e) }))
                                as VertexStream<'vertex, Result<Vertex, GraphError>>;
                        }
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
                    let corpus2 = corpus_clone.clone();
                    Box::pin(
                        stream::once(async move {
                            // Look up the package that owns the target.  If it is
                            // not loaded, yield nothing (optional edge).
                            let pkg = match corpus2.package(&target_sr.package).await {
                                Some(p) => p,
                                None => return Ok(None),
                            };
                            // Build the vertex.  A missing intro in a loaded package
                            // is a corpus inconsistency; surface it as an error so
                            // the caller can diagnose it rather than silently losing
                            // the row.
                            let v = vertex_for_intro(pkg, target_sr.intro)?;
                            Ok(Some(v))
                        })
                        // `stream::once` yields `Result<Option<Vertex>, GraphError>`.
                        // We must flatten the `Option` into a zero-or-one element
                        // stream without losing the `Result` wrapper.
                        .flat_map(|result| match result {
                            Err(e) => Box::pin(stream::once(async move { Err(e) }))
                                as VertexStream<'vertex, Result<Vertex, GraphError>>,
                            Ok(None) => Box::pin(stream::empty()),
                            Ok(Some(v)) => Box::pin(stream::once(async move { Ok(v) })),
                        }),
                    )
                })
            }

            // ── Trait → implementors ───────────────────────────────────────
            ("Trait", "implementors") => {
                let corpus_clone = corpus.clone();
                async_helpers::try_resolve_neighbors_with(contexts, move |vertex| {
                    match extract_symbol_vertex(vertex) {
                        None => {
                            let e = GraphError::UnknownEdge {
                                ty: "Trait".to_string(),
                                edge: "implementors".to_string(),
                            };
                            Box::pin(stream::once(async move { Err(e) }))
                                as VertexStream<'vertex, Result<Vertex, GraphError>>
                        }
                        Some(sv) => {
                            let target = sv.stable_ref();
                            let corpus2 = corpus_clone.clone();
                            Box::pin(
                                stream::once(async move { corpus2.packages().await }).flat_map(
                                    move |pkgs| {
                                        let target = target.clone();
                                        let pairs: Vec<(Arc<PackageView>, IntroId)> = pkgs
                                            .into_iter()
                                            .flat_map(|pkg| {
                                                let intros: Vec<IntroId> =
                                                    pkg.indexes().mentions_of(&target).to_vec();
                                                intros.into_iter().map(move |i| (pkg.clone(), i))
                                            })
                                            .collect();
                                        stream::iter(
                                            pairs
                                                .into_iter()
                                                .map(|(pkg, intro)| vertex_for_intro(pkg, intro)),
                                        )
                                    },
                                ),
                            )
                        }
                    }
                })
            }

            (ty, edge) => {
                let ty_owned = ty.to_string();
                let edge_owned = edge.to_string();
                async_helpers::try_resolve_neighbors_with(
                    contexts,
                    move |_vertex| -> VertexStream<'vertex, Result<Vertex, GraphError>> {
                        let e = GraphError::UnknownEdge {
                            ty: ty_owned.clone(),
                            edge: edge_owned.clone(),
                        };
                        Box::pin(stream::once(async move { Err(e) }))
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
    ) -> ContextOutcomeStream<'vertex, V, Result<bool, GraphError>> {
        let target = coerce_to_type.to_string();
        async_helpers::try_resolve_coercion_with(contexts, move |vertex| {
            Ok(vertex.typename() == target.as_str())
        })
    }
}

// ---------------------------------------------------------------------------
// Internal vertex extraction helpers
// ---------------------------------------------------------------------------

/// Extract a `SymbolVertex` from any symbol-type vertex variant.
fn extract_symbol_vertex(vertex: &Vertex) -> Option<&SymbolVertex> {
    match vertex {
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
        Vertex::Package(_) | Vertex::Occurrence(_) => None,
    }
}

/// Extract a `SymbolVertex` from a generic vertex, returning an error if the
/// vertex is not a symbol type.
fn symbol_vertex_from<'v>(vertex: &'v Vertex, ty: &str) -> Result<&'v SymbolVertex, GraphError> {
    extract_symbol_vertex(vertex).ok_or_else(|| GraphError::UnknownEdge {
        ty: ty.to_string(),
        edge: String::new(),
    })
}
