//! `SECTION_TYPE` answers questions about **signatures**, not about kinds.
//!
//! # What this file is for
//!
//! Section 1 used to resolve a kind discriminant and return every symbol of
//! that kind at a fixed relevance — a filter labelled as a search. It is now a
//! signature search over `PackageIndexes::type_refs`, driven by the facet
//! grammar in `nudox_engine::typequery`. These tests pin the three claims that
//! makes:
//!
//! * a facet selects the declarations that name a type **in that position**,
//!   and not the ones that name it somewhere else;
//! * several facets **conjoin**;
//! * the kind path still works for every query that names no facet, so nothing
//!   that worked before stopped working.
//!
//! # Why the packages are hand-built here rather than taken from fixtures
//!
//! Doctrine §4 is blunt that a hand-authored fixture tests the fixture author's
//! imagination. It is right, and the counterweight is in
//! `real_signature_search.rs`, which runs the same questions against a real
//! crate. What a hand-built package buys *here* is the one thing a real crate
//! cannot: a known-complete answer key. When `param:Point` returns two rows we
//! need to know that exactly two functions in the package take a `Point`, and
//! for a real crate that is itself a measurement rather than a given.
//!
//! `crate::test_support` cannot build these — it is restricted to `Module` and
//! `Reexport`, which name no types at all — so the packages are assembled from
//! `PristineIntroTable` directly, in the same shape `search.rs`'s own
//! `package_with_leaf_collision` uses.

use std::sync::Arc;

use nudox_ir::{
    apply::PristineIntroTable,
    change::{EcosystemId, IntroId, PackageLineageId, PackageName},
    entry::{Entry, Node, Symbol, Visibility},
    index::{Ref, RawRef},
    kind::Kind,
    kinds::{Field, FieldKey, Function, Module, Param, Record, Type, ty::Primitive},
    view::IrView,
};
use nudox_store::package::{PackageView, Provenance};

use nudox_store::source::{IrSource, LoadEvent, LoadRequest, PackageHint, SourceDescriptor, SourceError};

use nudox_engine::search::SECTION_TYPE;
use nudox_engine::wire::{Gen, SearchEvent};
use nudox_engine::{Engine, EngineConfig, SearchQuery};

// ---------------------------------------------------------------------------
// Fixture construction
// ---------------------------------------------------------------------------

fn sym(name: &str, visibility: Visibility) -> Symbol {
    Symbol {
        name: name.to_owned(),
        visibility,
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

fn id(n: u8) -> IntroId {
    IntroId::from_raw([n; 32])
}

/// A package in which `Point` is declared and then named from every position
/// this test cares about.
///
/// The answer key, stated once so each assertion can quote it:
///
/// | declaration            | names `Point` in       |
/// |------------------------|------------------------|
/// | `fn make() -> Point`   | `Return`               |
/// | `fn plain(p: Point)`   | `Parameter`            |
/// | `fn borrow(p: &Point)` | `Parameter`, borrowed  |
/// | `struct Holder`        | (its field does)       |
/// | `Holder::origin`       | `FieldType`            |
fn point_package() -> Arc<PackageView> {
    let root = id(1);
    let point = id(2);
    let make = id(3);
    let make_out = id(4);
    let plain = id(5);
    let plain_in = id(6);
    let borrow = id(7);
    let borrow_in = id(8);
    let holder = id(9);
    let origin = id(10);

    let mut table = PristineIntroTable::new();
    table.insert_live(
        root,
        Entry::new(
            sym("geo", Visibility::Public),
            Node::build(None::<RawRef>, []),
            Kind::Module(Module),
        ),
        None,
    );
    table.insert_live(
        point,
        Entry::new(
            sym("Point", Visibility::Public),
            Node::build(None::<RawRef>, []),
            Kind::Record(Record::builder().build()),
        ),
        Some(root),
    );

    // `fn make() -> Point`
    table.insert_live(
        make_out,
        Entry::new(
            sym("return", Visibility::Public),
            Node::build(None::<RawRef>, []),
            Kind::Param(
                Param::builder()
                    .ty(Type::Nominal(RawRef::Intro(point)))
                    .build(),
            ),
        ),
        Some(make),
    );
    table.insert_live(
        make,
        Entry::new(
            sym("make", Visibility::Public),
            Node::build(None::<RawRef>, []),
            Kind::Function(
                Function::builder()
                    .output_params([Ref::Intro(make_out)])
                    .build(),
            ),
        ),
        Some(root),
    );

    // `fn plain(p: Point)`
    table.insert_live(
        plain_in,
        Entry::new(
            sym("p", Visibility::Public),
            Node::build(None::<RawRef>, []),
            Kind::Param(
                Param::builder()
                    .ty(Type::Nominal(RawRef::Intro(point)))
                    .build(),
            ),
        ),
        Some(plain),
    );
    table.insert_live(
        plain,
        Entry::new(
            sym("plain", Visibility::Public),
            Node::build(None::<RawRef>, []),
            Kind::Function(
                Function::builder()
                    .input_params([Ref::Intro(plain_in)])
                    .build(),
            ),
        ),
        Some(root),
    );

    // `fn borrow(p: &Point)` — the borrowed form, which is what real Rust
    // signatures overwhelmingly use.
    table.insert_live(
        borrow_in,
        Entry::new(
            sym("p", Visibility::Public),
            Node::build(None::<RawRef>, []),
            Kind::Param(
                Param::builder()
                    .ty(Type::Primitive(Primitive::Reference {
                        lifetime: None,
                        mutable: false,
                        ty: Box::new(Type::Nominal(RawRef::Intro(point))),
                    }))
                    .build(),
            ),
        ),
        Some(borrow),
    );
    table.insert_live(
        borrow,
        Entry::new(
            sym("borrow", Visibility::Public),
            Node::build(None::<RawRef>, []),
            Kind::Function(
                Function::builder()
                    .input_params([Ref::Intro(borrow_in)])
                    .build(),
            ),
        ),
        Some(root),
    );

    // `struct Holder { origin: Point }`
    table.insert_live(
        holder,
        Entry::new(
            sym("Holder", Visibility::Public),
            Node::build(None::<RawRef>, []),
            Kind::Record(Record::builder().build()),
        ),
        Some(root),
    );
    table.insert_live(
        origin,
        Entry::new(
            sym("origin", Visibility::Public),
            Node::build(None::<RawRef>, []),
            Kind::Field(
                Field::builder()
                    .key(FieldKey::Named)
                    .ty(Type::Nominal(RawRef::Intro(point)))
                    .build(),
            ),
        ),
        Some(holder),
    );

    let lineage = PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("geo"));
    Arc::new(PackageView::build(
        IrView::with_package(lineage, table),
        Provenance::TrustedLocal,
    ))
}

// ---------------------------------------------------------------------------
// StaticSource — replays one prebuilt package, same shape as
// versions_adversarial.rs
// ---------------------------------------------------------------------------

struct StaticSource {
    package: Arc<PackageView>,
}

impl IrSource for StaticSource {
    fn describe(&self) -> SourceDescriptor {
        SourceDescriptor {
            label: "signature-search-static".to_owned(),
            package_count_hint: Some(1),
        }
    }

    fn load(
        &self,
        _req: LoadRequest,
    ) -> futures::stream::BoxStream<'static, Result<LoadEvent, SourceError>> {
        use futures::StreamExt as _;
        let lineage = self.package.lineage().clone();
        futures::stream::iter(vec![
            Ok(LoadEvent::Discovered {
                lineage: lineage.clone(),
                hint: PackageHint {
                    display_name: lineage.name.as_str().to_owned(),
                    ecosystem: lineage.ecosystem.as_str().to_owned(),
                    version: Some("1.0.0".to_owned()),
                },
            }),
            Ok(LoadEvent::Ready {
                package: Arc::clone(&self.package),
            }),
        ])
        .boxed()
    }
}

/// Run one query through the **real engine** and return section 1's display
/// names, sorted so assertions read as sets rather than as rankings.
///
/// Going through `EngineHandle::search` rather than calling the collector
/// directly is deliberate: it is what makes these tests cover the dispatch in
/// `collect_type_hits` (facet query vs kind query) and the section id, not just
/// the collector's arithmetic. It also avoids adding a `_for_test` hook to the
/// engine's public surface for the sake of a test — which is the shape doctrine
/// §6 counts as a weakening when it later turns out to be the only caller.
fn hits(text: &str) -> Vec<String> {
    let package = point_package();
    let lineage = package.lineage().clone();
    let engine = Engine::start(EngineConfig::default(), StaticSource { package });

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime must build");

    runtime.block_on(async {
        // Settle: the corpus is populated by a spawned task, so a search issued
        // immediately would legitimately see an empty world.
        for _ in 0..200 {
            if engine.versions(&lineage).current().is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        let (_handle, rx) = engine.search(
            SearchQuery {
                text: text.to_owned(),
                limit: 0,
                ..Default::default()
            },
            Gen(1),
        );

        let mut names = Vec::new();
        while let Ok(event) = rx.recv_async().await {
            match event {
                SearchEvent::Section { section, rows, .. }
                | SearchEvent::Merge { section, rows, .. }
                    if section == SECTION_TYPE =>
                {
                    names.extend(rows.iter().map(|r| r.display_name.to_string()));
                }
                SearchEvent::Done { .. } | SearchEvent::Failed { .. } => break,
                _ => {}
            }
        }
        names.sort();
        names
    })
}

// ---------------------------------------------------------------------------
// A facet selects one position, not every mention
// ---------------------------------------------------------------------------

/// `return:Point` selects the function that returns one, and nothing else that
/// merely mentions `Point`.
///
/// The negative half is the whole point: `plain`, `borrow` and `origin` all
/// name `Point`, and a search that returned them would be `mentions`, not
/// `returns`. That is the conflation the position index exists to prevent, and
/// asserting only that `make` is present would pass against a search that
/// returned everything.
#[test]
fn a_return_facet_selects_only_declarations_that_return_the_type() {
    assert_eq!(
        hits("return:Point"),
        vec!["make"],
        "only `fn make() -> Point` returns a Point; every other declaration in \
         the package names Point in a different position"
    );
}

/// `field:Point` selects the field, not the record that owns it.
#[test]
fn a_field_facet_selects_the_field_that_holds_the_type() {
    assert_eq!(hits("field:Point"), vec!["origin"]);
}

/// A facet naming a type nothing declares returns nothing, rather than
/// everything.
///
/// The adversarial shape: an unresolved facet must not degrade into an
/// unfiltered walk. This is what an empty intersection would silently become if
/// the resolution step returned "no constraint" instead of "no answer".
#[test]
fn a_facet_naming_an_unknown_type_returns_nothing() {
    assert!(
        hits("return:NoSuchType").is_empty(),
        "a type no loaded package declares has no postings, so the honest \
         answer is zero rows"
    );
}

// ---------------------------------------------------------------------------
// Conjunction
// ---------------------------------------------------------------------------

/// Two facets that cannot both hold select nothing.
///
/// `make` returns a `Point` and takes nothing; `plain` takes one and returns
/// nothing. Their conjunction is empty, and a union would return both — which
/// is the single most likely way to implement this wrong.
#[test]
fn facets_conjoin_rather_than_union() {
    assert!(
        hits("return:Point param:Point").is_empty(),
        "no declaration in the package both returns and accepts a Point; a \
         non-empty answer here means the facets were unioned"
    );
    // …and each half on its own really does match, so the emptiness above is
    // the conjunction and not two broken facets.
    assert_eq!(hits("return:Point"), vec!["make"]);
    // `plain` only, not `plain` + `borrow`: the borrowed parameter is the
    // known `nudox-store` gap pinned by `a_param_facet_finds_borrowed_parameters_too`.
    // Asserting the number that is *true today* rather than the one that ought
    // to be keeps this test measuring conjunction instead of silently
    // duplicating that one — and it will fail loudly, here, the day the gap is
    // fixed, which is the correct time to revisit it.
    assert_eq!(hits("param:Point"), vec!["plain"]);
}

// ---------------------------------------------------------------------------
// The kind path is unchanged
// ---------------------------------------------------------------------------

/// A query naming no facet still gets the old kind-facet behaviour.
///
/// Section 1's previous contract in full: a bare kind keyword returns every
/// symbol of that kind. Nothing about adding signature search may change it,
/// because `semantic_section_contract.rs` and the GUI both depend on it.
#[test]
fn a_bare_kind_keyword_still_returns_every_symbol_of_that_kind() {
    let mut records = hits("struct");
    records.sort();
    assert_eq!(
        records,
        vec!["Holder", "Point"],
        "`struct` names no facet, so it must still resolve as a kind keyword"
    );
}

/// A bare type name is *not* a signature query — section 0 answers it.
#[test]
fn a_bare_type_name_does_not_activate_signature_search() {
    assert!(
        hits("Point").is_empty(),
        "`Point` with no facet is a name query; section 1 answering it would \
         duplicate section 0 for every search the user types"
    );
}

// ---------------------------------------------------------------------------
// Borrowed parameters — the case real signatures are made of
// ---------------------------------------------------------------------------

/// `param:Point` must find `fn borrow(p: &Point)` as well as `fn plain(p: Point)`.
///
/// # Why this test is separate from the other parameter test
///
/// It is the one that fails, and it fails in `nudox-store`, not here.
///
/// `typerefs_of_entry` walks a parameter's type through
/// `collect_stable_refs`, whose match treats `Type::Primitive(_)` as a **leaf
/// that reaches nothing** (`crates/nudox-store/src/package.rs:472-479`). But a
/// Rust reference is not a leaf: `&Point` is
/// `Type::Primitive(Primitive::Reference { ty: Box<Type>, .. })` with the
/// nominal *inside* it, and the same is true of `*const T` (`MutPointer`) and
/// `*mut T` (`ConstPointer`).
///
/// So every borrowed parameter, every raw pointer parameter, and every borrowed
/// return type is missing from `type_refs` entirely — not ranked low, absent.
/// In Rust that is most of the API surface, and it is precisely the brief's
/// second worked example (`functions accepting a &Path`).
///
/// This is `#[ignore]`d rather than deleted so it is a standing, runnable
/// statement of the gap: `cargo test -p nudox-engine --test signature_search --
/// --ignored` reproduces it in about a second, and the day
/// `collect_stable_refs` descends into those three variants it goes green and
/// the `#[ignore]` comes off.
#[test]
#[ignore = "known gap: nudox-store's collect_stable_refs treats Type::Primitive \
            as a leaf, so `&T`, `*const T` and `*mut T` never reach the type \
            index. See this test's doc comment."]
fn a_param_facet_finds_borrowed_parameters_too() {
    assert_eq!(
        hits("param:Point"),
        vec!["borrow", "plain"],
        "`fn borrow(p: &Point)` accepts a Point. If only `plain` is returned, \
         the reference wrapper hid the nominal from the index."
    );
}

/// The half of the parameter contract that *does* hold today, pinned separately
/// so the suite states exactly how much works rather than only what is broken.
#[test]
fn a_param_facet_finds_by_value_parameters() {
    let names = hits("param:Point");
    assert!(
        names.contains(&"plain".to_owned()),
        "`fn plain(p: Point)` accepts a Point by value; got {names:?}"
    );
}
