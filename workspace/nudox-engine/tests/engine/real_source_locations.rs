//! L31/L42 end-to-end: a real declaration reaches the **wire** as somewhere a
//! reader can go.
//!
//! `workspace/compiler/languages/rust/tests/source_locations.rs` proves the
//! producer records the right file and line. This proves the value survives the
//! rest of the way — through `IrView`, `PackageView`, the corpus, the head
//! chunker, and the impl pager — to the two wire types the GUI actually reads:
//! [`SymbolHead::source`] and [`ImplRow::source`].
//!
//! That seam is where the defect lived. The IR has carried *some* span since
//! the beginning; what reached the GUI was `<file-id-806>` and `bytes
//! 8880–9435`, because the head chunker re-derived "is this recorded?" from an
//! empty-path test and the wire had no way to say "these are bytes, not lines".
//! A producer-side test alone would have been green throughout.
//!
//! Both assertions are checked against `result/memchr-2.8.3` on disk, so
//! neither can be satisfied by a fabricated path.
//!
//! # Cost
//!
//! Drives in-process rust-analyzer over the real checkout: ~30 s. Deliberately
//! **not** `#[ignore]`d — the whole point of L31 is that this path was never
//! measured end to end, and an ignored test measures nothing.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures::stream::BoxStream;

use nudox_ir::{
    change::{IntroId, PackageLineageId, StableRef},
    kind::Kind,
    view::IrView,
};
use nudox_languages::produce;
use nudox_languages::rust::RustProducer;
use nudox_engine::store::{
    package::{PackageView, Provenance},
    source::{
        IrSource, LoadEvent, LoadRequest, PackageHint, SourceDescriptor, Error,
        producer::PackageDescriptor,
    },
};

use nudox_engine::{
    Engine, EngineConfig, Gen,
    wire::{DocEvent, ImplsPage, SourceLocation},
};

const PKG_NAME: &str = "memchr";
const PKG_VERSION: &str = "2.8.3";

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../result/memchr-2.8.3")
}

// ---------------------------------------------------------------------------
// A one-package IrSource (mirrors impls_refs_flows.rs — test_support is not
// importable from here)
// ---------------------------------------------------------------------------

struct StaticSource {
    lineage: PackageLineageId,
    package: Arc<PackageView>,
}

impl IrSource for StaticSource {
    fn describe(&self) -> SourceDescriptor {
        SourceDescriptor {
            label: "real-source-locations".to_owned(),
            package_count_hint: Some(1),
        }
    }

    fn load(&self, _req: LoadRequest) -> BoxStream<'static, Result<LoadEvent, Error>> {
        use futures::StreamExt as _;
        let events: Vec<Result<LoadEvent, Error>> = vec![
            Ok(LoadEvent::Discovered {
                lineage: self.lineage.clone(),
                hint: PackageHint {
                    display_name: PKG_NAME.to_owned(),
                    ecosystem: "cargo".to_owned(),
                    version: Some(PKG_VERSION.to_owned()),
                },
            }),
            Ok(LoadEvent::Ready {
                package: Arc::clone(&self.package),
            }),
        ];
        futures::stream::iter(events).boxed()
    }
}

/// Lower the real checkout into a `PackageView`.
fn real_package() -> (PackageLineageId, Arc<PackageView>) {
    let root = root();
    assert!(
        root.join("Cargo.toml").is_file(),
        "no checkout at {} — run `nix build .#checks.corpus`",
        root.display()
    );

    let descriptor = PackageDescriptor::cargo(&root, PKG_NAME, PKG_VERSION);
    let (produced, _cost) = heart::cost::measured(
        &format!("l31/wire/{PKG_NAME}-{PKG_VERSION}"),
        &root,
        || {
            produce(
                &RustProducer { direct_repo: false },
                &descriptor.source,
                &descriptor.lineage,
                &nudox_ir::foreign::Unlinked,
            )
        },
    );
    let table = produced
        .unwrap_or_else(|e| panic!("{PKG_NAME} must lower: {e}"))
        .table;

    let view = IrView::with_package(descriptor.lineage.clone(), table);
    (
        descriptor.lineage.clone(),
        Arc::new(PackageView::build(view, Provenance::TrustedLocal)),
    )
}

async fn start_and_settle(
    lineage: PackageLineageId,
    package: Arc<PackageView>,
) -> nudox_engine::EngineHandle {
    let expected = package.view().table().len() as u64;
    let engine = Engine::start(
        EngineConfig::default(),
        StaticSource {
            lineage: lineage.clone(),
            package,
        },
    );
    for _ in 0..200 {
        if engine.versions(&lineage).current().map(|v| v.symbol_count) == Some(expected) {
            tokio::time::sleep(Duration::from_millis(5)).await;
            return engine;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("engine never settled for {lineage}");
}

async fn drain(
    engine: &nudox_engine::EngineHandle,
    key: StableRef,
) -> Vec<DocEvent> {
    let (handle, rx) = engine.open_symbol(key, Gen(1));
    let mut events = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(20), async {
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

/// 1-based line of `offset`, derived from the file rather than from the value
/// under test.
fn line_of(text: &str, offset: usize) -> u32 {
    (text[..offset].matches('\n').count() + 1) as u32
}

/// The invariant: opening `memchr` yields a `SymbolHead` whose `source` is a
/// navigable location naming the file and line the declaration is really at,
/// and at least one `ImplRow` on an impl-bearing type carries one too.
///
/// One test, one 30-second lowering, both wire types — because they are the
/// same claim ("the location survives to the seam") and running the oracle
/// twice to say it twice would be 30 s spent on nothing.
#[test]
fn a_real_declaration_reaches_the_wire_as_a_navigable_location() {
    let (lineage, package) = real_package();

    // Pick the targets out of the lowered table before starting the engine.
    let memchr_fn: IntroId = package
        .view()
        .entries()
        .find(|(_, e)| {
            e.sym().name == "memchr" && matches!(e.kind().as_owned_kind(), Some(Kind::Function(_)))
        })
        .map(|(id, _)| id)
        .expect("memchr must lower a Function named `memchr`");

    // A type that really has impls in this crate. Chosen by looking for one,
    // not by guessing a name, so the test does not silently degrade into
    // "there were no impls" if the producer's naming changes.
    let impl_bearing: IntroId = package
        .view()
        .entries()
        .filter(|(_, e)| matches!(e.kind().as_owned_kind(), Some(Kind::Impl(_))))
        .find_map(|(_, e)| {
            // The impl's own entry is enough to locate its self type via the
            // package's impl index; simpler and more robust is to ask for the
            // record the label names.
            let label = e.sym().name.clone();
            package
                .view()
                .entries()
                .find(|(_, r)| {
                    matches!(r.kind().as_owned_kind(), Some(Kind::Record(_)))
                        && label.contains(&r.sym().name)
                })
                .map(|(id, _)| id)
        })
        .expect("memchr must lower at least one impl over one of its own records");

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("runtime");

    rt.block_on(async move {
        let engine = start_and_settle(lineage.clone(), package).await;

        // ── SymbolHead.source (L42.2) ────────────────────────────────────────
        let events = drain(&engine, StableRef::new(lineage.clone(), memchr_fn)).await;
        let head = events
            .iter()
            .find_map(|e| match e {
                DocEvent::Head(h) => Some(h.clone()),
                _ => None,
            })
            .expect("opening a symbol must emit a Head");

        let SourceLocation::Declared {
            file, bytes, start, ..
        } = head.source.clone()
        else {
            panic!(
                "`memchr`'s head reached the wire as {:?}; only `Declared` can be \
                 turned into a source jump, which is the whole subject of L31",
                head.source
            );
        };

        assert_eq!(&*file, "src/memchr.rs");
        let text = std::fs::read_to_string(root().join(&*file))
            .expect("the wire's path must name a file that exists");
        assert_eq!(
            start.line.get(),
            line_of(&text, bytes[0] as usize),
            "the wire's line disagrees with the file it names"
        );
        assert_eq!(
            head.source.jump_target().as_deref(),
            Some(format!("src/memchr.rs:{}:{}", start.line, start.column).as_str()),
            "a Declared location must render as the path:line:col an editor opens"
        );

        // ── ImplRow.source (L42.3) ───────────────────────────────────────────
        let events = drain(&engine, StableRef::new(lineage.clone(), impl_bearing)).await;
        let pages: Vec<ImplsPage> = events
            .iter()
            .filter_map(|e| match e {
                DocEvent::Impls { page, .. } => Some(page.clone()),
                _ => None,
            })
            .collect();
        let rows: Vec<_> = pages.iter().flat_map(|p| p.impls.iter().cloned()).collect();
        assert!(
            !rows.is_empty(),
            "the chosen type must produce at least one ImplRow, or this half of \
             the test proves nothing"
        );

        let located = rows
            .iter()
            .find(|r| matches!(r.source, SourceLocation::Declared { .. }))
            .unwrap_or_else(|| {
                panic!(
                    "no ImplRow carries a navigable location; got {:?}",
                    rows.iter().map(|r| &r.source).collect::<Vec<_>>()
                )
            });

        let SourceLocation::Declared { file, bytes, .. } = &located.source else {
            unreachable!("filtered above")
        };
        let impl_text = std::fs::read_to_string(root().join(&**file))
            .expect("an ImplRow's path must name a file that exists");
        let slice = impl_text
            .get(bytes[0] as usize..bytes[1] as usize)
            .expect("an ImplRow's byte range must be inside the file it names");
        assert!(
            slice.contains("impl"),
            "the range an ImplRow points at does not contain an `impl` keyword; \
             the row and its location describe different things:\n{}",
            &slice[..slice.len().min(160)]
        );
    });
}
