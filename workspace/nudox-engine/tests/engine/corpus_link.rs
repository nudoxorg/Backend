//! The corpus-side cross-package LINK pass.
//!
//! Packages are produced+sealed in isolation against `ir::foreign::Unlinked`,
//! so every cross-package reference survives as a *named-but-unlinked*
//! `Ref::Foreign { key, target: None }`. Nothing turns that `None` into a
//! `StableRef` in production yet — the `ForeignResolver` seam that `seal`
//! uses is only ever fed `Unlinked` today. This is the structural gap:
//! inter-package refs are named everywhere, resolved nowhere.
//!
//! This pins the primitive that closes it: `CorpusResolver` — a real
//! `ForeignResolver` backed by the loaded `Corpus` — that matches a
//! `ForeignKey`'s `origin` + canonical `path` against the sibling package's
//! own moniker paths and returns the target's `StableRef`. It is the honest
//! use of the designed seam (feed a populated resolver instead of `Unlinked`),
//! not a query-time patch.
//!
//! Hermetic: hand-built sealed tables, no producer/toolchain. Mirrors the
//! construction in `multi_package_flows.rs` and the assertion shape in
//! `ir/model/tests/foreign_refs.rs`.

use std::sync::Arc;

use nudox_ir::{
    apply::PristineIntroTable,
    change::{EcosystemId, IntroId, PackageLineageId, PackageName, StableRef},
    entry::{Entry, EntryInner, Node, Symbol, Visibility},
    foreign::{ForeignKey, ForeignResolver, Resolution},
    index::{RawRef, Ref},
    kind::{Kind, KindDiscriminant},
    kinds::{Alias, Function, Module, Record, Trait, ty::Type},
    reflect::{PathStyle, moniker_path_styled},
    view::IrView,
};
use nudox_engine::store::package::{PackageView, Provenance};
use nudox_engine::store::prelude::Corpus;
use triomphe::Arc as TriompheArc;

fn lineage(ecosystem: &str, name: &str) -> PackageLineageId {
    PackageLineageId::new(EcosystemId::new(ecosystem), PackageName::new(name))
}

fn intro(n: u8) -> IntroId {
    IntroId::from_raw([n; 32])
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

fn entry_of(name: &str, kind: Kind) -> Entry {
    Entry::new(sym(name), Node::build(None::<RawRef>, []), kind)
}

fn module_entry(name: &str) -> Entry {
    entry_of(name, Kind::Module(Module))
}

/// Package `cargo:corelike`, built to pin two things at once:
///
/// * a two-level export (`corelike::Clone`, C1/C5) — the base case the
///   resolver must join a `ForeignKey.path` against, and
/// * a **three-level** chain (`corelike::sync::Mutex`, an intermediate
///   *module* between the root and the exported type) so that
///   [`moniker_path_styled`]'s root-segment behaviour is pinned from both
///   sides (C6): the fixture states the exact segments it built, and a
///   direct call to `moniker_path_styled` is asserted against that literal
///   string before the resolver is ever involved, so a resolver bug and a
///   `moniker_path_styled` surprise cannot be mistaken for each other.
///
/// Layout:
/// ```text
/// corelike            (intro 1, root, Module)
/// ├── Clone            (intro 2, Module — stand-in "exported type")
/// └── sync             (intro 3, Module)
///     └── Mutex         (intro 4, Module — stand-in "exported type")
/// ```
fn corelike_package() -> (Arc<PackageView>, StableRef, StableRef) {
    let lid = lineage("cargo", "corelike");
    let mut table = PristineIntroTable::new();
    // root symbol (segment 0) — its name is the first path segment.
    table.insert_live(intro(1), module_entry("corelike"), None);
    // the exported type, child of the root → path "corelike::Clone".
    table.insert_live(intro(2), module_entry("Clone"), Some(intro(1)));
    // an intermediate module, child of the root → path "corelike::sync".
    table.insert_live(intro(3), module_entry("sync"), Some(intro(1)));
    // the exported type, child of the module → path "corelike::sync::Mutex".
    table.insert_live(intro(4), module_entry("Mutex"), Some(intro(3)));

    // C6: pin what `moniker_path_styled` actually emits for both the direct
    // child and the grandchild-through-a-module, *before* any resolver code
    // runs, so the resolver's own test failures can never be confused with a
    // surprise in this lower-level primitive.
    assert_eq!(
        moniker_path_styled(&table, intro(1), PathStyle::DoubleColon),
        Some("corelike".to_string()),
        "the root segment alone renders as just its own name, with no \
         leading/trailing separator"
    );
    assert_eq!(
        moniker_path_styled(&table, intro(2), PathStyle::DoubleColon),
        Some("corelike::Clone".to_string()),
        "a direct child of the root is root-segment + separator + own name"
    );
    assert_eq!(
        moniker_path_styled(&table, intro(4), PathStyle::DoubleColon),
        Some("corelike::sync::Mutex".to_string()),
        "an intermediate module contributes its own segment in the middle — \
         it is not elided the way a package/crate-root sometimes is in other \
         path schemes"
    );

    let view = IrView::with_package(lid.clone(), table);
    let clone_ref = StableRef::new(lid.clone(), intro(2));
    let mutex_ref = StableRef::new(lid, intro(4));
    (
        Arc::new(PackageView::build(view, Provenance::TrustedLocal)),
        clone_ref,
        mutex_ref,
    )
}

/// Package `cargo:ambig`, whose root exports two entries sharing the exact
/// same styled path `ambig::Foo` — a `trait Foo` and a `fn Foo` — mirroring
/// Rust's separate type/value namespaces. Nothing about the *path* can tell
/// them apart; only `ForeignKey.kind` can.
fn ambiguous_package() -> (Arc<PackageView>, StableRef, StableRef) {
    let lid = lineage("cargo", "ambig");
    let mut table = PristineIntroTable::new();
    table.insert_live(intro(1), module_entry("ambig"), None);
    table.insert_live(
        intro(2),
        entry_of("Foo", Kind::Trait(Trait::builder().build())),
        Some(intro(1)),
    );
    table.insert_live(
        intro(3),
        entry_of("Foo", Kind::Function(Function::builder().build())),
        Some(intro(1)),
    );

    assert_eq!(
        moniker_path_styled(&table, intro(2), PathStyle::DoubleColon),
        Some("ambig::Foo".to_string())
    );
    assert_eq!(
        moniker_path_styled(&table, intro(3), PathStyle::DoubleColon),
        Some("ambig::Foo".to_string()),
        "trait Foo and fn Foo collide on styled path — Rust's separate type/\
         value namespaces mean this is a legal program, not a malformed fixture"
    );

    let view = IrView::with_package(lid.clone(), table);
    let trait_ref = StableRef::new(lid.clone(), intro(2));
    let fn_ref = StableRef::new(lid, intro(3));
    (
        Arc::new(PackageView::build(view, Provenance::TrustedLocal)),
        trait_ref,
        fn_ref,
    )
}

#[tokio::test]
async fn corpus_resolver_links_a_named_foreign_ref_to_its_stable_ref() {
    let (corelike, clone_ref, mutex_ref) = corelike_package();
    let corpus = Corpus::new();
    corpus.insert(corelike).await;

    // The resolver is a pure-sync snapshot of the async corpus (ForeignResolver
    // is sync; Corpus is async — the snapshot bridges them).
    let resolver = corpus.foreign_resolver().await;

    // (1) POSITIVE: a Package-origin key whose path names a real export links.
    let key = ForeignKey::in_package(lineage("cargo", "corelike"), "corelike::Clone", "Clone");
    assert_eq!(
        resolver.resolve(&key),
        Resolution::Resolved(clone_ref.clone()),
        "a named cross-package Ref::Foreign whose ForeignKey.path matches a loaded sibling's \
         export must link to that export's StableRef"
    );

    // (1b) POSITIVE, through an intermediate module: proves the resolver's
    // join key is the *same* multi-segment styled path pinned directly above
    // (C6), not merely a leaf-name or two-segment special case.
    let mutex_key =
        ForeignKey::in_package(lineage("cargo", "corelike"), "corelike::sync::Mutex", "Mutex");
    assert_eq!(
        resolver.resolve(&mutex_key),
        Resolution::Resolved(mutex_ref.clone()),
        "a path through an intermediate module must join exactly the way \
         moniker_path_styled renders it, module segment included"
    );

    // (2) PATH NOT FOUND: package is loaded, path names nothing in it.
    let missing_path =
        ForeignKey::in_package(lineage("cargo", "corelike"), "corelike::Nope", "Nope");
    assert_eq!(
        resolver.resolve(&missing_path),
        Resolution::PathNotFound {
            package: lineage("cargo", "corelike")
        },
        "a Package-origin key into a loaded package whose path names nothing must be \
         PathNotFound, never a wrong link"
    );

    // (3) PACKAGE NOT LOADED: origin names a package the corpus does not hold.
    let absent =
        ForeignKey::in_package(lineage("cargo", "absent"), "absent::Thing", "Thing");
    assert_eq!(
        resolver.resolve(&absent),
        Resolution::PackageNotLoaded,
        "a key whose owning package is not in the corpus must report PackageNotLoaded"
    );

    // (4) UNPLACEABLE: a Namespace origin names no exact package, so it cannot
    // link — the CorpusResolver must defer to the same rule as `Unlinked`.
    let namespaced = ForeignKey::in_namespace(
        EcosystemId::new("npm"),
        "external-dep",
        "external-dep.Foo",
        "Foo",
    );
    assert_eq!(
        resolver.resolve(&namespaced),
        Resolution::Unplaceable,
        "only a Package origin can ever link; a Namespace-origin key is Unplaceable"
    );
}

#[tokio::test]
async fn corpus_resolver_reports_ambiguous_when_kind_does_not_narrow() {
    let (ambig, _trait_ref, _fn_ref) = ambiguous_package();
    let corpus = Corpus::new();
    corpus.insert(ambig).await;
    let resolver = corpus.foreign_resolver().await;

    // No `kind` stated at all: two candidates share the path, nothing narrows.
    let key = ForeignKey::in_package(lineage("cargo", "ambig"), "ambig::Foo", "Foo");
    assert_eq!(
        resolver.resolve(&key),
        Resolution::Ambiguous { candidates: 2 },
        "trait Foo and fn Foo share one styled path; a resolver that picked \
         the first one found would sometimes link to the wrong symbol, which \
         is worse than not linking at all"
    );

    // A `kind` that matches *neither* candidate narrows to zero, not one —
    // must stay Ambiguous, never silently resolve.
    let wrong_kind = ForeignKey::in_package(lineage("cargo", "ambig"), "ambig::Foo", "Foo")
        .with_kind(KindDiscriminant::Record);
    assert_eq!(
        resolver.resolve(&wrong_kind),
        Resolution::Ambiguous { candidates: 2 },
        "a stated kind that matches none of the candidates must not be treated \
         as having narrowed anything"
    );
}

#[tokio::test]
async fn corpus_resolver_uses_kind_to_break_a_path_tie() {
    let (ambig, trait_ref, fn_ref) = ambiguous_package();
    let corpus = Corpus::new();
    corpus.insert(ambig).await;
    let resolver = corpus.foreign_resolver().await;

    let trait_key = ForeignKey::in_package(lineage("cargo", "ambig"), "ambig::Foo", "Foo")
        .with_kind(KindDiscriminant::Trait);
    assert_eq!(
        resolver.resolve(&trait_key),
        Resolution::Resolved(trait_ref),
        "stating kind = Trait must pick the trait out of the two same-path \
         candidates"
    );

    let fn_key = ForeignKey::in_package(lineage("cargo", "ambig"), "ambig::Foo", "Foo")
        .with_kind(KindDiscriminant::Function);
    assert_eq!(
        resolver.resolve(&fn_key),
        Resolution::Resolved(fn_ref),
        "stating kind = Function must pick the function out of the two \
         same-path candidates"
    );
}

/// END TO END: a real cross-package reference, sealed named-but-unlinked as a
/// producer emits it, becomes linked to the sibling's declaration once both are
/// in the corpus — via `relink` + `CorpusResolver`. This is the whole
/// inter-package chain in one test: producer emits `Ref::Foreign{None}` →
/// corpus loads the sibling → `foreign_resolver()` snapshots it →
/// `PristineIntroTable::relink` fills `target` with the sibling's `StableRef`.
#[tokio::test]
async fn relink_resolves_a_cross_package_ref_to_the_sibling_declaration_via_the_corpus() {
    // Package A ("cargo:corelike") declares the target type at `corelike::Clone`.
    let (corelike, clone_ref, _mutex) = corelike_package();
    let corpus = Corpus::new();
    corpus.insert(corelike).await;
    let resolver = corpus.foreign_resolver().await;

    // Package B ("cargo:app") references A's type cross-package. This is exactly
    // the shape a producer seals against `Unlinked`: a *named* `Ref::Foreign`
    // whose `key.path` spells A's export, with `target: None`.
    let mut b_table = PristineIntroTable::new();
    b_table.insert_live(intro(10), module_entry("app"), None);
    let foreign: RawRef = Ref::<Record>::Foreign {
        key: TriompheArc::new(ForeignKey::in_package(
            lineage("cargo", "corelike"),
            "corelike::Clone",
            "Clone",
        )),
        target: None,
    }
    .into_raw();
    let uses_clone = entry_of(
        "UsesClone",
        Kind::Alias(Alias::builder().target(Type::Nominal(foreign)).build()),
    );
    b_table.insert_live(intro(11), uses_clone, Some(intro(10)));

    // The post-load LINK pass, driven by the corpus-backed resolver.
    assert_eq!(
        b_table.relink(&resolver),
        1,
        "B's single cross-package reference must link to the loaded sibling"
    );

    // B's ref now carries A's real declaration identity — resolved, end to end.
    let (_, entry) = b_table
        .iter()
        .find(|(_, e)| e.sym().name == "UsesClone")
        .expect("the referencing alias must still be present");
    let EntryInner::Owned(Kind::Alias(alias)) = entry.kind() else {
        panic!("UsesClone must be an Alias");
    };
    let Some(Type::Nominal(Ref::Foreign { target, .. })) = alias.target.as_ref() else {
        panic!("UsesClone's target must be a cross-package Nominal(Foreign)");
    };
    assert_eq!(
        target.as_ref(),
        Some(&clone_ref),
        "relink through the CorpusResolver must resolve B's foreign ref to A's \
         actual `corelike::Clone` StableRef — nothing fabricated, nothing dropped"
    );
}

/// `Corpus::relink_all` links references *in place* across the whole loaded
/// corpus — the load-path integration. After it runs, a package read back FROM
/// THE CORPUS carries the linked target, which is what the query/render seams
/// (all corpus-independent, reading a single `PackageView`) need.
#[tokio::test]
async fn corpus_relink_all_links_every_loaded_package_in_place() {
    let (corelike, clone_ref, _mutex) = corelike_package();

    // Package B, a real `PackageView` in the corpus, referencing A cross-package.
    let b_lineage = lineage("cargo", "app");
    let mut b_table = PristineIntroTable::new();
    b_table.insert_live(intro(10), module_entry("app"), None);
    let foreign: RawRef = Ref::<Record>::Foreign {
        key: TriompheArc::new(ForeignKey::in_package(
            lineage("cargo", "corelike"),
            "corelike::Clone",
            "Clone",
        )),
        target: None,
    }
    .into_raw();
    b_table.insert_live(
        intro(11),
        entry_of(
            "UsesClone",
            Kind::Alias(Alias::builder().target(Type::Nominal(foreign)).build()),
        ),
        Some(intro(10)),
    );
    let b_view = IrView::with_package(b_lineage.clone(), b_table);
    let b_pkg = std::sync::Arc::new(PackageView::build(b_view, Provenance::TrustedLocal));

    let corpus = Corpus::new();
    corpus.insert(corelike).await;
    corpus.insert(b_pkg).await;

    assert_eq!(
        corpus.relink_all().await,
        1,
        "the one cross-package reference in the loaded corpus must link"
    );

    // Read B back FROM THE CORPUS — the swapped-in view must carry the target.
    let b = corpus
        .package(&b_lineage)
        .await
        .expect("B must still be in the corpus");
    let (_, entry) = b
        .view()
        .table()
        .iter()
        .find(|(_, e)| e.sym().name == "UsesClone")
        .expect("the referencing alias must still be present");
    let EntryInner::Owned(Kind::Alias(alias)) = entry.kind() else {
        panic!("UsesClone must be an Alias");
    };
    let Some(Type::Nominal(Ref::Foreign { target, .. })) = alias.target.as_ref() else {
        panic!("UsesClone's target must be a cross-package Nominal(Foreign)");
    };
    assert_eq!(
        target.as_ref(),
        Some(&clone_ref),
        "after relink_all, the corpus's own copy of B must have its foreign target filled"
    );
}
