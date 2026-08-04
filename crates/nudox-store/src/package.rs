//! [`PackageView`] and its derived [`PackageIndexes`].
//!
//! # Immutability guarantee
//!
//! A `PackageView` is sealed at construction time and is never mutated
//! afterwards. All mutable index building happens inside
//! [`PackageIndexes::build`] before the view is wrapped in an `Arc`. Once the
//! `Arc<PackageView>` is published into the [`Corpus`], every reader sees a
//! fully consistent snapshot — no partial indexes, no torn reads.
//!
//! # Why `Arc` and not `Rc`?
//!
//! The engine, the graph adapter, MCP tools, and GUI views all hold
//! `Arc<PackageView>` references concurrently from different threads and
//! Tokio tasks. `Arc` is the only choice that keeps the view `Send + Sync`.
//!
//! # GD-34: disposable projections
//!
//! `PackageIndexes` is never persisted. It is always re-derived from the
//! `IrView` at the same `(channel_tip, schema_version)` key. Discarding and
//! rebuilding costs O(n log n) in the number of symbols — fast enough for
//! hot-reload and cheap enough that we do not need a CAS-keyed cache at L1.

use std::collections::HashMap;
use std::sync::Arc;

use nudox_ir::{
    change::{IntroId, PackageLineageId, StableRef},
    kind::KindDiscriminant,
    reflect::moniker_path,
    view::IrView,
    vocab::Confidence,
};

use crate::index::{
    NameIndex,
    PostingList,
    posting::PostingListBuilder,
};

// Re-export so callers can name the prior-art helper without importing
// `workspace/registry/...` directly.
pub use self::typerefs::typerefs_of_entry;

mod typerefs {
    //! Port of `workspace/registry/graph/reverse_index::typerefs_of_entry`.
    //!
    //! Construction rules are correct as documented in the prior art; we
    //! port rather than re-invent (§L3.1).

    use nudox_ir::{
        change::{PackageLineageId, StableRef},
        entry::Entry,
        index::{RawRef, Ref},
        kind::Kind,
        kinds::Type,
    };

    /// Extract the load-bearing type references from one entry.
    ///
    /// Returns the [`StableRef`]s that are structurally load-bearing for graph
    /// edge construction:
    ///
    /// * For [`Kind::Impl`]: the implemented trait (`of`, if present).
    /// * For [`Kind::Trait`]: each supertrait in `supers`.
    ///
    /// Generic trait applications (e.g. `impl Iterator<Item = u32>`) appear as
    /// [`Type::Apply`] with a [`Type::Nominal`] base; `type_to_stable_ref`
    /// recurses into `base` so these are not silently dropped.
    ///
    /// **NOTE:** field-type mentions are intentionally omitted here; they will
    /// be added in a later wave (matching the prior-art comment).
    pub fn typerefs_of_entry(entry: &Entry, package: &PackageLineageId) -> Vec<StableRef> {
        let mut refs: Vec<StableRef> = Vec::new();

        let kind = match entry.kind().as_owned_kind() {
            Some(k) => k,
            None => return refs,
        };

        match kind {
            Kind::Impl(impl_) => {
                if let Some(of_ty) = &impl_.of {
                    if let Some(sr) = type_to_stable_ref(of_ty, package) {
                        refs.push(sr);
                    }
                }
            }
            Kind::Trait(trait_) => {
                for super_ty in trait_.supers.iter() {
                    if let Some(sr) = type_to_stable_ref(super_ty, package) {
                        refs.push(sr);
                    }
                }
            }
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

    fn type_to_stable_ref(ty: &Type, package: &PackageLineageId) -> Option<StableRef> {
        match ty {
            Type::Nominal(raw_ref) => raw_ref_to_stable(raw_ref, package),
            Type::Apply { base, .. } => type_to_stable_ref(base, package),
            _ => None,
        }
    }

    fn raw_ref_to_stable(raw: &RawRef, package: &PackageLineageId) -> Option<StableRef> {
        match raw {
            Ref::Intro(id) => Some(StableRef::new(package.clone(), *id)),
            Ref::Foreign(sr) => Some(sr.clone()),
            // Ref::Local must not appear in a sealed table.
            Ref::Local(_) => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Provenance
// ---------------------------------------------------------------------------

/// How confidently this package's IR was produced.
///
/// `#[non_exhaustive]` so that new provenance tiers (e.g. `SyncedLocal` with
/// a generation id) can be added without breaking `match` arms in callers.
///
/// See GUI-LOCAL-PLAN §L2.1 for the full provenance vocabulary; the variants
/// here are the subset that L1 needs to record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Provenance {
    /// Produced on this machine from source we can see.
    ///
    /// This is the default for every package produced by [`ProducerSource`] or
    /// [`FixtureSource`]. LR-10: local is the truth; this is not the
    /// exception.
    ///
    /// [`ProducerSource`]: crate::source::producer::ProducerSource
    /// [`FixtureSource`]: crate::source::fixtures::FixtureSource
    TrustedLocal,
    /// The package was not re-produced in this session; its IR was loaded from
    /// a prior snapshot. Still trusted, but the snapshot generation may be
    /// behind the current source.
    SnapshotLocal,
}

// ---------------------------------------------------------------------------
// PackageIndexes
// ---------------------------------------------------------------------------

/// All derived, rebuildable indexes for one package.
///
/// Built by [`PackageIndexes::build`] from a sealed [`IrView`].
/// Never persisted (GD-34).
pub struct PackageIndexes {
    /// Case-folded name → introductions, for prefix and fuzzy search (§L3.1).
    pub by_name: NameIndex,
    /// `KindDiscriminant` → introductions, for kind facets.
    ///
    /// Each entry appears in exactly one bucket (the bucket for its own
    /// discriminant). This lets the search layer cheaply filter to, e.g., all
    /// functions without scanning the full entry set.
    pub by_kind: HashMap<KindDiscriminant, Vec<IntroId>>,
    /// Reverse occurrence postings: `target StableRef → owners`.
    ///
    /// Only occurrences with `confidence >= Confidence::Index` are projected
    /// here (the graph-worthy floor, mirroring the prior art).
    pub usages: PostingList<StableRef, IntroId>,
    /// Reverse type-mention postings, from `Kind::Impl::of` and
    /// `Kind::Trait::supers`.
    ///
    /// Construction rules are ported from
    /// `workspace/registry/graph/reverse_index::typerefs_of_entry`.
    pub mentions: PostingList<StableRef, IntroId>,
    /// Fully-qualified path per intro, precomputed once.
    ///
    /// The path is the `moniker_path` of the entry: root-first, dot-joined
    /// symbol names. Precomputed so that search result rendering never
    /// re-walks the parent chain per query.
    pub paths: HashMap<IntroId, Arc<str>>,
}

impl PackageIndexes {
    /// Build all indexes from a sealed [`IrView`] in a single pass.
    ///
    /// Complexity: O(n log n) dominated by sort+dedup of posting lists and
    /// path-string allocation. Called once per package load.
    pub fn build(view: &IrView) -> Self {
        let package = view.package();

        let mut by_name = NameIndex::new();
        let mut by_kind: HashMap<KindDiscriminant, Vec<IntroId>> = HashMap::new();
        let mut usages_builder: PostingListBuilder<StableRef, IntroId> = PostingListBuilder::new();
        let mut mentions_builder: PostingListBuilder<StableRef, IntroId> =
            PostingListBuilder::new();
        let mut paths: HashMap<IntroId, Arc<str>> = HashMap::new();

        // Single pass over the declaration table.
        for (intro, entry) in view.entries() {
            // -- NameIndex ---------------------------------------------------
            by_name.insert(entry.sym().name.clone(), intro);

            // -- by_kind -----------------------------------------------------
            if let Some(disc) = entry.kind().discriminant() {
                by_kind.entry(disc).or_default().push(intro);
            }

            // -- paths -------------------------------------------------------
            if let Some(path) = moniker_path(view.table(), intro) {
                paths.insert(intro, Arc::from(path.as_str()));
            }

            // -- mentions (type refs) ----------------------------------------
            // Ported from workspace/registry/graph/reverse_index::typerefs_of_entry.
            for sr in typerefs_of_entry(entry, package) {
                mentions_builder.push(sr, intro);
            }
        }

        // -- usages (occurrence postings, graph-worthy only) -----------------
        // Mirrors the prior art: filter to confidence >= Index.
        for (owner, occ) in view.all_occurrences() {
            if occ.confidence >= Confidence::Index {
                usages_builder.push(occ.target.clone(), owner);
            }
        }

        // Sort by_kind vecs for determinism.
        for list in by_kind.values_mut() {
            list.sort_unstable();
            list.dedup();
        }

        Self {
            by_name,
            by_kind,
            usages: usages_builder.finish(),
            mentions: mentions_builder.finish(),
            paths,
        }
    }

    /// The sorted, deduplicated owners that hold a graph-worthy occurrence
    /// whose target is `target`.
    pub fn usages_of(&self, target: &StableRef) -> &[IntroId] {
        self.usages.get(target)
    }

    /// The entries that carry a load-bearing type reference pointing at `ty`.
    pub fn mentions_of(&self, ty: &StableRef) -> &[IntroId] {
        self.mentions.get(ty)
    }

    /// The precomputed fully-qualified path for `intro`, if present.
    pub fn path_of(&self, intro: IntroId) -> Option<&Arc<str>> {
        self.paths.get(&intro)
    }
}

// ---------------------------------------------------------------------------
// PackageView
// ---------------------------------------------------------------------------

/// One package's sealed IR plus all derived indexes. Immutable once built.
///
/// Always held as `Arc<PackageView>` — see the module-level docs for why.
///
/// # Accessing entries
///
/// Go through `view()` to reach the underlying [`IrView`]. The indexes
/// (`indexes()`) answer "which intros match this query?" and the view answers
/// "give me the entry for this intro".
pub struct PackageView {
    /// The sealed declaration table and occurrence map for this package.
    view: IrView,
    /// How this package's IR was obtained.
    provenance: Provenance,
    /// All derived indexes, built once from `view` at construction time.
    indexes: PackageIndexes,
}

/// Deliberately *not* derived.
///
/// A derived `Debug` would recurse through the whole `IrView` — every entry,
/// every type, every occurrence — so one `tracing::debug!` on a `LoadEvent`
/// would dump an entire package's IR into the log. The summary below is what a
/// human actually wants when a load event goes past: which package, how big,
/// and where it came from.
impl core::fmt::Debug for PackageView {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PackageView")
            .field("package", &format_args!("{}", self.view.package()))
            .field("entries", &self.view.table().len())
            .field("provenance", &self.provenance)
            .finish()
    }
}

impl PackageView {
    /// Build a `PackageView` by sealing the `IrView` and computing all indexes.
    ///
    /// This is the only constructor. After this call the view and its indexes
    /// are frozen; wrap the result in `Arc::new` before sharing.
    pub fn build(view: IrView, provenance: Provenance) -> Self {
        let indexes = PackageIndexes::build(&view);
        Self { view, provenance, indexes }
    }

    /// The lineage of this package (ecosystem + name).
    ///
    /// Forwarded from the inner [`IrView`] so callers do not need to reach
    /// through `view()`.
    pub fn lineage(&self) -> &PackageLineageId {
        self.view.package()
    }

    /// Borrow the sealed declaration table and occurrence map.
    pub fn view(&self) -> &IrView {
        &self.view
    }

    /// The provenance of this package's IR.
    pub fn provenance(&self) -> Provenance {
        self.provenance
    }

    /// The derived indexes built from this package's IR.
    pub fn indexes(&self) -> &PackageIndexes {
        &self.indexes
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use nudox_ir::{
        apply::PristineIntroTable,
        change::{EcosystemId, IntroId, PackageLineageId, PackageName, StableRef},
        entry::{Node, Symbol, Visibility},
        kind::Kind,
        kinds::{Function, Module, Trait, Type},
        index::Ref,
        view::IrView,
        vocab::{Confidence, Occurrence, ReferenceKind, RelSpan},
    };

    fn intro(n: u8) -> IntroId {
        IntroId::from_raw([n; 32])
    }

    fn lineage() -> PackageLineageId {
        PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("test-store"))
    }

    fn sym(name: &str) -> Symbol {
        Symbol {
            name: name.to_owned(),
            visibility: Visibility::Public,
            documentation: String::new(),
            source: std::path::PathBuf::new(),
            span: 0..0,
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        }
    }

    fn make_view() -> IrView {
        let mut table = PristineIntroTable::new();
        table.insert_live(intro(1), nudox_ir::entry::Entry::new(sym("root"), Node::build(None::<nudox_ir::index::RawRef>, []), Kind::Module(Module)), None);
        table.insert_live(intro(2), nudox_ir::entry::Entry::new(sym("my_fn"), Node::build(None::<nudox_ir::index::RawRef>, []), Kind::Function(Function::builder().build())), Some(intro(1)));
        IrView::with_package(lineage(), table)
    }

    #[test]
    fn build_populates_by_kind_and_by_name() {
        let view = make_view();
        let indexes = PackageIndexes::build(&view);

        // NameIndex: both "root" and "my_fn" must be present.
        assert!(!indexes.by_name.get_exact("root").is_empty());
        assert!(!indexes.by_name.get_exact("my_fn").is_empty());

        // by_kind: Module bucket has intro(1), Function bucket has intro(2).
        let modules = indexes.by_kind.get(&KindDiscriminant::Module).unwrap();
        assert!(modules.contains(&intro(1)));
        let fns = indexes.by_kind.get(&KindDiscriminant::Function).unwrap();
        assert!(fns.contains(&intro(2)));
    }

    #[test]
    fn usages_floor_is_confidence_index() {
        let mut table = PristineIntroTable::new();
        table.insert_live(intro(1), nudox_ir::entry::Entry::new(sym("root"), Node::build(None::<nudox_ir::index::RawRef>, []), Kind::Module(Module)), None);
        table.insert_live(intro(2), nudox_ir::entry::Entry::new(sym("caller"), Node::build(None::<nudox_ir::index::RawRef>, []), Kind::Function(Function::builder().build())), Some(intro(1)));
        let mut view = IrView::with_package(lineage(), table);

        let target = StableRef::new(lineage(), intro(99));

        // Below the floor — must NOT appear.
        view.add_occurrence(intro(2), Occurrence::new(
            target.clone(), ReferenceKind::FunctionCall, Confidence::Syntactic, RelSpan::new(0, 5),
        ));

        let indexes = PackageIndexes::build(&view);
        assert_eq!(indexes.usages_of(&target), &[] as &[IntroId]);
    }

    #[test]
    fn usages_at_index_confidence_appear() {
        let mut table = PristineIntroTable::new();
        table.insert_live(intro(1), nudox_ir::entry::Entry::new(sym("root"), Node::build(None::<nudox_ir::index::RawRef>, []), Kind::Module(Module)), None);
        table.insert_live(intro(2), nudox_ir::entry::Entry::new(sym("caller"), Node::build(None::<nudox_ir::index::RawRef>, []), Kind::Function(Function::builder().build())), Some(intro(1)));
        let mut view = IrView::with_package(lineage(), table);

        let target = StableRef::new(lineage(), intro(99));
        view.add_occurrence(intro(2), Occurrence::new(
            target.clone(), ReferenceKind::FunctionCall, Confidence::Index, RelSpan::new(0, 5),
        ));

        let indexes = PackageIndexes::build(&view);
        assert_eq!(indexes.usages_of(&target), &[intro(2)]);
    }

    #[test]
    fn mentions_from_trait_supers() {
        let base_intro = intro(10);
        let base_ref = Type::Nominal(Ref::Intro(base_intro));
        let mut table = PristineIntroTable::new();
        table.insert_live(intro(1), nudox_ir::entry::Entry::new(sym("root"), Node::build(None::<nudox_ir::index::RawRef>, []), Kind::Module(Module)), None);
        table.insert_live(intro(10), nudox_ir::entry::Entry::new(sym("BaseTrait"), Node::build(None::<nudox_ir::index::RawRef>, []), Kind::Trait(Trait::builder().build())), Some(intro(1)));
        table.insert_live(intro(11), nudox_ir::entry::Entry::new(sym("DerivedTrait"), Node::build(None::<nudox_ir::index::RawRef>, []), Kind::Trait(Trait::builder().supers([base_ref]).build())), Some(intro(1)));

        let view = IrView::with_package(lineage(), table);
        let indexes = PackageIndexes::build(&view);

        let base_sr = StableRef::new(lineage(), base_intro);
        let mentions = indexes.mentions_of(&base_sr);
        assert_eq!(mentions, &[intro(11)]);
    }

    #[test]
    fn paths_precomputed() {
        let view = make_view();
        let indexes = PackageIndexes::build(&view);

        // intro(2) = "my_fn" whose parent is intro(1) = "root".
        let path = indexes.path_of(intro(2)).expect("path must be present");
        assert_eq!(path.as_ref(), "root.my_fn");
    }
}
