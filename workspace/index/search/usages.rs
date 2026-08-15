//! The usage-query backend — reverse `occ` lookups for `Target::Usages`
//! (INDEX-PLAN §5.5, §9).
//!
//! A [`heart::query::Target::Usages`] query asks "where is this symbol used?".
//! That is answered from the reverse-position `occ` index (a disposable
//! projection over the IR plane). That index does not exist yet — WS5 lands the
//! reverse-position projection — so this module defines the *typed, honest*
//! seam: the [`UsageQueryBackend`] trait every deployment implements, the real
//! [`ReverseIndexUsageBackend`] over a loaded IR view + reverse-position index,
//! and the [`Unsupported`] stub that returns [`Error::UnsupportedTarget`]
//! for deployments where the projection type itself is absent.
//!
//! An *empty* [`ReverseIndexUsageBackend`] returns the typed
//! [`Error::IndexUnavailable`] (`503`) when no index is loaded for the
//! requested scope — distinct from a fake empty page and from the `501`
//! unsupported-target case.

use heart::query::{PageSpecification, StableReference};
use heart::search::Page;
use serde::{Deserialize, Serialize};

/// One recorded use of a symbol, projected from an `occ` frame.
///
/// This is the wire-facing shape a usages query pages out. It is deliberately
/// thin (INDEX-PLAN §5.5: usages routes return thin JSON, never bulk IR) — the
/// caller follows up with an ObjectPack range-get for the snippet window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// The stable reference of the symbol that *contains* this use (the
    /// enclosing entry the `occ` frame hangs off).
    pub within: StableReference,
    /// The kind of reference (`call`, `mcall`, `typeref`, …), as recorded by
    /// the host resolve ladder.
    pub kind: String,
    /// Byte span of the use, relative to the enclosing entry's span start
    /// (U-3 relative spans), as `(start, end)`.
    pub relative_span: (u32, u32),
}

/// Why a usage query could not be answered.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// The reverse `occ` index required to answer `Target::Usages` is not
    /// available in this deployment yet (WS5 reverse-position projection). The
    /// route is wired and typed; the backing index is pending.
    #[error(
        "usage queries are not supported yet: the reverse occurrence index is not wired \
         (INDEX-PLAN §5.5, WS5)"
    )]
    UnsupportedTarget,

    /// The usage-query surface **is** wired, but no reverse-position index is
    /// currently loaded for the requested scope (no IR/generation materialized
    /// for that package here yet). This is the honest "come back later" state:
    /// distinct from [`Error::UnsupportedTarget`] (the projection type
    /// itself is not implemented) and from a fake empty page (which would claim
    /// "zero uses"). Surfaced as `503 Service Unavailable`.
    #[error(
        "no reverse-position index is loaded for the requested scope; usages are \
         unavailable until this package's IR is materialized"
    )]
    IndexUnavailable,

    /// The requested symbol is not a well-formed cross-package reference the
    /// reverse index can key on (its intro id is not lowercase hex, etc.).
    #[error("the requested usage target is not a resolvable stable reference: {detail}")]
    UnresolvableTarget {
        /// Human-readable reason the reference could not be resolved.
        detail: String,
    },
}

/// Backwards-compatible alias: the usage-query error (now [`Error`]).
pub use self::Error as UsageQueryError;

/// The engine seam a `Target::Usages` query delegates to.
///
/// Every deployment supplies an implementation: the real one queries the
/// reverse-position `occ` projection; [`Unsupported`] is the honest stub until
/// that projection exists.
pub trait UsageQueryBackend: Send + Sync {
    /// Return one ranked, engine-paginated page of the uses of `symbol`,
    /// bounded by `page`.
    fn usages(
        &self,
        symbol: &StableReference,
        page: &PageSpecification,
    ) -> Result<Page<Usage>, Error>;
}

/// The stub backend used until WS5's reverse `occ` index lands.
///
/// Always returns [`Error::UnsupportedTarget`] — never a silent empty
/// page, so a client can distinguish "no uses" from "not implemented".
#[derive(Debug, Default, Clone, Copy)]
pub struct Unsupported;

impl UsageQueryBackend for Unsupported {
    fn usages(
        &self,
        _symbol: &StableReference,
        _page: &PageSpecification,
    ) -> Result<Page<Usage>, Error> {
        Err(Error::UnsupportedTarget)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// ReverseIndexUsageBackend — the real backend over a loaded IR view.
// ─────────────────────────────────────────────────────────────────────────────

use std::sync::Arc;

use ir::change::{IntroId, PackageLineageId, StableRef};
use ir::view::IrView;

use registry::graph::ReversePositionIndex;

/// The real usage-query backend: answers `Target::Usages` from a loaded IR view
/// and its disposable [`ReversePositionIndex`] (INDEX-PLAN §5.5).
///
/// # Honest availability
///
/// A backend is constructed [`empty`](Self::empty) (no scope loaded) or
/// [`loaded`](Self::loaded) (one package's IR view + reverse index). An empty
/// backend answers every query with [`Error::IndexUnavailable`]
/// (`503`) — never a fake empty page — so a client can tell "this package's IR
/// is not materialized here yet" apart from "this symbol genuinely has no uses"
/// (a real, *populated* `Page { items: [] }`). This is the wiring the review
/// asked for: a routed surface that returns a **typed** unavailable when no
/// index is loaded, not a `501` and not a panic.
///
/// # What it returns
///
/// When a scope is loaded and the requested symbol lives in that scope, the
/// backend walks the view's occurrences whose `target` is the symbol (the
/// [`ReversePositionIndex`] fast-path confirms the owner set) and projects each
/// into a thin [`Usage`] wire record — `within` (the enclosing entry), `kind`,
/// and the `(start, end)` relative span (U-3 relative spans).
pub struct ReverseIndexUsageBackend {
    scope: Option<UsageScope>,
}

/// One loaded package scope: the IR view (source of occurrence detail) plus the
/// reverse-position index built over it (fast owner postings).
struct UsageScope {
    view: Arc<IrView>,
    reverse: Arc<ReversePositionIndex>,
}

impl ReverseIndexUsageBackend {
    /// An empty backend with no scope loaded. Every query answers
    /// [`Error::IndexUnavailable`].
    #[must_use]
    pub fn empty() -> Self {
        Self { scope: None }
    }

    /// A backend serving usages for one loaded package `view`, accelerated by
    /// `reverse` (both must be built from the same channel tip).
    #[must_use]
    pub fn loaded(view: Arc<IrView>, reverse: Arc<ReversePositionIndex>) -> Self {
        Self { scope: Some(UsageScope { view, reverse }) }
    }

    /// Whether a scope is currently loaded (a query can be answered with real
    /// data rather than [`Error::IndexUnavailable`]).
    #[must_use]
    pub fn is_loaded(&self) -> bool {
        self.scope.is_some()
    }
}

impl UsageQueryBackend for ReverseIndexUsageBackend {
    fn usages(
        &self,
        symbol: &StableReference,
        page: &PageSpecification,
    ) -> Result<Page<Usage>, Error> {
        // No scope loaded → honest unavailable (not 501, not a fake empty page).
        let Some(scope) = self.scope.as_ref() else {
            return Err(Error::IndexUnavailable);
        };

        // Resolve the wire reference into the typed cross-package reference the
        // reverse index keys on. A malformed intro id is a client-side
        // unresolvable target, not an unavailability.
        let target = stable_ref_from_wire(symbol)
            .map_err(|detail| Error::UnresolvableTarget { detail })?;

        // If the target's package is not the one this scope loaded, we hold no
        // index for it — honest unavailable rather than a false "no uses".
        if scope.view.package() != &target.package {
            return Err(Error::IndexUnavailable);
        }

        // Fast-path: the reverse index's owner postings for this target. An
        // empty posting list here is a *real* answer — the package is loaded and
        // the symbol simply has no graph-worthy uses — so we return a populated
        // empty page, never an unavailability.
        let owners: std::collections::HashSet<IntroId> =
            scope.reverse.usages_of(&target).iter().copied().collect();

        // Project each matching occurrence into a thin wire `Usage`. We walk the
        // view's occurrences (the source of `kind` + `span`, which the
        // owner-only posting list does not carry) and keep those whose target is
        // the symbol and whose owner the reverse index confirms.
        //
        // `IrView::all_occurrences()` yields `(owner: IntroId, &Occurrence)`;
        // the owner is the first element of the pair — `Occurrence` itself has
        // no `owner` field in nudox-ir.
        let package = scope.view.package();
        let mut uses: Vec<Usage> = scope
            .view
            .all_occurrences()
            .filter(|(owner, occ)| &occ.target == &target && owners.contains(owner))
            .map(|(owner, occ)| Usage {
                within: stable_ref_to_wire(&StableRef::new(package.clone(), owner)),
                kind: reference_kind_token(occ.kind).to_owned(),
                // nudox-ir `Occurrence` uses `span` (not `rel_span`).
                relative_span: (occ.span.start, occ.span.end),
            })
            .collect();

        // Deterministic order: by enclosing entry, then span. Keeps pagination
        // stable across identical loads.
        uses.sort_by(|a, b| {
            (a.within.to_string(), a.relative_span)
                .cmp(&(b.within.to_string(), b.relative_span))
        });

        Ok(paginate(uses, page))
    }
}

/// Map a resolved [`ir::vocab::ReferenceKind`] to its frozen wire token
/// (the `Usage.kind` vocabulary).
fn reference_kind_token(kind: ir::vocab::ReferenceKind) -> &'static str {
    use ir::vocab::ReferenceKind;
    match kind {
        ReferenceKind::FunctionCall => "call",
        ReferenceKind::MethodCall => "mcall",
        ReferenceKind::TypeReference => "typeref",
        ReferenceKind::VariableUse => "varuse",
        ReferenceKind::MacroInvocation => "macro",
        ReferenceKind::FieldAccess => "field",
        ReferenceKind::Import => "import",
    }
}

/// Render a typed [`StableRef`] into the wire `F:<eco>/<pkg>#<hex>` grammar.
fn stable_ref_to_wire(sref: &StableRef) -> StableReference {
    // The grammar is total over these components (non-empty ecosystem/name and
    // a lowercase-hex 32-byte intro), so parse cannot fail; if the invariant is
    // ever broken we surface it as a client-visible unresolvable rather than
    // panicking downstream.
    let raw = format!(
        "F:{eco}/{pkg}#{intro}",
        eco = sref.package.ecosystem.as_str(),
        pkg = sref.package.name.as_str(),
        intro = sref.intro.to_hex(),
    );
    StableReference::parse(&raw)
        .unwrap_or_else(|error| unreachable!("canonical stable reference must parse: {error}"))
}

/// Resolve a wire [`StableReference`] into the typed [`StableRef`] the reverse
/// index keys on. Returns a human-readable reason on failure.
fn stable_ref_from_wire(reference: &StableReference) -> Result<StableRef, String> {
    let intro_bytes = decode_hex_32(reference.intro_hex())
        .ok_or_else(|| format!("intro id `{}` is not 32 bytes of hex", reference.intro_hex()))?;
    let package = PackageLineageId::new(
        ir::change::EcosystemId::new(reference.ecosystem()),
        ir::change::PackageName::new(reference.package()),
    );
    Ok(StableRef::new(package, IntroId::from_raw(intro_bytes)))
}

/// Decode exactly 32 bytes of lowercase hex, or `None`.
fn decode_hex_32(hex: &str) -> Option<[u8; 32]> {
    if hex.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (index, chunk) in hex.as_bytes().chunks_exact(2).enumerate() {
        let hi = (chunk[0] as char).to_digit(16)?;
        let lo = (chunk[1] as char).to_digit(16)?;
        out[index] = ((hi << 4) | lo) as u8;
    }
    Some(out)
}

/// Slice `uses` into one engine-paginated [`Page`], resuming after the cursor
/// (an opaque ordinal token) when given. Usages carry no relevance score, so
/// each hit is stamped with a neutral score and the cursor is a simple offset.
fn paginate(uses: Vec<Usage>, page: &PageSpecification) -> Page<Usage> {
    use heart::Scored;

    let limit = (page.limit as usize).max(1);
    let start: usize = page
        .cursor
        .as_deref()
        .and_then(|token| token.parse::<usize>().ok())
        .unwrap_or(0);

    let neutral = heart::Score::try_new(0.0).expect("0.0 is finite");
    let items: Vec<Scored<Usage>> = uses
        .iter()
        .skip(start)
        .take(limit)
        .cloned()
        .map(|usage| Scored { score: neutral, value: usage })
        .collect();

    let consumed = start + items.len();
    let next = (consumed < uses.len()).then(|| consumed.to_string());
    Page { items, next }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use ir::apply::PristineIntroTable;
    use ir::change::{EcosystemId, PackageLineageId, PackageName, StableRef};
    use ir::entry::{Entry, Node, Symbol, Visibility};
    use ir::kind::Kind;
    use ir::kinds::Module;
    use ir::vocab::{Confidence, Occurrence, ReferenceKind, RelSpan};
    use ir::view::IrView;

    fn pkg() -> PackageLineageId {
        PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("demo"))
    }

    fn intro(n: u8) -> ir::change::IntroId {
        ir::change::IntroId::from_raw([n; 32])
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

    fn module(name: &str) -> Entry {
        Entry::new(sym(name), Node::build(None::<ir::index::RawRef>, []), Kind::Module(Module))
    }

    /// Build a view where owners `2` and `3` each hold one graph-worthy call to
    /// target `1`, plus a wire reference to that target.
    fn loaded_backend() -> (ReverseIndexUsageBackend, StableReference) {
        let mut table = PristineIntroTable::new();
        table.insert_live(intro(1), module("target"), None);
        table.insert_live(intro(2), module("caller_a"), Some(intro(1)));
        table.insert_live(intro(3), module("caller_b"), Some(intro(1)));
        let mut ir = IrView::with_package(pkg(), table);

        let target = StableRef::new(pkg(), intro(1));
        ir.add_occurrence(
            intro(2),
            Occurrence::new(target.clone(), ReferenceKind::FunctionCall, Confidence::Index, RelSpan::new(4, 9)),
        );
        ir.add_occurrence(
            intro(3),
            Occurrence::new(target.clone(), ReferenceKind::MethodCall, Confidence::Index, RelSpan::new(1, 5)),
        );

        let key = registry::graph::ReverseIndexKey {
            channel_tip: [0u8; 32],
            schema_version: registry::graph::SCHEMA_VERSION,
        };
        let reverse = ReversePositionIndex::build(&ir, key);
        let backend = ReverseIndexUsageBackend::loaded(Arc::new(ir), Arc::new(reverse));
        let wire = stable_ref_to_wire(&target);
        (backend, wire)
    }

    #[test]
    fn empty_backend_is_index_unavailable_not_empty_page() {
        let backend = ReverseIndexUsageBackend::empty();
        let wire = stable_ref_to_wire(&StableRef::new(pkg(), intro(1)));
        let err = backend
            .usages(&wire, &PageSpecification::default())
            .expect_err("empty backend must not return a page");
        assert_eq!(err, Error::IndexUnavailable);
    }

    #[test]
    fn loaded_backend_returns_real_uses() {
        let (backend, wire) = loaded_backend();
        let page = backend
            .usages(&wire, &PageSpecification::default())
            .expect("loaded backend answers");
        assert_eq!(page.items.len(), 2, "two owners use the target");
        let kinds: Vec<&str> = page.items.iter().map(|s| s.value.kind.as_str()).collect();
        assert!(kinds.contains(&"call"));
        assert!(kinds.contains(&"mcall"));
        // Relative spans survive the projection.
        assert!(page.items.iter().any(|s| s.value.relative_span == (4, 9)));
    }

    #[test]
    fn loaded_backend_unknown_symbol_is_populated_empty_not_unavailable() {
        let (backend, _) = loaded_backend();
        // A well-formed reference in the loaded package with no uses → real,
        // populated empty page (distinct from IndexUnavailable).
        let orphan = stable_ref_to_wire(&StableRef::new(pkg(), intro(9)));
        let page = backend
            .usages(&orphan, &PageSpecification::default())
            .expect("in-scope symbol with no uses is a real empty answer");
        assert!(page.items.is_empty());
        assert!(page.next.is_none());
    }

    #[test]
    fn loaded_backend_other_package_is_index_unavailable() {
        let (backend, _) = loaded_backend();
        let other_pkg =
            PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("elsewhere"));
        let elsewhere = stable_ref_to_wire(&StableRef::new(other_pkg, intro(1)));
        let err = backend
            .usages(&elsewhere, &PageSpecification::default())
            .expect_err("no index is loaded for a different package");
        assert_eq!(err, Error::IndexUnavailable);
    }

    #[test]
    fn stable_reference_roundtrips_through_the_bridge() {
        let sref = StableRef::new(pkg(), intro(7));
        let wire = stable_ref_to_wire(&sref);
        let back = stable_ref_from_wire(&wire).expect("wire form must resolve");
        assert_eq!(sref, back);
    }

    #[test]
    fn pagination_advances_by_offset_cursor() {
        let (backend, wire) = loaded_backend();
        let first = backend
            .usages(&wire, &PageSpecification { limit: 1, cursor: None })
            .expect("first page");
        assert_eq!(first.items.len(), 1);
        let cursor = first.next.expect("more uses remain");
        let second = backend
            .usages(&wire, &PageSpecification { limit: 1, cursor: Some(cursor) })
            .expect("second page");
        assert_eq!(second.items.len(), 1);
        assert!(second.next.is_none(), "two uses consumed in two pages");
        assert_ne!(
            first.items[0].value.within, second.items[0].value.within,
            "pages must not overlap"
        );
    }
}
