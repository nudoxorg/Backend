//! Schedules the production composer and page admission; no native GUI credit.

use super::*;
use crate::core::VersionedRoot;
use crate::model::pages::{Known, Landing, PageStore};
use backend_library::{Document, Fragment, SourceAvailability, SourceExcerpt, SourceExcerptExtent};
use std::sync::mpsc;
use std::time::Duration;

fn symbol() -> SymbolRef {
    SymbolRef::new("/workspace/primary::src/item.rs:7::Item").expect("fixture coordinate")
}

fn document() -> Document {
    let mut document = Document::new(
        backend_library::symbol_key("primary Item"),
        backend_library::view_state_root(&[]),
        vec![Fragment::Text("Authenticated primary document".into())],
    );
    document.signature = Some("pub struct Item".into());
    document.location = SourceAvailability::Captured(
        backend_library::SourceLocation::new("src/item.rs", 7).expect("captured location"),
    );
    document.excerpt = SourceExcerpt::captured("pub struct Item {}", SourceExcerptExtent::Complete)
        .expect("captured source");
    document
}

struct PrimaryEngine {
    cancel_in_document: Option<CancellationToken>,
    secondary_entered: Option<mpsc::Sender<()>>,
    secondary_release: Option<mpsc::Receiver<()>>,
    secondary_calls: usize,
}

impl Engine for PrimaryEngine {
    fn revision(&mut self) -> Result<ViewStateRoot, ClientError> {
        Ok(backend_library::view_state_root(&[]))
    }
    fn health(&mut self) -> Result<HealthReport, ClientError> {
        Err(ClientError::Io("unused health".into()))
    }
    fn probe(&mut self, probe: Probe<'_>) -> Result<ReplyDto, ClientError> {
        if matches!(probe, Probe::Document(_)) {
            if let Some(cancel) = &self.cancel_in_document {
                cancel.cancel();
            }
            return Ok(ReplyDto::new(1, CommandReply::Document(document())));
        }
        self.secondary_calls += 1;
        if matches!(probe, Probe::Related(_)) {
            if let Some(entered) = self.secondary_entered.take() {
                entered.send(()).expect("secondary entered");
            }
            if let Some(release) = self.secondary_release.take() {
                release
                    .recv_timeout(Duration::from_secs(3))
                    .expect("secondary released");
            }
        }
        Err(ClientError::Io("optional read unavailable".into()))
    }
    fn surface(&mut self, _: SurfaceCommand) -> Result<SurfaceReply, ClientError> {
        Err(ClientError::Io("optional references unavailable".into()))
    }
}

fn engine() -> PrimaryEngine {
    PrimaryEngine {
        cancel_in_document: None,
        secondary_entered: None,
        secondary_release: None,
        secondary_calls: 0,
    }
}

fn assert_primary_admission(primary: &crate::model::pages::store::PrimarySymbol) {
    let key = PageKey::Symbol(symbol());
    let root = VersionedRoot::synthetic(document().basis, 1);
    let mut store = PageStore::default();
    let generation = store
        .begin(&key, root)
        .expect("slot admission")
        .expect("read generation");
    assert_eq!(
        store.stage(&key, generation, PageValue::PrimarySymbol(primary.clone())),
        Landing::Applied
    );
    assert_eq!(
        store.inflight(&key),
        Some(generation),
        "optional refinement still owns the read"
    );
    let resource = store.symbol(&symbol());
    assert!(
        crate::core::admit_resource(&resource, root, true).allows_actions(),
        "the authenticated primary document is Current before optional reads finish"
    );
    assert!(
        !crate::core::admit_resource(&resource, root, false).allows_actions(),
        "primary readiness cannot bypass serving-owner admission"
    );
}

#[test]
fn primary_symbol_page_arrives_before_blocked_secondary_and_still_honours_cancel() {
    for cancelled in [false, true] {
        let cancel = CancellationToken::new();
        let worker_cancel = cancel.clone();
        let (pages, received) = mpsc::channel();
        let (entered, secondary) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut engine = engine();
            engine.secondary_entered = Some(entered);
            engine.secondary_release = Some(released);
            let outlines = OutlineCache::default();
            let publish = |page| pages.send(page).expect("primary delivered");
            compose_symbol(
                &mut engine,
                &symbol(),
                &ReadContext {
                    worker: 0,
                    cancel: &worker_cancel,
                    outlines: &outlines,
                    progress: Some(&publish),
                },
            )
        });
        secondary
            .recv_timeout(Duration::from_secs(2))
            .expect("secondary blocked");
        let primary = received
            .try_recv()
            .expect("primary preceded the blocked secondary");
        let PageValue::PrimarySymbol(primary) = primary else {
            panic!("authenticated primary symbol");
        };
        assert_primary_admission(&primary);
        let page = primary.page();
        assert_eq!(page.identity.name.as_ref(), "Item");
        assert_eq!(
            page.site
                .location
                .known()
                .map(|location| (location.path.as_ref(), location.line)),
            Some(("src/item.rs", 7)),
            "captured source location is already available"
        );
        assert_eq!(
            page.site
                .excerpt
                .known()
                .map(|excerpt| (excerpt.text.as_ref(), excerpt.complete)),
            Some(("pub struct Item {}", true)),
            "complete captured source text is already available"
        );
        assert_eq!(
            page.references.gap().map(|gap| gap.reason),
            Some(GapReason::Stale)
        );
        assert_eq!(
            page.members.gap().map(|gap| gap.reason),
            Some(GapReason::Stale)
        );
        assert!(!worker.is_finished(), "the optional read is genuinely held");
        if cancelled {
            cancel.cancel();
        }
        release.send(()).expect("release optional read");
        let result = worker.join().expect("composer worker");
        if cancelled {
            assert!(matches!(result, Err(ReadFailure::Cancelled)));
            assert!(
                received.try_recv().is_err(),
                "cancelled enrichment cannot publish"
            );
        } else {
            let PageValue::Symbol(final_page) = result.expect("primary remains useful") else {
                panic!("final symbol");
            };
            assert_eq!(final_page.identity, page.identity);
            assert_eq!(
                final_page.references.gap().map(|gap| gap.reason),
                Some(GapReason::ReadFailed)
            );
        }
    }
}

#[test]
fn cancelled_primary_document_or_delivery_never_starts_secondary_reads() {
    for during_document in [true, false] {
        let cancel = CancellationToken::new();
        let mut engine = engine();
        engine.cancel_in_document = during_document.then(|| cancel.clone());
        let publications = std::cell::Cell::new(0);
        let publish = |_| {
            publications.set(publications.get() + 1);
            cancel.cancel();
        };
        let outlines = OutlineCache::default();
        let result = compose_symbol(
            &mut engine,
            &symbol(),
            &ReadContext {
                worker: 0,
                cancel: &cancel,
                outlines: &outlines,
                progress: Some(&publish),
            },
        );
        assert!(matches!(result, Err(ReadFailure::Cancelled)));
        assert_eq!(publications.get(), usize::from(!during_document));
        assert_eq!(engine.secondary_calls, 0);
    }
}

#[test]
fn primary_symbol_partial_keeps_exact_generation_admission_after_owner_replacement() {
    let cancel = CancellationToken::new();
    let outlines = OutlineCache::default();
    let partials = std::cell::RefCell::new(Vec::new());
    let publish = |page| partials.borrow_mut().push(page);
    compose_symbol(
        &mut engine(),
        &symbol(),
        &ReadContext {
            worker: 0,
            cancel: &cancel,
            outlines: &outlines,
            progress: Some(&publish),
        },
    )
    .expect("primary composer");
    let primary = partials.borrow()[0].clone();
    let key = PageKey::Symbol(symbol());
    let root = |epoch| VersionedRoot::synthetic(backend_library::view_state_root(&[]), epoch);
    let mut store = PageStore::default();
    let old = store
        .begin(&key, root(1))
        .expect("old admission")
        .expect("old generation");
    assert_eq!(store.stage(&key, old, primary.clone()), Landing::Applied);
    assert!(store.revoke_owner_read(&key));
    let current = store
        .begin(&key, root(2))
        .expect("current admission")
        .expect("current generation");
    assert_eq!(store.stage(&key, old, primary.clone()), Landing::Superseded);
    assert_eq!(
        store.stage(&key, current, primary.clone()),
        Landing::Applied
    );
    assert_eq!(
        store.land(&key, old, Ok(primary.clone())),
        Landing::Superseded
    );
    assert_eq!(store.symbol(&symbol()).value_root(), Some(root(2)));
    assert!(
        matches!(&store.symbol(&symbol()).loaded_value().expect("current partial").references,
        Known::Unknown(gap) if gap.reason == GapReason::Stale)
    );
    let quiet_root = root(2);
    let PageValue::PrimarySymbol(document) = primary.clone() else {
        panic!("typed primary document");
    };
    let mut seeded = PageStore::default();
    assert!(seeded.seed(
        crate::model::pages::store::SeedEntry::Symbol(symbol(), Arc::new(document.page().clone())),
        VersionedRoot::synthetic(
            backend_library::view_state_root(&[("before".into(), "publication".into())]),
            1
        ),
    ));
    let quiet = seeded
        .begin(&key, quiet_root)
        .expect("quiet admission")
        .expect("quiet generation");
    assert_eq!(
        seeded.stage(&key, quiet, PageValue::Symbol(document.page().clone())),
        Landing::Unchanged,
        "generic partial cannot change a quiet launch snapshot"
    );
    assert_eq!(
        seeded.stage(&key, quiet, primary.clone()),
        Landing::Applied,
        "exact primary authenticates the seeded document even during a quiet new-root read"
    );
    assert!(!seeded.is_seeded(&key));
    assert_eq!(seeded.inflight(&key), Some(quiet));
    assert!(
        crate::core::admit_resource(&seeded.symbol(&symbol()), quiet_root, true).allows_actions()
    );
    assert!(
        !crate::core::admit_resource(&seeded.symbol(&symbol()), quiet_root, false).allows_actions()
    );
    assert_eq!(
        seeded.stage(&key, quiet, primary.clone()),
        Landing::Unchanged,
        "already-current primary coalesces within the running generation"
    );
}

#[test]
fn primary_symbol_delivery_refuses_wrong_root_coordinate_package_and_generic_partial() {
    use crate::model::pages::store::PrimarySymbol;
    let root = VersionedRoot::synthetic(document().basis, 1);
    let key = PageKey::Symbol(symbol());
    let cancel = CancellationToken::new();
    let outlines = OutlineCache::default();
    let values = std::cell::RefCell::new(Vec::new());
    let publish = |value| values.borrow_mut().push(value);
    compose_symbol(
        &mut engine(),
        &symbol(),
        &ReadContext {
            worker: 0,
            cancel: &cancel,
            outlines: &outlines,
            progress: Some(&publish),
        },
    )
    .expect("authenticated primary composer");
    let PageValue::PrimarySymbol(primary) = values.borrow()[0].clone() else {
        panic!("typed primary delivery");
    };
    let mut slots = PageStore::default();
    let generation = slots
        .begin(&key, root)
        .expect("slot admission")
        .expect("generation");
    assert_eq!(
        slots.stage(&key, generation, PageValue::Symbol(primary.page().clone())),
        Landing::Applied
    );
    assert!(
        !crate::core::admit_resource(&slots.symbol(&symbol()), root, true).allows_actions(),
        "ordinary Partial never gains the typed primary capability"
    );
    let wrong_root = backend_library::view_state_root(&[("other".into(), "root".into())]);
    let mut wrong_document = document();
    wrong_document.basis = wrong_root;
    let wrong = PrimarySymbol::from_document(&symbol(), &wrong_document, primary.page().clone())
        .expect("root-bound document");
    assert_eq!(
        slots.stage(&key, generation, PageValue::PrimarySymbol(wrong)),
        Landing::Superseded
    );
    let other = SymbolRef::new("/workspace/other::src/item.rs:7::Item").expect("other symbol");
    assert!(PrimarySymbol::from_document(&other, &document(), primary.page().clone()).is_none());
    let mut wrong_package = primary.page().clone();
    wrong_package.package =
        Known::Known(PackageRef::parse("/workspace/other").expect("other package"));
    assert!(PrimarySymbol::from_document(&symbol(), &document(), wrong_package).is_none());
    let mut wrong_document = document();
    wrong_document.symbol = backend_library::symbol_key("other exact row");
    assert!(
        PrimarySymbol::from_document(&symbol(), &wrong_document, primary.page().clone()).is_none()
    );
    assert_eq!(
        slots.stage(
            &PageKey::Symbol(other),
            generation,
            PageValue::PrimarySymbol(primary.clone())
        ),
        Landing::Superseded
    );
    slots.cancel(&key).expect("cancel current generation");
    assert_eq!(
        slots.stage(&key, generation, PageValue::PrimarySymbol(primary)),
        Landing::Superseded
    );
}
