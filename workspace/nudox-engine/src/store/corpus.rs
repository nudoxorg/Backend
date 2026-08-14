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
//!   the guard, so callers always hold a value (never a reference into a
//!   locked structure) — this is safe across `.await` points.
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

use std::collections::BTreeMap;
use std::sync::Arc;

use nudox_ir::{
    change::{IntroId, PackageLineageId, StableRef},
    entry::Entry,
    view::IrView,
};
use tokio::sync::RwLock;

use crate::store::package::PackageView;

// ---------------------------------------------------------------------------
// CorpusInner (private)
// ---------------------------------------------------------------------------

/// The inner, `Arc`-wrapped state shared across all `Corpus` handles.
struct CorpusInner {
    /// The complete map of loaded packages, keyed by their lineage.
    ///
    /// Each `PackageView` is itself `Arc`-wrapped so that returning a
    /// reference to one package does not require holding the outer lock.
    ///
    /// Ordered by `PackageLineageId` (ecosystem, then name) so that every
    /// iteration of the corpus is reproducible — see the module docs.
    packages: RwLock<BTreeMap<PackageLineageId, Arc<PackageView>>>,
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
                packages: RwLock::new(BTreeMap::new()),
            }),
        }
    }

    /// Insert or replace a package view.
    ///
    /// Called by the engine loader when a `LoadEvent::Ready` arrives.
    pub async fn insert(&self, package: Arc<PackageView>) {
        let mut map = self.inner.packages.write().await;
        map.insert(package.lineage().clone(), package);
    }

    /// Look up a package by its lineage identity.
    ///
    /// Returns `None` if the package has not yet been loaded (or if it
    /// failed during loading). The returned `Arc` can be held across
    /// `.await` points safely.
    pub async fn package(&self, id: &PackageLineageId) -> Option<Arc<PackageView>> {
        let map = self.inner.packages.read().await;
        map.get(id).cloned()
    }

    /// Look up a single entry by its [`StableRef`] (package lineage + intro).
    ///
    /// Returns `None` if the package is not loaded or the intro is not present
    /// in that package's declaration table.
    pub async fn entry(&self, key: &StableRef) -> Option<EntryRef> {
        let map = self.inner.packages.read().await;
        let pkg = map.get(&key.package)?.clone();
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
        let map = self.inner.packages.read().await;
        map.values().cloned().collect()
    }

    /// The number of packages currently in the corpus.
    pub async fn len(&self) -> usize {
        self.inner.packages.read().await.len()
    }

    /// True if no packages have been loaded.
    pub async fn is_empty(&self) -> bool {
        self.inner.packages.read().await.is_empty()
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
    pub fn get<'a>(&'a self) -> Option<&'a Entry> {
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
