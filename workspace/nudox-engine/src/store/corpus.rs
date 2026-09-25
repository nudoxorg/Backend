//! The [`Corpus`] — the whole local world of loaded packages.
//!
//! # Cheap cloning
//!
//! `Corpus` is a newtype over `Arc<CorpusInner>`. Cloning it is O(1) — just
//! an atomic reference-count increment. This lets the engine, the graph
//! adapter, search handlers, and MCP tools all hold their own `Corpus`
//! handles without any coordination beyond the one `Arc`.
//!
//! # Interior mutability
//!
//! The package map is wrapped in a `tokio::sync::RwLock<BTreeMap<...>>` rather
//! than a `DashMap` or `papaya::HashMap`. Rationale (see also `lib.rs`):
//!
//! * Packages are inserted during the initial workspace load and then the map
//!   is effectively read-only. Read contention is therefore zero in steady
//!   state.
//! * `RwLock::read()` is `async`, which composes cleanly with the engine's
//!   Tokio runtime. A lock-free structure would be cheaper per-operation but
//!   would complicate the ownership model of the returned `Arc<PackageView>`.
//! * `package()` and `entry()` clone the `Arc<PackageView>` before releasing
//!   the guard, so callers always hold a value (never a reference into a locked
//!   structure) — this is safe across `.await` points.
//!
//! # Why `BTreeMap` and not `HashMap`
//!
//! `packages()` hands the whole corpus to the search fan-out, which ranks the
//! rows it produces and shows them to a user. Under a `HashMap` that Vec came
//! out in std `RandomState` order, so the *same* corpus and the *same* query
//! produced a different ranking in every process — a per-process random seed
//! reaching the UI. Sorting inside `packages()` would fix that one caller;
//! keying the map by the `Ord` lineage id fixes it for every caller there will
//! ever be, at no cost worth measuring for a map this small. This is the
//! house rule from `nudox_ir::continuity`: iteration is over `BTreeMap`/
//! `BTreeSet`.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use nudox_ir::{
    change::{IntroId, PackageLineageId, StableRef},
    entry::Entry,
    kind::KindDiscriminant,
    view::IrView,
};
use tokio::sync::RwLock;

use crate::store::package::PackageView;

// ---------------------------------------------------------------------------
// CorpusInner (private)
// ---------------------------------------------------------------------------

/// The inner, `Arc`-wrapped state shared across all `Corpus` handles.
struct CorpusState {
    /// Loaded packages, ordered by lineage.
    packages: BTreeMap<PackageLineageId, Arc<PackageView>>,
    /// Lowercased symbol name → the packages that declare it.
    ///
    /// Name search ranges this map instead of opening every package.
    names: BTreeMap<String, BTreeSet<PackageLineageId>>,
    /// Type `StableRef` → packages whose indexes reference it.
    ///
    /// Signature search intersects these sets instead of opening every package.
    refs: BTreeMap<StableRef, BTreeSet<PackageLineageId>>,
    /// Kind → packages that declare at least one symbol of that kind.
    ///
    /// A kind query opens these packages. A package with no function is not
    /// part of a function search.
    kinds: BTreeMap<KindDiscriminant, BTreeSet<PackageLineageId>>,
}

struct CorpusInner {
    state: RwLock<CorpusState>,
}

// ---------------------------------------------------------------------------
// Corpus
// ---------------------------------------------------------------------------

/// The whole local world of loaded packages.
///
/// Cheap to clone — `Arc` inside. Safe to share across threads and Tokio
/// tasks. All mutation goes through the `insert` method; all reads return
/// owned `Arc<PackageView>` clones so callers never hold the internal lock
/// across an `.await` point.
#[derive(Clone)]
pub struct Corpus {
    inner: Arc<CorpusInner>,
}

impl Corpus {
    /// Construct an empty corpus.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(CorpusInner {
                state: RwLock::new(CorpusState {
                    packages: BTreeMap::new(),
                    names: BTreeMap::new(),
                    refs: BTreeMap::new(),
                    kinds: BTreeMap::new(),
                }),
            }),
        }
    }

    /// Insert or replace a package view.
    ///
    /// Called by the engine loader when a `LoadEvent::Ready` arrives.
    pub async fn insert(&self, package: Arc<PackageView>) {
        let mut state = self.inner.state.write().await;
        let id = package.lineage().clone();
        if let Some(previous) = state.packages.get(&id).cloned() {
            detach_names(&mut state.names, &previous);
            detach_refs(&mut state.refs, &previous);
            detach_kinds(&mut state.kinds, &previous);
        }
        attach_names(&mut state.names, &package);
        attach_refs(&mut state.refs, &package);
        attach_kinds(&mut state.kinds, &package);
        state.packages.insert(id, package);
    }

    /// Packages that declare a symbol whose lowercased name starts with
    /// `prefix_lower`. The prefix must already be lowercased. Lineage order.
    pub async fn packages_with_name_prefix(&self, prefix_lower: &str) -> Vec<Arc<PackageView>> {
        let state = self.inner.state.read().await;
        let mut ids = BTreeSet::new();
        for (key, owners) in state.names.range::<str, _>((
            std::ops::Bound::Included(prefix_lower),
            std::ops::Bound::Unbounded,
        )) {
            if !key.starts_with(prefix_lower) {
                break;
            }
            ids.extend(owners.iter().cloned());
        }
        ids.into_iter()
            .filter_map(|id| state.packages.get(&id).cloned())
            .collect()
    }

    /// Packages that declare each exact lowercased symbol name.
    ///
    /// Signature search resolves `return:Result` against these owners, then
    /// still walks every package for *uses* of that declaration. A name with
    /// no owner is present and maps to an empty vec.
    pub async fn packages_declaring(
        &self,
        names: &[String],
    ) -> BTreeMap<String, Vec<Arc<PackageView>>> {
        let state = self.inner.state.read().await;
        let mut out = BTreeMap::new();
        for name in names {
            let owners = state
                .names
                .get(name)
                .map(|ids| {
                    ids.iter()
                        .filter_map(|id| state.packages.get(id).cloned())
                        .collect()
                })
                .unwrap_or_default();
            out.insert(name.clone(), owners);
        }
        out
    }

    /// Packages that can satisfy a signature conjunction.
    ///
    /// Each inner vec is one facet's resolved declarations (a union: any
    /// `Point` counts). A package must reference at least one declaration of
    /// every facet. An empty facet matches nothing. Lineage order.
    pub async fn packages_referencing_facets(
        &self,
        facets: &[Vec<StableRef>],
    ) -> Vec<Arc<PackageView>> {
        if facets.is_empty() {
            return Vec::new();
        }
        let state = self.inner.state.read().await;
        let mut ids: Option<BTreeSet<PackageLineageId>> = None;
        for facet in facets {
            if facet.is_empty() {
                return Vec::new();
            }
            let mut owners = BTreeSet::new();
            for target in facet {
                if let Some(set) = state.refs.get(target) {
                    owners.extend(set.iter().cloned());
                }
            }
            if owners.is_empty() {
                return Vec::new();
            }
            ids = Some(match ids {
                None => owners,
                Some(previous) => previous.intersection(&owners).cloned().collect(),
            });
        }
        ids.unwrap_or_default()
            .into_iter()
            .filter_map(|id| state.packages.get(&id).cloned())
            .collect()
    }

    /// Packages that declare any of `kinds`. Lineage order. An empty list
    /// matches nothing: a query that names no kind is not a kind search.
    pub async fn packages_with_kinds(&self, kinds: &[KindDiscriminant]) -> Vec<Arc<PackageView>> {
        if kinds.is_empty() {
            return Vec::new();
        }
        let state = self.inner.state.read().await;
        let mut ids = BTreeSet::new();
        for kind in kinds {
            if let Some(owners) = state.kinds.get(kind) {
                ids.extend(owners.iter().cloned());
            }
        }
        ids.into_iter()
            .filter_map(|id| state.packages.get(&id).cloned())
            .collect()
    }

    /// Look up a package by its lineage identity.
    ///
    /// Returns `None` if the package has not yet been loaded (or if it
    /// failed during loading). The returned `Arc` can be held across
    /// `.await` points safely.
    pub async fn package(&self, id: &PackageLineageId) -> Option<Arc<PackageView>> {
        let state = self.inner.state.read().await;
        state.packages.get(id).cloned()
    }

    /// Look up a single entry by its [`StableRef`] (package lineage + intro).
    ///
    /// Returns `None` if the package is not loaded or the intro is not present
    /// in that package's declaration table.
    pub async fn entry(&self, key: &StableRef) -> Option<EntryRef> {
        let state = self.inner.state.read().await;
        let pkg = state.packages.get(&key.package)?.clone();
        // Verify the intro exists before producing the EntryRef.
        let _ = pkg.view().entry(key.intro)?;
        Some(EntryRef {
            package: pkg,
            intro: key.intro,
        })
    }

    /// Iterate over all currently loaded packages, ordered by lineage
    /// (ecosystem, then package name).
    ///
    /// Collects into a `Vec` to avoid holding the read lock across the
    /// caller's iteration. The allocation is bounded by the number of loaded
    /// packages, which is small (< 10 000 in any realistic workspace).
    ///
    /// The order is part of the contract, not an artefact: search fans out over
    /// this Vec and its ranking inherits the order for rows that tie, so a
    /// caller must be able to rely on two processes producing the same list.
    pub async fn packages(&self) -> Vec<Arc<PackageView>> {
        let state = self.inner.state.read().await;
        state.packages.values().cloned().collect()
    }

    /// Lineage ids in corpus order, without cloning each [`PackageView`].
    pub async fn lineages(&self) -> Vec<PackageLineageId> {
        let state = self.inner.state.read().await;
        state.packages.keys().cloned().collect()
    }

    /// The number of packages currently in the corpus.
    pub async fn len(&self) -> usize {
        self.inner.state.read().await.packages.len()
    }

    /// True if no packages have been loaded.
    pub async fn is_empty(&self) -> bool {
        self.inner.state.read().await.packages.is_empty()
    }
}

fn attach_refs(refs: &mut BTreeMap<StableRef, BTreeSet<PackageLineageId>>, package: &PackageView) {
    let id = package.lineage().clone();
    for (target, _) in package.indexes().type_refs.iter() {
        refs.entry(target.clone()).or_default().insert(id.clone());
    }
}

fn detach_refs(refs: &mut BTreeMap<StableRef, BTreeSet<PackageLineageId>>, package: &PackageView) {
    let id = package.lineage();
    for (target, _) in package.indexes().type_refs.iter() {
        let Some(owners) = refs.get_mut(target) else {
            continue;
        };
        owners.remove(id);
        if owners.is_empty() {
            refs.remove(target);
        }
    }
}

fn attach_kinds(
    kinds: &mut BTreeMap<KindDiscriminant, BTreeSet<PackageLineageId>>,
    package: &PackageView,
) {
    let id = package.lineage().clone();
    for kind in package.indexes().by_kind.keys() {
        kinds.entry(*kind).or_default().insert(id.clone());
    }
}

fn detach_kinds(
    kinds: &mut BTreeMap<KindDiscriminant, BTreeSet<PackageLineageId>>,
    package: &PackageView,
) {
    let id = package.lineage();
    for kind in package.indexes().by_kind.keys() {
        let Some(owners) = kinds.get_mut(kind) else {
            continue;
        };
        owners.remove(id);
        if owners.is_empty() {
            kinds.remove(kind);
        }
    }
}

fn attach_names(names: &mut BTreeMap<String, BTreeSet<PackageLineageId>>, package: &PackageView) {
    let id = package.lineage().clone();
    for key in package.indexes().by_name.keys() {
        names.entry(key.to_owned()).or_default().insert(id.clone());
    }
}

fn detach_names(names: &mut BTreeMap<String, BTreeSet<PackageLineageId>>, package: &PackageView) {
    let id = package.lineage();
    for key in package.indexes().by_name.keys() {
        let Some(owners) = names.get_mut(key) else {
            continue;
        };
        owners.remove(id);
        if owners.is_empty() {
            names.remove(key);
        }
    }
}

impl Default for Corpus {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// EntryRef
// ---------------------------------------------------------------------------

/// A resolved entry reference: a package view plus the intro id of the entry.
///
/// Holds an `Arc<PackageView>` so the entry can be accessed without a borrow
/// on the `Corpus` lock. The lifetime-free design matches the `SymbolVertex`
/// shape in `nudox-graph` (§L4.1).
pub struct EntryRef {
    /// The package that owns this entry.
    pub package: Arc<PackageView>,
    /// The stable introduction id of the entry within the package.
    pub intro: IntroId,
}

impl EntryRef {
    /// Borrow the [`Entry`] from the underlying `IrView`.
    ///
    /// Returns `None` only if the intro is absent from the package's
    /// declaration table (should not happen for a well-formed `EntryRef`
    /// produced by [`Corpus::entry`]).
    pub fn get(&self) -> Option<&Entry> {
        self.package.view().entry(self.intro)
    }

    /// The [`IrView`] of the owning package.
    pub fn view(&self) -> &IrView {
        self.package.view()
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
        kinds::Module,
        view::IrView,
    };

    use crate::store::package::{PackageView, Provenance};

    fn intro(n: u8) -> IntroId {
        IntroId::from_raw([n; 32])
    }

    fn lineage(name: &str) -> PackageLineageId {
        PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new(name))
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

    fn make_package(name: &str) -> Arc<PackageView> {
        make_package_with_symbol(name, "root")
    }

    fn make_package_with_symbol(name: &str, symbol: &str) -> Arc<PackageView> {
        let lid = lineage(name);
        let mut table = PristineIntroTable::new();
        table.insert_live(
            intro(1),
            nudox_ir::entry::Entry::new(
                sym(symbol),
                Node::build(None::<nudox_ir::index::RawRef>, []),
                Kind::Module(Module),
            ),
            None,
        );
        let view = IrView::with_package(lid, table);
        Arc::new(PackageView::build(view, Provenance::TrustedLocal))
    }

    #[tokio::test]
    async fn insert_and_lookup_package() {
        let corpus = Corpus::new();
        let pkg = make_package("my-crate");
        corpus.insert(pkg.clone()).await;

        let found = corpus.package(&lineage("my-crate")).await;
        assert!(found.is_some());
        assert_eq!(found.unwrap().lineage(), &lineage("my-crate"));
    }

    #[tokio::test]
    async fn missing_package_returns_none() {
        let corpus = Corpus::new();
        let found = corpus.package(&lineage("nonexistent")).await;
        assert!(found.is_none());
    }

    #[tokio::test]
    async fn entry_lookup_by_stable_ref() {
        let corpus = Corpus::new();
        let pkg = make_package("pkg");
        corpus.insert(pkg).await;

        let key = StableRef::new(lineage("pkg"), intro(1));
        let entry_ref = corpus.entry(&key).await;
        assert!(entry_ref.is_some());
        let er = entry_ref.unwrap();
        assert_eq!(er.get().unwrap().sym().name, "root");
    }

    #[tokio::test]
    async fn entry_missing_intro_returns_none() {
        let corpus = Corpus::new();
        let pkg = make_package("pkg");
        corpus.insert(pkg).await;

        // intro(99) does not exist in the package.
        let key = StableRef::new(lineage("pkg"), intro(99));
        assert!(corpus.entry(&key).await.is_none());
    }

    #[tokio::test]
    async fn packages_iterates_all() {
        let corpus = Corpus::new();
        corpus.insert(make_package("a")).await;
        corpus.insert(make_package("b")).await;
        corpus.insert(make_package("c")).await;

        let pkgs = corpus.packages().await;
        assert_eq!(pkgs.len(), 3);
    }

    #[tokio::test]
    async fn name_prefix_skips_packages_that_do_not_declare_the_symbol() {
        let corpus = Corpus::new();
        corpus.insert(make_package("alpha")).await;
        corpus
            .insert(make_package_with_symbol("beta", "widget"))
            .await;

        let matched = corpus.packages_with_name_prefix("wid").await;
        let names: Vec<_> = matched
            .iter()
            .map(|pkg| pkg.lineage().name.as_str().to_owned())
            .collect();
        assert_eq!(names, vec!["beta".to_owned()]);

        corpus
            .insert(make_package_with_symbol("beta", "other"))
            .await;
        assert!(corpus.packages_with_name_prefix("wid").await.is_empty());
        assert_eq!(corpus.packages_with_name_prefix("oth").await.len(), 1);
    }

    #[tokio::test]
    async fn kind_index_skips_packages_that_do_not_declare_the_kind() {
        use nudox_ir::{kind::Kind, kinds::Function};

        let corpus = Corpus::new();
        corpus.insert(make_package("module-only")).await;
        corpus.insert(make_function_package("fns")).await;

        let matched = corpus
            .packages_with_kinds(&[KindDiscriminant::Function])
            .await;
        let names: Vec<_> = matched
            .iter()
            .map(|pkg| pkg.lineage().name.as_str().to_owned())
            .collect();
        assert_eq!(names, vec!["fns".to_owned()]);
        assert!(corpus.packages_with_kinds(&[]).await.is_empty());

        corpus.insert(make_package("fns")).await;
        assert!(
            corpus
                .packages_with_kinds(&[KindDiscriminant::Function])
                .await
                .is_empty()
        );
        let modules = corpus
            .packages_with_kinds(&[KindDiscriminant::Module])
            .await;
        assert_eq!(modules.len(), 2);

        fn make_function_package(name: &str) -> Arc<PackageView> {
            let lid = lineage(name);
            let mut table = PristineIntroTable::new();
            table.insert_live(
                intro(1),
                nudox_ir::entry::Entry::new(
                    sym("root"),
                    Node::build(None::<nudox_ir::index::RawRef>, []),
                    Kind::Module(Module),
                ),
                None,
            );
            table.insert_live(
                intro(2),
                nudox_ir::entry::Entry::new(
                    sym("find"),
                    Node::build(None::<nudox_ir::index::RawRef>, []),
                    Kind::Function(Function::builder().build()),
                ),
                Some(intro(1)),
            );
            let view = IrView::with_package(lid, table);
            Arc::new(PackageView::build(view, Provenance::TrustedLocal))
        }
    }

    #[tokio::test]
    async fn type_ref_index_skips_packages_that_do_not_use_the_type() {
        use nudox_ir::{
            index::Ref,
            kind::Kind,
            kinds::{Field, FieldKey, Record, Type},
        };

        let corpus = Corpus::new();
        let point = lineage("point");
        let point_intro = intro(3);
        let mut table = PristineIntroTable::new();
        table.insert_live(
            point_intro,
            nudox_ir::entry::Entry::new(
                sym("Point"),
                Node::build(None::<nudox_ir::index::RawRef>, []),
                Kind::Record(Record::builder().build()),
            ),
            None,
        );
        table.insert_live(
            intro(4),
            nudox_ir::entry::Entry::new(
                sym("origin"),
                Node::build(None::<nudox_ir::index::RawRef>, []),
                Kind::Field(
                    Field::builder()
                        .key(FieldKey::Named)
                        .ty(Type::Nominal(Ref::Intro(point_intro)))
                        .build(),
                ),
            ),
            Some(point_intro),
        );
        let view = IrView::with_package(point.clone(), table);
        corpus
            .insert(Arc::new(PackageView::build(view, Provenance::TrustedLocal)))
            .await;
        corpus.insert(make_package("other")).await;

        let target = nudox_ir::change::StableRef::new(point, point_intro);
        let matched = corpus.packages_referencing_facets(&[vec![target.clone()]]).await;
        let names: Vec<_> = matched
            .iter()
            .map(|pkg| pkg.lineage().name.as_str().to_owned())
            .collect();
        assert_eq!(names, vec!["point".to_owned()]);

        corpus.insert(make_package("point")).await;
        assert!(
            corpus
                .packages_referencing_facets(&[vec![target]])
                .await
                .is_empty()
        );
    }

    /// `packages()` is ordered by lineage, and that order does not depend on
    /// the order packages were loaded in.
    ///
    /// Search fans out over this Vec and its ranking inherits the order for
    /// rows that tie, so an unordered result made the same query rank
    /// differently in every process.
    #[tokio::test]
    async fn packages_are_ordered_by_lineage_whatever_the_load_order() {
        // Enough packages that agreeing with the sorted order by chance is
        // implausible, and names whose sorted order is not the insertion order.
        let names = ["delta", "alpha", "echo", "charlie", "bravo", "foxtrot"];

        let forward = Corpus::new();
        for name in names {
            forward.insert(make_package(name)).await;
        }

        let reversed = Corpus::new();
        for name in names.iter().rev() {
            reversed.insert(make_package(name)).await;
        }

        let listed = |c: &Corpus| {
            let c = c.clone();
            async move {
                c.packages()
                    .await
                    .iter()
                    .map(|p| p.lineage().name.as_str().to_owned())
                    .collect::<Vec<_>>()
            }
        };

        let expected: Vec<String> = {
            let mut v: Vec<String> = names.iter().map(|s| (*s).to_owned()).collect();
            v.sort();
            v
        };

        assert_eq!(listed(&forward).await, expected);
        assert_eq!(
            listed(&reversed).await,
            expected,
            "the corpus listing changed when packages were loaded in a \
             different order"
        );
    }

    #[tokio::test]
    async fn clone_shares_state() {
        let corpus = Corpus::new();
        let corpus2 = corpus.clone();
        corpus.insert(make_package("shared")).await;

        // The clone must see packages inserted through the original.
        assert!(corpus2.package(&lineage("shared")).await.is_some());
    }
}
