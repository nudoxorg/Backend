//! Reverse-position index — a disposable O(log n) look-up layer over the IR.
//!
//! A [`ReversePositionIndex`] is built from one [`IrView`] and is **never** synced or
//! persisted: it is always re-derived from the view at the same
//! `(channel_tip, schema_version)` key and can be discarded and rebuilt freely.
//!
//! ## What it indexes
//!
//! * **Reverse occurrence postings** (`target → [owner, …]`): built from
//!   [`IrView::all_occurrences`] filtered to `confidence >= Confidence::Index`
//!   (the graph-worthy floor, §vocab).
//!
//! * **Reverse type-ref postings** (`ty_ref → [entry, …]`): built by scanning
//!   [`Kind::Impl`] (the implemented trait, via `of`) and [`Kind::Trait`]
//!   (each entry in `supers`). Field-type mentions are left to a later wave (see
//!   the `NOTE` in [`typerefs_of_entry`]).
//!
//! Both posting lists are sorted and deduped for determinism.

use std::collections::BTreeMap;

use ir::change::{IntroId, PackageLineageId, StableRef};
use ir::entry::Entry;
use ir::index::{RawRef, Ref};
use ir::kind::Kind;
use ir::kinds::Type;
use ir::view::IrView;

// ---------------------------------------------------------------------------
// SCHEMA_VERSION
// ---------------------------------------------------------------------------

/// Frozen schema version of the reverse-position index format.
///
/// Bump this whenever the posting-list construction rules change so that callers
/// can detect stale cached indices.
pub const SCHEMA_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// ReverseIndexKey
// ---------------------------------------------------------------------------

/// Cache key for a [`ReversePositionIndex`].
///
/// Encodes the identity of the channel tip and the index schema so that a
/// cached index can be invalidated when either changes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ReverseIndexKey {
    /// The 32-byte BLAKE3 digest that names the channel tip this index was
    /// built from (i.e. the `GenerationStamp` or `CasKey` of the generation,
    /// treated as an opaque byte array here so this crate need not depend on
    /// the full hash-types hierarchy).
    pub channel_tip: [u8; 32],
    /// The schema version this index was built against; compare with
    /// [`SCHEMA_VERSION`].
    pub schema_version: u32,
}

// ---------------------------------------------------------------------------
// type_to_stable_ref  (private helper)
// ---------------------------------------------------------------------------

/// Extract a [`StableRef`] from a nudox-ir [`Type`] within a package context.
///
/// The extraction rules follow the design brief:
///
/// * [`Type::Nominal`] with a [`Ref::Intro`] → same-package `StableRef`.
/// * [`Type::Nominal`] with a [`Ref::Foreign`] → the `StableRef` directly.
/// * [`Type::Apply`] → recurse into `base` (a generic trait application such
///   as `impl Iterator<Item = u32>` appears here; the old flat wire form
///   silently dropped this case).
/// * [`Type::Nominal`] with a [`Ref::Local`] must not appear in a sealed
///   table; `seal` lowers every `Local` to `Intro`/`Foreign`. Treat as `None`
///   and do not emit a posting for it.
/// * All other [`Type`] variants carry no nominal reference and yield `None`.
fn type_to_stable_ref(ty: &Type, package: &PackageLineageId) -> Option<StableRef> {
    match ty {
        Type::Nominal(raw_ref) => raw_ref_to_stable(raw_ref, package),
        Type::Apply { base, .. } => {
            // Recurse into the base of a generic application.
            // E.g. `impl Iterator<Item = u32>` → base is `Nominal(Iterator)`.
            // `base` is `&Box<Type>` via match ergonomics; Deref coercion
            // converts it to `&Type` at the call site automatically.
            type_to_stable_ref(base, package)
        }
        // Primitives, tuples, slices, arrays, unions, intersections, Never, Any,
        // and SelfType carry no cross-package nominal reference at this wave.
        _ => None,
    }
}

/// Convert a [`RawRef`] (= [`Ref<UntypedMarker>`]) to a [`StableRef`].
///
/// `Ref::Intro(id)` → same-package reference using the given `package`.
/// `Ref::Foreign(sr)` → used directly.
/// `Ref::Local(_)` → `None` (must not appear post-seal; logged as a comment).
#[inline]
fn raw_ref_to_stable(raw: &RawRef, package: &PackageLineageId) -> Option<StableRef> {
    match raw {
        Ref::Intro(id) => Some(StableRef::new(package.clone(), *id)),
        Ref::Foreign(sr) => Some(sr.clone()),
        // Ref::Local must not appear in a sealed table — seal lowers every
        // Local to Intro/Foreign in one pass.  Treat as None: no posting.
        Ref::Local(_) => None,
    }
}

// ---------------------------------------------------------------------------
// typerefs_of_entry  (shared helper — also used by trustfall_adapter)
// ---------------------------------------------------------------------------

/// Extract the load-bearing type references from one entry.
///
/// Returns the [`StableRef`]s that are structurally load-bearing for graph
/// edge construction:
///
/// * For [`Kind::Impl`]: the implemented trait (`of`, if present).
/// * For [`Kind::Trait`]: each supertrait in `supers`.
///
/// Generic trait applications (e.g. `impl Iterator<Item = u32>`) appear as
/// [`Type::Apply`] with a [`Type::Nominal`] base; `type_to_stable_ref` recurses
/// into `base` so these are not silently dropped (the old flat-wire form missed
/// this case).
///
/// [`Ref::Local`] refs, which must not appear in a sealed table, yield no
/// posting (see `raw_ref_to_stable`).
///
/// **NOTE:** field-type mentions (`Kind::Field`, parameter types in
/// `Kind::Function`, etc.) are intentionally omitted here; they will be
/// added in a later wave once the field-type graph edges are specced.
pub fn typerefs_of_entry(entry: &Entry, package: &PackageLineageId) -> Vec<StableRef> {
    let mut refs: Vec<StableRef> = Vec::new();

    let kind = match entry.kind().as_owned_kind() {
        Some(k) => k,
        None => return refs, // Reference entry — no kind body to inspect.
    };

    match kind {
        Kind::Impl(impl_) => {
            // The implemented trait (e.g. `impl Display for T` → trait is load-bearing).
            if let Some(of_ty) = &impl_.of {
                if let Some(sr) = type_to_stable_ref(of_ty, package) {
                    refs.push(sr);
                }
            }
        }
        Kind::Trait(trait_) => {
            // Each supertrait (e.g. `trait Foo: Bar + Baz` → Bar and Baz).
            for super_ty in trait_.supers.iter() {
                if let Some(sr) = type_to_stable_ref(super_ty, package) {
                    refs.push(sr);
                }
            }
        }
        // All other kinds: no load-bearing type refs extracted in this wave.
        Kind::Module(_)
        | Kind::Record(_)
        | Kind::Field(_)
        | Kind::Function(_)
        | Kind::Alias(_)
        | Kind::Enum(_)
        | Kind::Variant(_)
        | Kind::Const(_)
        | Kind::Static(_)
        | Kind::Reexport(_)
        | Kind::Param(_) => {}
    }

    refs
}

// ---------------------------------------------------------------------------
// ReversePositionIndex
// ---------------------------------------------------------------------------

/// A disposable reverse-position index over one [`IrView`].
///
/// Build once with [`ReversePositionIndex::build`]; cache keyed on
/// `(channel_tip, schema_version)` via the embedded [`ReverseIndexKey`]. Discard
/// and rebuild whenever the channel tip advances or [`SCHEMA_VERSION`] bumps.
///
/// Never persisted, never synced.
#[derive(Debug)]
pub struct ReversePositionIndex {
    /// The cache key this index was built under.
    key: ReverseIndexKey,
    /// Reverse occurrence postings: `target StableRef → [owner IntroId, …]`.
    ///
    /// Populated only from occurrences with `confidence >= Confidence::Index`
    /// (the graph floor). Posting lists are sorted and deduped.
    occ_postings: BTreeMap<StableRef, Vec<IntroId>>,
    /// Reverse type-reference postings: `type StableRef → [entry IntroId, …]`.
    ///
    /// Built from `Impl.of` (trait refs) and `Trait.supers`. Posting lists are
    /// sorted and deduped.
    typeref_postings: BTreeMap<StableRef, Vec<IntroId>>,
}

impl ReversePositionIndex {
    // -----------------------------------------------------------------------
    // Constructor
    // -----------------------------------------------------------------------

    /// Build the reverse-position index from an `IrView`.
    ///
    /// This is a one-shot, single-pass operation.  It iterates
    /// [`IrView::all_occurrences`] once (O(n)) and all live entries once
    /// (O(m)); it then sorts and deduplicates every posting list for
    /// determinism (O(n log n)).
    pub fn build(ir: &IrView, key: ReverseIndexKey) -> Self {
        let mut occ_postings: BTreeMap<StableRef, Vec<IntroId>> = BTreeMap::new();
        let mut typeref_postings: BTreeMap<StableRef, Vec<IntroId>> = BTreeMap::new();

        // -- Occurrence reverse pass (graph-worthy only) --------------------
        // `all_occurrences()` yields `(owner: IntroId, &Occurrence)` pairs.
        // The owner is the first element; `Occurrence` itself has no `owner` field.
        for (owner, occ) in ir.all_occurrences() {
            if occ.confidence.is_graph_worthy() {
                occ_postings
                    .entry(occ.target.clone())
                    .or_default()
                    .push(owner);
            }
        }

        // -- Type-ref reverse pass -----------------------------------------
        let package = ir.package();
        for (intro, entry) in ir.entries() {
            let refs = typerefs_of_entry(entry, package);
            for sr in refs {
                typeref_postings.entry(sr).or_default().push(intro);
            }
        }

        // -- Dedup + sort for determinism ----------------------------------
        for list in occ_postings.values_mut() {
            list.sort_unstable();
            list.dedup();
        }
        for list in typeref_postings.values_mut() {
            list.sort_unstable();
            list.dedup();
        }

        Self {
            key,
            occ_postings,
            typeref_postings,
        }
    }

    // -----------------------------------------------------------------------
    // Accessors
    // -----------------------------------------------------------------------

    /// The cache key this index was built under.
    #[inline]
    pub fn key(&self) -> &ReverseIndexKey {
        &self.key
    }

    /// The sorted, deduplicated list of owners that hold a graph-worthy
    /// occurrence whose `target` is `target`.
    ///
    /// Returns an empty slice when no entry references `target` at the graph
    /// floor.
    #[inline]
    pub fn usages_of(&self, target: &StableRef) -> &[IntroId] {
        self.occ_postings
            .get(target)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// The sorted, deduplicated list of entries that carry a load-bearing type
    /// reference pointing at `ty` (impl-of or trait-super only in this wave).
    ///
    /// Returns an empty slice when no entry mentions `ty`.
    #[inline]
    pub fn mentions_of(&self, ty: &StableRef) -> &[IntroId] {
        self.typeref_postings
            .get(ty)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
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
    use ir::index::Ref;
    use ir::kind::Kind;
    use ir::kinds::{Function, Module, Trait, Type};
    use ir::view::IrView;
    use ir::vocab::{Confidence, Occurrence, ReferenceKind, RelSpan};

    // -----------------------------------------------------------------------
    // Local test helpers
    // -----------------------------------------------------------------------

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

    /// Build an owned [`Entry`] with a root `Node` (no pre-encoded parent or
    /// children) and the given kind.  The parent edge is recorded separately by
    /// [`PristineIntroTable::insert_live`]; keeping the `Node` empty avoids the
    /// `Local`-ref lowering that `seal` would normally do.
    fn entry(name: &str, kind: Kind) -> Entry {
        Entry::new(sym(name), Node::build(None::<ir::index::RawRef>, []), kind)
    }

    fn module(name: &str) -> Entry {
        entry(name, Kind::Module(Module))
    }

    fn function(name: &str) -> Entry {
        entry(name, Kind::Function(Function::builder().build()))
    }

    fn eco() -> EcosystemId {
        EcosystemId::new("cargo")
    }

    fn pkg_id() -> PackageLineageId {
        PackageLineageId::new(eco(), PackageName::new("test-pkg"))
    }

    fn intro(n: u8) -> IntroId {
        IntroId::from_raw([n; 32])
    }

    fn sref_local(n: u8) -> StableRef {
        StableRef::new(pkg_id(), intro(n))
    }

    fn default_key() -> ReverseIndexKey {
        ReverseIndexKey {
            channel_tip: [0u8; 32],
            schema_version: SCHEMA_VERSION,
        }
    }

    /// Build a minimal [`IrView`] for `pkg_id()` with the given entries and
    /// occurrences.  Entries are `(intro_byte, name, parent_byte_or_none)`;
    /// occurrences are added via `view.add_occurrence(owner, occ)`.
    fn make_view() -> IrView {
        let table = PristineIntroTable::new();
        IrView::with_package(pkg_id(), table)
    }

    // -----------------------------------------------------------------------
    // Tests
    // -----------------------------------------------------------------------

    /// `usages_of` returns exactly the owners for a target when confidence is
    /// at or above the graph floor.
    #[test]
    fn usages_of_returns_graph_worthy_owners() {
        let mut table = PristineIntroTable::new();
        table.insert_live(intro(1), module("root"), None);
        table.insert_live(intro(2), function("caller"), Some(intro(1)));
        table.insert_live(intro(3), function("callee"), Some(intro(1)));
        let mut view = IrView::with_package(pkg_id(), table);

        let target = sref_local(3);

        // Oracle occurrence (above floor) → should appear.
        view.add_occurrence(
            intro(2),
            Occurrence::new(
                target.clone(),
                ReferenceKind::FunctionCall,
                Confidence::Oracle,
                RelSpan::new(0, 5),
            ),
        );

        let idx = ReversePositionIndex::build(&view, default_key());
        assert_eq!(idx.usages_of(&target), &[intro(2)]);
    }

    /// A `Confidence::Syntactic` occurrence (below the graph floor) MUST NOT
    /// appear in the reverse-occurrence postings.
    #[test]
    fn syntactic_occurrence_is_excluded() {
        let mut table = PristineIntroTable::new();
        table.insert_live(intro(1), module("root"), None);
        table.insert_live(intro(2), function("caller"), Some(intro(1)));
        table.insert_live(intro(3), function("callee"), Some(intro(1)));
        let mut view = IrView::with_package(pkg_id(), table);

        let target = sref_local(3);

        // Syntactic only — below floor.
        view.add_occurrence(
            intro(2),
            Occurrence::new(
                target.clone(),
                ReferenceKind::FunctionCall,
                Confidence::Syntactic,
                RelSpan::new(0, 5),
            ),
        );

        let idx = ReversePositionIndex::build(&view, default_key());
        assert_eq!(
            idx.usages_of(&target),
            &[] as &[IntroId],
            "Syntactic confidence must not project into graph postings"
        );
    }

    /// `Confidence::Suffix` (also below floor) is likewise excluded.
    #[test]
    fn suffix_occurrence_is_excluded() {
        let mut table = PristineIntroTable::new();
        table.insert_live(intro(1), function("a"), None);
        let mut view = IrView::with_package(pkg_id(), table);
        let target = sref_local(2);
        view.add_occurrence(
            intro(1),
            Occurrence::new(
                target.clone(),
                ReferenceKind::TypeReference,
                Confidence::Suffix,
                RelSpan::new(0, 3),
            ),
        );
        let idx = ReversePositionIndex::build(&view, default_key());
        assert_eq!(idx.usages_of(&target), &[] as &[IntroId]);
    }

    /// Building the index twice from the same view yields identical posting maps.
    #[test]
    fn build_is_deterministic() {
        let mut table = PristineIntroTable::new();
        table.insert_live(intro(1), module("mod"), None);
        table.insert_live(intro(2), function("a"), Some(intro(1)));
        table.insert_live(intro(3), function("b"), Some(intro(1)));
        let mut view = IrView::with_package(pkg_id(), table);

        let target = sref_local(3);
        view.add_occurrence(
            intro(2),
            Occurrence::new(
                target.clone(),
                ReferenceKind::FunctionCall,
                Confidence::Index,
                RelSpan::new(0, 4),
            ),
        );

        let idx1 = ReversePositionIndex::build(&view, default_key());
        let idx2 = ReversePositionIndex::build(&view, default_key());

        // Compare occ postings via the public API.
        assert_eq!(
            idx1.usages_of(&target),
            idx2.usages_of(&target),
            "rebuild must yield identical occurrence postings"
        );
        // Compare typeref postings (empty here, but the call exercising both maps).
        let some_ty = sref_local(10);
        assert_eq!(
            idx1.mentions_of(&some_ty),
            idx2.mentions_of(&some_ty),
            "rebuild must yield identical typeref postings"
        );
    }

    /// `mentions_of` returns the entries that carry a trait supertrait pointing
    /// at the given StableRef.
    #[test]
    fn mentions_of_supertrait() {
        // `BaseTrait` is intro(2); `DerivedTrait` (intro 3) extends it via a
        // `Type::Nominal(Ref::Intro(intro(2)))` supertrait.
        let base_intro = intro(2);
        let base_ref = Type::Nominal(Ref::Intro(base_intro));
        let derived_entry = entry(
            "DerivedTrait",
            Kind::Trait(Trait::builder().supers([base_ref]).build()),
        );

        let mut table = PristineIntroTable::new();
        table.insert_live(intro(1), module("mod"), None);
        table.insert_live(intro(2), function("BaseTrait"), Some(intro(1)));
        table.insert_live(intro(3), derived_entry, Some(intro(1)));
        let view = IrView::with_package(pkg_id(), table);

        let base_sr = sref_local(2);
        let idx = ReversePositionIndex::build(&view, default_key());
        let mentions = idx.mentions_of(&base_sr);
        assert_eq!(
            mentions,
            &[intro(3)],
            "derived trait should appear in mentions of base"
        );
    }

    /// Multiple owners for the same target are all captured and deduplicated.
    #[test]
    fn multiple_owners_for_same_target() {
        let mut table = PristineIntroTable::new();
        table.insert_live(intro(1), module("mod"), None);
        table.insert_live(intro(2), function("fn_a"), Some(intro(1)));
        table.insert_live(intro(3), function("fn_b"), Some(intro(1)));
        table.insert_live(intro(4), function("target_fn"), Some(intro(1)));
        let mut view = IrView::with_package(pkg_id(), table);

        let target = sref_local(4);

        view.add_occurrence(
            intro(2),
            Occurrence::new(
                target.clone(),
                ReferenceKind::FunctionCall,
                Confidence::Import,
                RelSpan::new(0, 3),
            ),
        );
        view.add_occurrence(
            intro(3),
            Occurrence::new(
                target.clone(),
                ReferenceKind::FunctionCall,
                Confidence::Oracle,
                RelSpan::new(0, 3),
            ),
        );
        // Duplicate from owner 2 (should dedup).
        view.add_occurrence(
            intro(2),
            Occurrence::new(
                target.clone(),
                ReferenceKind::MethodCall,
                Confidence::Index,
                RelSpan::new(10, 15),
            ),
        );

        let idx = ReversePositionIndex::build(&view, default_key());
        let owners = idx.usages_of(&target);
        // Both intro(2) and intro(3), deduped and sorted.
        assert_eq!(owners, &[intro(2), intro(3)]);
    }
}
