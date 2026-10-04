//! Acquisition stages land through the real root into a mounted package page.

#![allow(clippy::expect_used)]

use super::{Landed, Stage, additions, drain};
use crate::model::pages::{GapReason, Known, PackageRef, PageKey, PageValue, ReadFailure};
use crate::model::release::Release;
use crate::navigation::{PackageLane, PackageRoute, Route};
use crate::runtime::actor::{EngineClient, EngineDto, EngineFault, EngineRequest};
use crate::runtime::reads::{PageReader, ReadContext, ReadPool, ReadRequest};
use crate::shell::tests::{Fixture, PACKAGE, Rig, dossier, registry_dossier, rig_with_engine};
use gpui::{AppContext as _, TestAppContext};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Observations {
    publication: AtomicUsize,
    roots: AtomicUsize,
    reads: Mutex<BTreeMap<PackageRef, usize>>,
}

impl Observations {
    fn count(&self, package: &PackageRef) -> usize {
        self.reads.lock().expect("read counts").get(package).copied().unwrap_or(0)
    }
}

struct PublicationReader(Arc<Observations>);

impl PageReader for PublicationReader {
    fn read(&mut self, request: &ReadRequest, context: &ReadContext<'_>) -> Result<PageValue, ReadFailure> {
        let ReadRequest::Package(package) = request else { return Fixture.read(request, context) };
        *self.0.reads.lock().expect("read counts").entry(package.clone()).or_default() += 1;
        let mut about = if package.is_local() { dossier() } else { registry_dossier(package) };
        let publication = self.0.publication.load(Ordering::Acquire);
        if publication == 0 {
            about.record = Known::unknown(GapReason::NotRecorded, "library record not found");
            about.outline = Known::unknown(GapReason::ReadFailed, "library record not found");
        } else if let Known::Known(record) = &mut about.record {
            record.description = Known::Known(Arc::from(format!("Publication {publication} reached the open page.")));
        }
        Ok(PageValue::Package(about))
    }
}

/// A root read can return the same admitted authority after publication.
/// Advancing it here would hide a missing stage-to-store invalidation.
struct UnchangedRoot(Arc<Observations>);

impl EngineClient for UnchangedRoot {
    fn execute(&mut self, request: &EngineRequest) -> Result<EngineDto, EngineFault> {
        let EngineRequest::Root { request, basis, .. } = request else { return Err(EngineFault::Cancelled) };
        self.0.roots.fetch_add(1, Ordering::Relaxed);
        Ok(EngineDto::Root {
            request: *request,
            basis: *basis,
            key: *basis,
            revision: basis.revision(),
            delta: None,
            project: None,
            catalog: None,
        })
    }
}

fn land(rig: &mut Rig, release: &Release, stage: Stage) {
    let root = rig.graph.root.downgrade();
    rig.cx.update(|_, cx| {
        let additions = additions(cx);
        additions.root = Some(root);
        additions.mailbox.lock().expect("acquisition mailbox").push(Landed::Stage(release.clone(), stage));
        drain(cx);
    });
    rig.settle();
}

#[gpui::test]
fn acquisition_publications_repair_the_mounted_dossier_at_the_same_authority(cx: &mut TestAppContext) {
    let release = Release::new("present", "0.4.2").expect("release");
    let registry = PackageRef::parse(&release.purl()).expect("registry address");
    let source = PackageRef::parse(PACKAGE).expect("source address");
    let active_key = PageKey::Package(registry.clone());
    let source_key = PageKey::Package(source.clone());
    let route = Route::Package(PackageRoute {
        cargo: None,
        project: None,
        package: crate::core::PackageId::new(registry.as_str()).expect("package route"),
        lane: PackageLane::Overview,
        selected: None,
        at: None,
    });
    let observed = Arc::new(Observations::default());
    let reads = Arc::clone(&observed);
    let pool = ReadPool::start(2, move |_| PublicationReader(Arc::clone(&reads))).expect("publication pool");
    let mut rig = rig_with_engine(cx, Some(route), 1440.0, 900.0, pool, UnchangedRoot(Arc::clone(&observed)));
    rig.graph.store.update(rig.cx, |store, cx| { store.ensure(source_key.clone(), cx); });
    rig.settle();
    let before = rig.graph.store.read_with(rig.cx, |store, _| {
        assert!(!store.focused().contains(&source_key));
        assert!(store.package(&registry).loaded_value().expect("initial dossier").record.known().is_none());
        assert!(store.package(&registry).loaded_value().expect("initial dossier").outline.known().is_none());
        store.snapshot()
    });
    let stages = [
        Stage::Added(source.clone()),
        Stage::Partial { page: source.clone(), words: Arc::from("compiler refused one declaration") },
    ];
    for (at, stage) in stages.into_iter().enumerate() {
        let active_reads = observed.count(&registry);
        let source_reads = observed.count(&source);
        let root_reads = observed.roots.load(Ordering::Relaxed);
        let (active_stamp, source_stamp) = rig.graph.store.read_with(rig.cx, |store, _| {
            (store.stamp(&active_key), store.stamp(&source_key))
        });
        observed.publication.store(at + 1, Ordering::Release);
        land(&mut rig, &release, stage.clone());
        assert_eq!(observed.roots.load(Ordering::Relaxed), root_reads + 1, "the actual root refresh follows publication");
        assert_eq!(observed.count(&registry), active_reads + 1, "the mounted registry dossier rereads once");
        assert_eq!(observed.count(&source), source_reads, "the separate hidden source address remains lazy");
        rig.graph.store.read_with(rig.cx, |store, _| {
            let now = store.snapshot();
            assert_eq!(before.key(), now.key(), "publication needs no invented root generation");
            assert_eq!(before.route(), now.route());
            assert_eq!(before.session().back, now.session().back);
            assert_eq!(before.session().forward, now.session().forward);
            assert_ne!(store.stamp(&active_key), active_stamp);
            assert_ne!(store.stamp(&source_key), source_stamp, "the Stage source address is also invalidated");
            assert!(!store.observation_revoked(&active_key));
            assert!(store.observation_revoked(&source_key));
            let current = store.package(&registry);
            assert!(current.loaded_value().expect("renewed dossier").record.known().is_some());
            assert!(current.loaded_value().expect("renewed dossier").outline.known().is_some());
        });
        let words = format!("Publication {} reached the open page.", at + 1);
        assert!(rig.said().iter().any(|text| text.contains(&words)), "fresh facts reach the mounted Reader without Back");
        // An explicit later ask renews the revoked source observation without
        // equating its identity with the registry discovery address.
        rig.graph.store.update(rig.cx, |store, cx| { store.ensure(source_key.clone(), cx); });
        rig.settle();
        assert_eq!(observed.count(&source), source_reads + 1);
        assert!(!rig.graph.store.read_with(rig.cx, |store, _| store.observation_revoked(&source_key)));
        let submitted = rig.graph.store.read_with(rig.cx, |store, _| store.stats().submitted);
        land(&mut rig, &release, stage);
        assert_eq!(rig.graph.store.read_with(rig.cx, |store, _| store.stats().submitted), submitted,
            "the identical terminal stage cannot invalidate or submit again");
        assert_eq!(observed.roots.load(Ordering::Relaxed), root_reads + 1);
        assert_eq!(observed.count(&registry), active_reads + 1);
        assert_eq!(observed.count(&source), source_reads + 1);
    }
    let unchanged = rig.graph.store.read_with(rig.cx, |store, _| {
        (store.stats().submitted, store.stamp(&active_key), store.stamp(&source_key))
    });
    let root_reads = observed.roots.load(Ordering::Relaxed);
    for stage in [Stage::Resolving, Stage::Indexing, Stage::Failed(Arc::from("fixture rejected the source"))] {
        land(&mut rig, &release, stage);
        rig.graph.store.read_with(rig.cx, |store, _| {
            assert_eq!((store.stats().submitted, store.stamp(&active_key), store.stamp(&source_key)), unchanged,
                "nonpublication stages cannot refresh observations");
            assert!(!store.observation_revoked(&active_key));
            assert!(!store.observation_revoked(&source_key));
        });
        assert_eq!(observed.roots.load(Ordering::Relaxed), root_reads);
    }
}
