//! Integration tests for `DocEvent::Impls` and `DocEvent::Refs` emission.
//!
//! # Why these tests exist
//!
//! `DocEvent::Impls` and `DocEvent::Refs` were never emitted before 2026-07-28.
//! The GUI store had correct accumulation arms for them, and the wire protocol
//! declared both page types, but `stream_symbol` in `doc.rs` simply never
//! called `collect_impls` or `collect_refs`.  The result was that the
//! Implementations and References tabs were permanently empty for every symbol.
//!
//! These tests verify the new emission end-to-end: hand-build a package that
//! contains a type and impl blocks for it, open the symbol page, and assert that
//! the correct `Impls` / `Refs` events arrive.
//!
//! # Design choices
//!
//! * All packages are hand-built via `PristineIntroTable` — no producer, no
//!   toolchain, no filesystem.  This is the pattern established in
//!   `crate::test_support` and mirrored in `versions_adversarial.rs`.
//! * The `StaticSource` wrapper duplicated here follows the same pattern as
//!   `versions_adversarial.rs` — the test-support module is `pub(crate)` and
//!   cannot be imported from integration tests.
//! * No `#[tokio::test]` timeout is set explicitly; Tokio's default test runner
//!   keeps these bounded.  The `settle` helper polls for up to ~500 ms which is
//!   more than enough for an in-memory package.

use std::sync::Arc;
use std::time::Duration;

use futures::stream::BoxStream;

use nudox_engine::store::{
    package::{PackageView, Provenance},
    source::{Error, IrSource, LoadEvent, LoadRequest, PackageHint, SourceDescriptor},
};
use nudox_ir::{
    apply::PristineIntroTable,
    change::{EcosystemId, IntroId, PackageLineageId, PackageName, StableRef},
    entry::{ByteSpan, Entry, LineCol, Node, SourceFile, SourceLocation, Symbol, Visibility},
    index::{RawRef, Ref},
    kind::Kind,
    kinds::{Impl, ImplFlags, Module, Record, Type},
    view::IrView,
    vocab::{Confidence, Occurrence, ReferenceKind, RelSpan},
};

use nudox_engine::{
    Engine, EngineConfig,
    wire::{DocEvent, Gen, ImplsPage, RefsPage, RenderSection},
};

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn lineage(name: &str) -> PackageLineageId {
    PackageLineageId::new(EcosystemId::new("test"), PackageName::new(name))
}

fn intro(n: u8) -> IntroId {
    IntroId::from_raw([n; 32])
}

fn blank_sym(name: &str) -> Symbol {
    Symbol {
        name: name.to_owned(),
        visibility: Visibility::Public,
        documentation: String::new(),
        source: std::path::PathBuf::from("src/lib.rs"),
        span: 0..0,
        aliases: Box::new([]),
        deprecation: None,
        doc_links: Box::new([]),
        attrs: Box::new([]),
        cfg: None,
    }
}

fn module_entry(name: &str) -> Entry {
    Entry::new(
        blank_sym(name),
        Node::build(None::<RawRef>, []),
        Kind::Module(Module),
    )
}

fn record_entry(name: &str) -> Entry {
    Entry::new(
        blank_sym(name),
        Node::build(None::<RawRef>, []),
        Kind::Record(Record::builder().build()),
    )
}

/// Build an inherent impl entry (`impl <self_ty>`).
fn inherent_impl_entry(name: &str, self_ty_intro: IntroId) -> Entry {
    let impl_ = Impl::builder()
        .maybe_of(None)
        .self_ty(Type::Nominal(Ref::Intro(self_ty_intro)))
        .build();
    Entry::new(
        blank_sym(name),
        Node::build(None::<RawRef>, []),
        Kind::Impl(impl_),
    )
}

/// An impl whose producer supplied the complete typed source location.
fn located_inherent_impl_entry(name: &str, self_ty_intro: IntroId) -> Entry {
    let entry = inherent_impl_entry(name, self_ty_intro);
    Entry::new_located(
        entry.sym().clone(),
        Node::build(None::<RawRef>, []),
        match entry.kind().as_owned_kind() {
            Some(Kind::Impl(impl_)) => Kind::Impl(impl_.clone()),
            _ => unreachable!(),
        },
        SourceLocation::Declared {
            file: SourceFile::new("src/router.rs").expect("relative source file"),
            bytes: ByteSpan::new(120, 160).expect("non-empty source span"),
            start: LineCol::from_zero_based(11, 2),
            end: LineCol::from_zero_based(11, 42),
        },
    )
}

fn located_member_entry(name: &str) -> Entry {
    Entry::new_located(
        blank_sym(name),
        Node::build(None::<RawRef>, []),
        Kind::Function(nudox_ir::kinds::Function::builder().build()),
        SourceLocation::Declared {
            file: SourceFile::new("src/router.rs").expect("relative source file"),
            bytes: ByteSpan::new(40, 75).expect("non-empty source span"),
            start: LineCol::from_zero_based(3, 0),
            end: LineCol::from_zero_based(3, 35),
        },
    )
}

/// Build a trait impl entry (`impl <trait_intro> for <self_ty_intro>`).
fn trait_impl_entry(name: &str, self_ty_intro: IntroId, trait_intro: IntroId) -> Entry {
    let impl_ = Impl::builder()
        .maybe_of(Some(Type::Nominal(Ref::Intro(trait_intro))))
        .self_ty(Type::Nominal(Ref::Intro(self_ty_intro)))
        .build();
    Entry::new(
        blank_sym(name),
        Node::build(None::<RawRef>, []),
        Kind::Impl(impl_),
    )
}

/// Build a blanket impl entry (`impl<T: Bound> <trait_intro> for T`).
///
/// The self type is left as `Type::Any` (representing the type variable `T`)
/// to trigger `ImplFlags::blanket`.  The `self_ty_intro` is used as the
/// nominal base here because the filter in `collect_impls` matches against
/// `ty_matches`, which looks for `Nominal(Ref::Intro(target))`.  A blanket
/// impl whose `self_ty` is `Any` will NOT match the normal filter — that is
/// correct behaviour (blanket impls don't target a specific type).
///
/// For the blanket-flag tests we therefore set the `self_ty` to the target's
/// nominal AND set `flags.blanket = true`, so the impl shows up in the page
/// (it matches `self_ty`) and the flag is exposed on the row.
fn blanket_impl_entry(name: &str, self_ty_intro: IntroId, trait_intro: IntroId) -> Entry {
    let impl_ = Impl::builder()
        .flags(ImplFlags {
            blanket: true,
            negative: false,
        })
        .maybe_of(Some(Type::Nominal(Ref::Intro(trait_intro))))
        .self_ty(Type::Nominal(Ref::Intro(self_ty_intro)))
        .build();
    Entry::new(
        blank_sym(name),
        Node::build(None::<RawRef>, []),
        Kind::Impl(impl_),
    )
}

/// Build a trait impl whose self type is `Type::Apply { base: Nominal(self_ty_intro), args }`.
///
/// Used to test `self_generic_count`: the outer `Apply` carries `args.len()` generic
/// arguments, which `collect_impls` counts as `self_generic_count`.
fn generic_impl_entry(
    name: &str,
    self_ty_intro: IntroId,
    trait_intro: IntroId,
    arg_count: usize,
) -> Entry {
    // `List<T>` in nudox-ir is `Box<[T]>` (pub(crate)).  `Box::from(Vec<T>)`
    // gives us a `Box<[T]>` without needing to name the alias.
    let args_vec: Vec<Type> = (0..arg_count).map(|_| Type::Any).collect();
    let self_ty = Type::Apply {
        base: Box::new(Type::Nominal(Ref::Intro(self_ty_intro))),
        args: args_vec.into_boxed_slice(),
    };
    let impl_ = Impl::builder()
        .maybe_of(Some(Type::Nominal(Ref::Intro(trait_intro))))
        .self_ty(self_ty)
        .build();
    Entry::new(
        blank_sym(name),
        Node::build(None::<RawRef>, []),
        Kind::Impl(impl_),
    )
}

// ---------------------------------------------------------------------------
// StaticSource  (mirrors versions_adversarial.rs — cannot import test_support)
// ---------------------------------------------------------------------------

struct StaticSource {
    items: Vec<(PackageLineageId, String, Arc<PackageView>)>,
}

impl StaticSource {
    fn single(lid: PackageLineageId, version: &str, pkg: Arc<PackageView>) -> Self {
        Self {
            items: vec![(lid, version.to_owned(), pkg)],
        }
    }
}

impl IrSource for StaticSource {
    fn describe(&self) -> SourceDescriptor {
        SourceDescriptor {
            label: "impls-refs-static".to_owned(),
            package_count_hint: Some(self.items.len() as u32),
        }
    }

    fn load(&self, _req: LoadRequest) -> BoxStream<'static, Result<LoadEvent, Error>> {
        use futures::StreamExt as _;
        let events: Vec<Result<LoadEvent, Error>> = self
            .items
            .iter()
            .flat_map(|(lid, v, pkg)| {
                [
                    Ok(LoadEvent::Discovered {
                        lineage: lid.clone(),
                        hint: PackageHint {
                            display_name: lid.name.as_str().to_owned(),
                            ecosystem: lid.ecosystem.as_str().to_owned(),
                            version: Some(v.clone()),
                        },
                    }),
                    Ok(LoadEvent::Ready {
                        package: Arc::clone(pkg),
                    }),
                ]
            })
            .collect();
        futures::stream::iter(events).boxed()
    }
}

/// Start an engine over a single package and wait until the corpus holds it.
async fn start_and_settle(
    lid: PackageLineageId,
    pkg: Arc<PackageView>,
) -> nudox_engine::EngineHandle {
    let source = StaticSource::single(lid.clone(), "1.0.0", pkg.clone());
    let engine = Engine::start(EngineConfig::default(), source);

    // Poll until the corpus holds a package whose entry count matches.
    let expected = pkg.view().table().len() as u64;
    for _ in 0..50 {
        let found = engine.versions(&lid).current().map(|v| v.symbol_count);
        if found == Some(expected) {
            // Give the corpus write one more tick to catch up.
            tokio::time::sleep(Duration::from_millis(5)).await;
            return engine;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("engine never settled for package {lid}");
}

/// Collect all DocEvents for `key` to completion.
async fn drain(
    engine: &nudox_engine::EngineHandle,
    key: StableRef,
    generation: u64,
) -> Vec<DocEvent> {
    let (handle, rx) = engine.open_symbol(key, Gen(generation));
    let mut events = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(5), async {
        while let Ok(ev) = rx.recv_async().await {
            let done = matches!(ev, DocEvent::Done | DocEvent::Failed(_));
            events.push(ev);
            if done {
                break;
            }
        }
    })
    .await;
    drop(handle);
    events
}

/// Collect all `ImplsPage`s from an event stream.
fn collect_impls_pages(events: &[DocEvent]) -> Vec<ImplsPage> {
    events
        .iter()
        .filter_map(|e| match e {
            DocEvent::Impls { page, .. } => Some(page.clone()),
            _ => None,
        })
        .collect()
}

/// Collect all `RefsPage`s from an event stream.
fn collect_refs_pages(events: &[DocEvent]) -> Vec<RefsPage> {
    events
        .iter()
        .filter_map(|e| match e {
            DocEvent::Refs { page, .. } => Some(page.clone()),
            _ => None,
        })
        .collect()
}

/// Count the total impls reported across all pages (using any page's `total`).
fn total_impls(events: &[DocEvent]) -> u64 {
    events
        .iter()
        .find_map(|e| match e {
            DocEvent::Impls { page, .. } => Some(page.total),
            _ => None,
        })
        .unwrap_or(0)
}

/// Count the total refs reported across all pages.
fn total_refs(events: &[DocEvent]) -> u64 {
    events
        .iter()
        .find_map(|e| match e {
            DocEvent::Refs { page, .. } => Some(page.total),
            _ => None,
        })
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Tests: Implementations
// ---------------------------------------------------------------------------

/// Opening a symbol that has three impls (inherent + two trait) must yield
/// exactly three `ImplRow`s across the Impls pages.
///
/// Package layout:
///   intro(1) = root module
///   intro(2) = `Router` (struct)
///   intro(3) = `Display` (trait)
///   intro(4) = `Debug` (trait)
///   intro(5) = impl block for Router (inherent)
///   intro(6) = `impl Display for Router`
///   intro(7) = `impl Debug for Router`
#[tokio::test]
async fn three_impls_all_appear_in_impls_events() {
    let lid = lineage("impl-test");

    let mut table = PristineIntroTable::new();
    table.insert_live(intro(1), module_entry("root"), None);
    table.insert_live(intro(2), record_entry("Router"), Some(intro(1)));
    table.insert_live(intro(3), module_entry("Display"), Some(intro(1)));
    table.insert_live(intro(4), module_entry("Debug"), Some(intro(1)));
    table.insert_live(
        intro(5),
        located_inherent_impl_entry("impl Router", intro(2)),
        Some(intro(1)),
    );
    table.insert_live(
        intro(6),
        trait_impl_entry("impl Display for Router", intro(2), intro(3)),
        Some(intro(1)),
    );
    table.insert_live(
        intro(7),
        trait_impl_entry("impl Debug for Router", intro(2), intro(4)),
        Some(intro(1)),
    );
    table.insert_live(intro(8), located_member_entry("route"), Some(intro(2)));

    let view = IrView::with_package(lid.clone(), table);
    let pkg = Arc::new(PackageView::build(view, Provenance::TrustedLocal));

    let engine = start_and_settle(lid.clone(), pkg).await;
    let key = StableRef::new(lid.clone(), intro(2)); // open Router
    let events = drain(&engine, key, 1).await;

    let pages = collect_impls_pages(&events);
    assert!(!pages.is_empty(), "must have at least one Impls page");

    let row_count: usize = pages.iter().map(|p| p.impls.len()).sum();
    assert_eq!(
        row_count, 3,
        "Router must have exactly three impl rows; got {row_count}"
    );
    assert_eq!(total_impls(&events), 3, "total must agree with row count");

    // Every row must carry a non-empty label.
    for page in &pages {
        for row in page.impls.iter() {
            assert!(!row.label.is_empty(), "impl row label must not be empty");
        }
    }

    let located = pages
        .iter()
        .flat_map(|page| page.impls.iter())
        .find(|row| row.label.contains("impl Router"))
        .expect("located impl row must be emitted");
    assert_eq!(
        located.source.jump_target().as_deref(),
        Some("src/router.rs:12:3"),
        "impl rows must preserve the producer's package-relative line location"
    );

    let member_events = drain(&engine, StableRef::new(lid, intro(2)), 2).await;
    let member = member_events.iter().find_map(|event| match event {
        DocEvent::Section(RenderSection::Members { entries, .. }) => {
            entries.iter().find(|row| &*row.name == "route")
        }
        _ => None,
    });
    assert_eq!(
        member.and_then(|row| row.source.jump_target()),
        Some("src/router.rs:4:1".to_owned()),
        "member rows must preserve their package-relative line location"
    );
}

/// Opening a symbol with zero impls must still emit exactly one `Impls` page
/// with `done: true` and an empty `impls` slice.  Without that event the GUI
/// cannot distinguish "none" from "still loading".
#[tokio::test]
async fn zero_impls_emits_one_empty_done_page() {
    let lid = lineage("no-impl-test");

    let mut table = PristineIntroTable::new();
    table.insert_live(intro(1), module_entry("root"), None);
    table.insert_live(intro(2), record_entry("Foo"), Some(intro(1)));
    // No impl entries for Foo.

    let view = IrView::with_package(lid.clone(), table);
    let pkg = Arc::new(PackageView::build(view, Provenance::TrustedLocal));

    let engine = start_and_settle(lid.clone(), pkg).await;
    let key = StableRef::new(lid, intro(2));
    let events = drain(&engine, key, 2).await;

    // Must have exactly one Impls event.
    let impls_events: Vec<_> = events
        .iter()
        .filter(|e| matches!(e, DocEvent::Impls { .. }))
        .collect();
    assert_eq!(
        impls_events.len(),
        1,
        "zero-impl symbol must produce exactly one Impls event"
    );

    // That event must be marked done with an empty slice.
    match impls_events[0] {
        DocEvent::Impls { page, done } => {
            assert!(*done, "the single Impls event must be done");
            assert!(page.impls.is_empty(), "impls slice must be empty");
            assert_eq!(page.total, 0);
        }
        _ => unreachable!(),
    }
}

// ---------------------------------------------------------------------------
// Tests: References
// ---------------------------------------------------------------------------

/// A symbol that is used (called/accessed) by two other symbols must yield
/// those two usages in the Refs pages.
///
/// Package layout:
///   intro(1) = root module
///   intro(2) = `MyFn` (the target — a function)
///   intro(3) = `CallerA` (calls MyFn, confidence=Index)
///   intro(4) = `CallerB` (calls MyFn, confidence=Index)
///   intro(5) = `CallerC` (calls MyFn, confidence=Syntactic — below Index floor, must NOT appear)
#[tokio::test]
async fn two_index_confidence_usages_appear_in_refs_events() {
    use nudox_ir::kinds::Function;

    let lid = lineage("refs-test");

    let target_sr = StableRef::new(lid.clone(), intro(2));

    let fn_entry = Entry::new(
        blank_sym("my_fn"),
        Node::build(None::<RawRef>, []),
        Kind::Function(Function::builder().build()),
    );

    let mut table = PristineIntroTable::new();
    table.insert_live(intro(1), module_entry("root"), None);
    table.insert_live(intro(2), fn_entry, Some(intro(1)));
    table.insert_live(
        intro(3),
        Entry::new(
            blank_sym("caller_a"),
            Node::build(None::<RawRef>, []),
            Kind::Function(Function::builder().build()),
        ),
        Some(intro(1)),
    );
    table.insert_live(
        intro(4),
        Entry::new(
            blank_sym("caller_b"),
            Node::build(None::<RawRef>, []),
            Kind::Function(Function::builder().build()),
        ),
        Some(intro(1)),
    );
    table.insert_live(
        intro(5),
        Entry::new(
            blank_sym("caller_c"),
            Node::build(None::<RawRef>, []),
            Kind::Function(Function::builder().build()),
        ),
        Some(intro(1)),
    );

    let mut view = IrView::with_package(lid.clone(), table);

    // CallerA → MyFn at Index confidence.
    view.add_occurrence(
        intro(3),
        Occurrence::new(
            target_sr.clone(),
            ReferenceKind::FunctionCall,
            Confidence::Index,
            RelSpan::new(0, 5),
        ),
    );
    // CallerB → MyFn at Index confidence.
    view.add_occurrence(
        intro(4),
        Occurrence::new(
            target_sr.clone(),
            ReferenceKind::FunctionCall,
            Confidence::Index,
            RelSpan::new(10, 15),
        ),
    );
    // CallerC → MyFn at Syntactic confidence — below the Index floor; must NOT appear.
    view.add_occurrence(
        intro(5),
        Occurrence::new(
            target_sr,
            ReferenceKind::FunctionCall,
            Confidence::Syntactic,
            RelSpan::new(20, 25),
        ),
    );

    let pkg = Arc::new(PackageView::build(view, Provenance::TrustedLocal));

    let engine = start_and_settle(lid.clone(), pkg).await;
    let key = StableRef::new(lid, intro(2));
    let events = drain(&engine, key, 3).await;

    let pages = collect_refs_pages(&events);
    assert!(!pages.is_empty(), "must have at least one Refs page");

    let row_count: usize = pages.iter().map(|p| p.refs.len()).sum();
    assert_eq!(
        row_count, 2,
        "exactly two Index-confidence callers must appear; got {row_count}"
    );
    assert_eq!(total_refs(&events), 2, "total must agree");

    // The rows must name caller_a and caller_b (paths include "root.caller_a" etc.).
    let paths: Vec<&str> = pages
        .iter()
        .flat_map(|p| p.refs.iter())
        .map(|r| &*r.path)
        .collect();
    assert!(
        paths.iter().any(|p| p.contains("caller_a")),
        "caller_a must appear in refs; got {paths:?}"
    );
    assert!(
        paths.iter().any(|p| p.contains("caller_b")),
        "caller_b must appear in refs; got {paths:?}"
    );
    // Syntactic-confidence caller_c must NOT appear.
    assert!(
        !paths.iter().any(|p| p.contains("caller_c")),
        "caller_c (Syntactic confidence) must not appear; got {paths:?}"
    );
}

/// A symbol with zero usages must emit exactly one Refs page that is done and
/// empty — same reasoning as `zero_impls_emits_one_empty_done_page`.
#[tokio::test]
async fn zero_refs_emits_one_empty_done_page() {
    let lid = lineage("no-refs-test");

    let mut table = PristineIntroTable::new();
    table.insert_live(intro(1), module_entry("root"), None);
    table.insert_live(intro(2), record_entry("Bar"), Some(intro(1)));

    let view = IrView::with_package(lid.clone(), table);
    let pkg = Arc::new(PackageView::build(view, Provenance::TrustedLocal));

    let engine = start_and_settle(lid.clone(), pkg).await;
    let key = StableRef::new(lid, intro(2));
    let events = drain(&engine, key, 4).await;

    let refs_events: Vec<_> = events
        .iter()
        .filter(|e| matches!(e, DocEvent::Refs { .. }))
        .collect();
    assert_eq!(
        refs_events.len(),
        1,
        "zero-refs symbol must produce exactly one Refs event"
    );

    match refs_events[0] {
        DocEvent::Refs { page, done } => {
            assert!(*done, "the single Refs event must be done");
            assert!(page.refs.is_empty(), "refs slice must be empty");
            assert_eq!(page.total, 0);
        }
        _ => unreachable!(),
    }
}

// ---------------------------------------------------------------------------
// Tests: Protocol ordering
// ---------------------------------------------------------------------------

/// §9.3 invariant: `Impls` and `Refs` events must arrive after `Head` and
/// must not arrive after `Done`.
///
/// This test verifies the ordering rule on a stream that contains both
/// `Impls` and `Refs` events.
#[tokio::test]
async fn impls_and_refs_arrive_after_head_and_before_done() {
    use nudox_ir::kinds::Function;

    let lid = lineage("order-test");
    let target_sr = StableRef::new(lid.clone(), intro(2));

    let fn_entry = Entry::new(
        blank_sym("target_fn"),
        Node::build(None::<RawRef>, []),
        Kind::Function(Function::builder().build()),
    );

    let mut table = PristineIntroTable::new();
    table.insert_live(intro(1), module_entry("root"), None);
    table.insert_live(intro(2), fn_entry, Some(intro(1)));
    // One impl for good measure (impl block targets intro(2) as self_ty).
    table.insert_live(
        intro(3),
        inherent_impl_entry("impl target_fn", intro(2)),
        Some(intro(1)),
    );
    // One usage.
    table.insert_live(
        intro(4),
        Entry::new(
            blank_sym("caller"),
            Node::build(None::<RawRef>, []),
            Kind::Function(Function::builder().build()),
        ),
        Some(intro(1)),
    );

    let mut view = IrView::with_package(lid.clone(), table);
    view.add_occurrence(
        intro(4),
        Occurrence::new(
            target_sr,
            ReferenceKind::FunctionCall,
            Confidence::Index,
            RelSpan::new(0, 10),
        ),
    );

    let pkg = Arc::new(PackageView::build(view, Provenance::TrustedLocal));
    let engine = start_and_settle(lid.clone(), pkg).await;
    let key = StableRef::new(lid, intro(2));
    let events = drain(&engine, key, 5).await;

    let mut head_pos: Option<usize> = None;
    let mut done_pos: Option<usize> = None;

    for (i, ev) in events.iter().enumerate() {
        match ev {
            DocEvent::Head(_) => head_pos = Some(i),
            DocEvent::Done => done_pos = Some(i),
            DocEvent::Impls { .. } | DocEvent::Refs { .. } => {
                assert!(
                    head_pos.is_some(),
                    "Impls/Refs event at position {i} arrived before Head"
                );
                assert!(
                    done_pos.is_none(),
                    "Impls/Refs event at position {i} arrived after Done"
                );
            }
            _ => {}
        }
    }

    assert!(head_pos.is_some(), "stream must contain a Head event");
    assert!(done_pos.is_some(), "stream must reach Done");
}

// ---------------------------------------------------------------------------
// Tests: Determinism
// ---------------------------------------------------------------------------

/// Opening the same symbol twice must produce identical `ImplRow` sequences —
/// same order, same labels, same keys.
///
/// This is the machine-checkable form of the "sorted by label" ordering
/// promise: if sorting were non-deterministic the two runs would disagree.
#[tokio::test]
async fn impls_order_is_deterministic_across_two_opens() {
    let lid = lineage("det-test");

    let mut table = PristineIntroTable::new();
    table.insert_live(intro(1), module_entry("root"), None);
    table.insert_live(intro(2), record_entry("Point"), Some(intro(1)));
    table.insert_live(intro(3), module_entry("Display"), Some(intro(1)));
    table.insert_live(intro(4), module_entry("Debug"), Some(intro(1)));
    table.insert_live(intro(5), module_entry("Clone"), Some(intro(1)));
    table.insert_live(
        intro(6),
        inherent_impl_entry("impl Point", intro(2)),
        Some(intro(1)),
    );
    table.insert_live(
        intro(7),
        trait_impl_entry("impl Display for Point", intro(2), intro(3)),
        Some(intro(1)),
    );
    table.insert_live(
        intro(8),
        trait_impl_entry("impl Debug for Point", intro(2), intro(4)),
        Some(intro(1)),
    );
    table.insert_live(
        intro(9),
        trait_impl_entry("impl Clone for Point", intro(2), intro(5)),
        Some(intro(1)),
    );

    let view = IrView::with_package(lid.clone(), table);
    let pkg = Arc::new(PackageView::build(view, Provenance::TrustedLocal));

    let engine = start_and_settle(lid.clone(), pkg).await;
    let key = StableRef::new(lid, intro(2));

    let events1 = drain(&engine, key.clone(), 10).await;
    let events2 = drain(&engine, key, 11).await;

    // Extract the flat sequence of impl row labels from each run.
    let labels_of = |evs: &[DocEvent]| -> Vec<String> {
        evs.iter()
            .filter_map(|e| match e {
                DocEvent::Impls { page, .. } => Some(
                    page.impls
                        .iter()
                        .map(|r| r.label.to_string())
                        .collect::<Vec<_>>(),
                ),
                _ => None,
            })
            .flatten()
            .collect()
    };

    let labels1 = labels_of(&events1);
    let labels2 = labels_of(&events2);

    assert_eq!(labels1.len(), 4, "first run must have 4 impl rows");
    assert_eq!(
        labels1, labels2,
        "impl row order must be identical across two opens of the same symbol"
    );
}

// ---------------------------------------------------------------------------
// Tests: ImplRow wire fields (is_blanket, trait_label, self_generic_count)
// ---------------------------------------------------------------------------

/// A blanket impl (`ImplFlags::blanket = true`) must have `is_blanket == true`
/// on the emitted `ImplRow`, and a non-blanket impl must have `is_blanket == false`.
#[tokio::test]
async fn blanket_impl_flag_is_emitted_on_impl_row() {
    let lid = lineage("blanket-flag-test");

    let mut table = PristineIntroTable::new();
    table.insert_live(intro(1), module_entry("root"), None);
    table.insert_live(intro(2), record_entry("MyType"), Some(intro(1)));
    table.insert_live(intro(3), module_entry("Display"), Some(intro(1)));
    table.insert_live(intro(4), module_entry("ToString"), Some(intro(1)));
    // intro(5): normal (non-blanket) trait impl
    table.insert_live(
        intro(5),
        trait_impl_entry("impl Display for MyType", intro(2), intro(3)),
        Some(intro(1)),
    );
    // intro(6): blanket impl (ImplFlags::blanket = true)
    table.insert_live(
        intro(6),
        blanket_impl_entry("impl<T: Display> ToString for MyType", intro(2), intro(4)),
        Some(intro(1)),
    );

    let view = IrView::with_package(lid.clone(), table);
    let pkg = Arc::new(PackageView::build(view, Provenance::TrustedLocal));
    let engine = start_and_settle(lid.clone(), pkg).await;
    let key = StableRef::new(lid, intro(2));
    let events = drain(&engine, key, 20).await;

    let pages = collect_impls_pages(&events);
    let all_rows: Vec<_> = pages.iter().flat_map(|p| p.impls.iter()).collect();
    assert_eq!(all_rows.len(), 2, "must have two impl rows");

    // Exactly one must be blanket, one must not.
    let blanket_count = all_rows.iter().filter(|r| r.is_blanket).count();
    let non_blanket_count = all_rows.iter().filter(|r| !r.is_blanket).count();
    assert_eq!(blanket_count, 1, "exactly one blanket impl");
    assert_eq!(non_blanket_count, 1, "exactly one non-blanket impl");
}

/// A 16-arity family of trait impls (same trait, self type with 1..=16 type
/// args) must produce 16 rows whose `self_generic_count` values span 1–16.
/// The GUI's grouping layer (in `refs.rs`) collapses them; the engine's job is
/// simply to populate the count correctly.
#[tokio::test]
async fn self_generic_count_is_emitted_for_apply_self_types() {
    let lid = lineage("variadic-count-test");

    let trait_idx: u8 = 50;
    let self_type_idx: u8 = 51;

    let mut table = PristineIntroTable::new();
    table.insert_live(intro(1), module_entry("root"), None);
    table.insert_live(intro(self_type_idx), record_entry("F"), Some(intro(1)));
    table.insert_live(intro(trait_idx), module_entry("Handler"), Some(intro(1)));

    for n in 1_u8..=16 {
        let name = format!("impl Handler for F<{n}>");
        // Use n+100 to avoid colliding with intro(1) ([1;32]) or intro(50/51).
        table.insert_live(
            IntroId::from_raw([100 + n; 32]),
            generic_impl_entry(&name, intro(self_type_idx), intro(trait_idx), n as usize),
            Some(intro(1)),
        );
    }

    let view = IrView::with_package(lid.clone(), table);
    let pkg = Arc::new(PackageView::build(view, Provenance::TrustedLocal));
    let engine = start_and_settle(lid.clone(), pkg).await;
    let key = StableRef::new(lid, intro(self_type_idx));
    let events = drain(&engine, key, 21).await;

    let pages = collect_impls_pages(&events);
    let all_rows: Vec<_> = pages.iter().flat_map(|p| p.impls.iter()).collect();
    assert_eq!(all_rows.len(), 16, "must have 16 impl rows");

    // Collect the counts; they must cover 1..=16.
    let mut counts: Vec<u32> = all_rows.iter().map(|r| r.self_generic_count).collect();
    counts.sort_unstable();
    let expected: Vec<u32> = (1..=16).collect();
    assert_eq!(counts, expected, "self_generic_count must cover 1..=16");
}

/// A two-member arity run must NOT be collapsed by the engine.
/// (Collapsing is a GUI concern; the engine emits raw rows.)
/// This test confirms two rows are emitted with the correct `self_generic_count`
/// values — the threshold check happens in the GUI layer.
#[tokio::test]
async fn two_member_arity_run_emits_two_rows_not_one() {
    let lid = lineage("two-member-arity-test");

    let trait_idx: u8 = 50;
    let self_type_idx: u8 = 51;

    let mut table = PristineIntroTable::new();
    table.insert_live(intro(1), module_entry("root"), None);
    table.insert_live(intro(self_type_idx), record_entry("F"), Some(intro(1)));
    table.insert_live(intro(trait_idx), module_entry("Handler"), Some(intro(1)));

    // Only two arities — does not meet the ≥3 threshold.
    // Use 101/102 to avoid colliding with intro(1) ([1;32]) or intro(50/51).
    table.insert_live(
        IntroId::from_raw([101u8; 32]),
        generic_impl_entry(
            "impl Handler for F<T1>",
            intro(self_type_idx),
            intro(trait_idx),
            1,
        ),
        Some(intro(1)),
    );
    table.insert_live(
        IntroId::from_raw([102u8; 32]),
        generic_impl_entry(
            "impl Handler for F<T1,T2>",
            intro(self_type_idx),
            intro(trait_idx),
            2,
        ),
        Some(intro(1)),
    );

    let view = IrView::with_package(lid.clone(), table);
    let pkg = Arc::new(PackageView::build(view, Provenance::TrustedLocal));
    let engine = start_and_settle(lid.clone(), pkg).await;
    let key = StableRef::new(lid, intro(self_type_idx));
    let events = drain(&engine, key, 22).await;

    let pages = collect_impls_pages(&events);
    let all_rows: Vec<_> = pages.iter().flat_map(|p| p.impls.iter()).collect();
    assert_eq!(
        all_rows.len(),
        2,
        "engine emits both rows; GUI decides whether to collapse"
    );

    let mut counts: Vec<u32> = all_rows.iter().map(|r| r.self_generic_count).collect();
    counts.sort_unstable();
    assert_eq!(counts, vec![1, 2], "two rows with counts 1 and 2");
}

/// Non-consecutive arities must produce distinct rows with the correct counts.
#[tokio::test]
async fn non_consecutive_arities_emit_separate_rows() {
    let lid = lineage("non-consec-arity-test");

    let trait_idx: u8 = 50;
    let self_type_idx: u8 = 51;

    let mut table = PristineIntroTable::new();
    table.insert_live(intro(1), module_entry("root"), None);
    table.insert_live(intro(self_type_idx), record_entry("F"), Some(intro(1)));
    table.insert_live(intro(trait_idx), module_entry("Handler"), Some(intro(1)));

    // Arities 1, 2, 5 — non-consecutive.
    // Use n+100 to avoid colliding with intro(1) ([1;32]) or intro(50/51).
    for &n in &[1u8, 2, 5] {
        table.insert_live(
            IntroId::from_raw([100 + n; 32]),
            generic_impl_entry(
                &format!("impl Handler for F<{n}>"),
                intro(self_type_idx),
                intro(trait_idx),
                n as usize,
            ),
            Some(intro(1)),
        );
    }

    let view = IrView::with_package(lid.clone(), table);
    let pkg = Arc::new(PackageView::build(view, Provenance::TrustedLocal));
    let engine = start_and_settle(lid.clone(), pkg).await;
    let key = StableRef::new(lid, intro(self_type_idx));
    let events = drain(&engine, key, 23).await;

    let pages = collect_impls_pages(&events);
    let all_rows: Vec<_> = pages.iter().flat_map(|p| p.impls.iter()).collect();
    assert_eq!(
        all_rows.len(),
        3,
        "three non-consecutive arities produce three rows"
    );

    let mut counts: Vec<u32> = all_rows.iter().map(|r| r.self_generic_count).collect();
    counts.sort_unstable();
    assert_eq!(counts, vec![1, 2, 5], "non-consecutive counts 1, 2, 5");
}
