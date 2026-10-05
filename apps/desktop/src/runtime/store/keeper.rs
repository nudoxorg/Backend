//! The launch snapshot's keeper (W-Open I2): seeds the pages the last launch
//! left, retains them until fresh reads, and saves the route's
//! pages for the next launch, at rest and on quit.

use super::DataStore;
use crate::core::{Resource, VersionedRoot};
use crate::model::AppSnapshot;
use crate::model::pages::{PageKey, PageStore, SeedEntry};
use crate::runtime::snapshot::{DisplayCapture, Keep, SnapRoot, SnapshotFile, kept_keys};
use crate::runtime::snapshot::RetainedDisplay;
use gpui::{AppContext as _, Context, Task};
use std::sync::{Arc, Mutex, PoisonError};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// How long the pages rest before the launch snapshot is saved (I2).
const SAVE_IDLE: Duration = Duration::from_millis(1_500);

/// The route's pages, the root they are current at, and where they go.
pub(crate) struct PendingSave {
    sequence: u64,
    committed: Arc<Mutex<u64>>,
    owner: Option<super::OwnerAttachment>,
    file: SnapshotFile,
    root: VersionedRoot,
    pages: Vec<SeedEntry>,
    capture: Option<DisplayCapture>,
    prepared: Option<Arc<RetainedDisplay>>,
}

impl PendingSave {
    pub(crate) fn captured_root(&self) -> VersionedRoot { self.root }

    /// Writes them, and says how long it took.
    pub(crate) fn write(&self, when: &str) -> std::io::Result<usize> {
        let saving = std::time::Instant::now();
        let display = self.capture.as_ref().and_then(DisplayCapture::prepare).map(Arc::new)
            .or_else(|| self.prepared.clone());
        let displays = display.into_iter().collect::<Vec<_>>();
        // Prepare outside the lock; publish in admission order. A cancelled
        // older at-rest task can finish its I/O, but cannot overwrite a newer
        // successful close checkpoint once it eventually reaches this fence.
        let mut committed = self.committed.lock().unwrap_or_else(PoisonError::into_inner);
        if self.sequence <= *committed || self.owner.as_ref().is_some_and(|owner| !owner.is_current()) { return Ok(0); }
        let written = self.file.write_displays(self.root, &self.pages, &displays)?;
        *committed = self.sequence;
        crate::runtime::trace::span(
            "snapshot.write",
            saving,
            format_args!("{} pages, {written} bytes, {when}", self.pages.len()),
        );
        Ok(written)
    }
}

/// Where the pages are saved, the root the seeded ones were read at, and the
/// pending save.
#[derive(Default)]
pub(super) struct SnapshotKeeper {
    next_write: AtomicU64,
    committed: Arc<Mutex<u64>>,
    /// The root the launch snapshot's pages were read at, until the first
    /// served root schedules their fresh revalidation.
    seed_root: Option<SnapRoot>,
    file: Option<SnapshotFile>,
    saving: Option<Task<()>>,
    retained: Vec<Arc<RetainedDisplay>>,
    prepared: Option<Arc<RetainedDisplay>>,
}

impl SnapshotKeeper {
    /// Seeds the launch snapshot's pages at `root` (the unserved root at
    /// launch) and remembers where to save them.
    pub(super) fn keep(&mut self, pages: &mut PageStore, root: VersionedRoot, keep: Keep) {
        if let Some(seed) = keep.seed {
            if root.is_unserved() { self.retained = seed.displays; }
            let mut seeded = 0_usize;
            for entry in seed.pages {
                let key = crate::runtime::trace::enabled().then(|| entry.key());
                if pages.seed(entry, root) {
                    seeded += 1;
                    if let Some(key) = key {
                        crate::runtime::trace::mark("snapshot.seed", format_args!("{key:?}"));
                    }
                }
            }
            if seeded > 0 {
                self.seed_root = Some(seed.root);
            }
        }
        self.file = Some(keep.file);
    }

    /// The first served root schedules quiet fresh revalidation. Root/build
    /// equality alone never grants a cached page present read authority.
    pub(super) fn settle(&mut self, _pages: &mut PageStore, root: VersionedRoot) {
        if root.is_unserved() {
            return;
        }
        let Some(seed) = self.seed_root.take() else {
            return;
        };
        // Root/build equality is diagnostic only. Cached payload hashes do
        // not prove a present read; every family gets a fresh worker landing.
        crate::runtime::trace::mark("snapshot.revalidate",
            format_args!("retained reading; same observed root/build: {}", seed.serves(root)));
    }

    pub(super) fn retained(&self, route: &crate::navigation::Route) -> Option<Arc<RetainedDisplay>> {
        self.prepared.iter().chain(self.retained.iter())
            .find(|display| display.source_matches_route(route)).cloned()
    }

    /// The route's pages as they are now, when all are current at a served
    /// root: what the next launch paints first.
    fn to_save(&self, pages: &PageStore, snapshot: &AppSnapshot, owner_serving: bool) -> Option<PendingSave> {
        // Gate loss may precede the store's revocation events. Such bytes
        // must not be saved as current during that intervening UI turn.
        if !owner_serving {
            return None;
        }
        fn at<T>(resource: &Resource<T>, root: VersionedRoot) -> Option<Arc<T>> {
            (resource.is_loaded()
                && resource
                    .value_root()
                    .is_some_and(|at| at.same_authority(root)))
            .then(|| resource.loaded_arc().cloned())
            .flatten()
        }
        let file = self.file.clone()?;
        let root = snapshot.key();
        if root.is_unserved() {
            return None;
        }
        let current = |key: &PageKey| -> Option<SeedEntry> {
            // A staged page is useful on screen, but its read is still in
            // flight. Never replay that partial model as a complete page on
            // the next launch.
            if pages.is_seeded(key) || pages.is_owner_read_revoked(key) || pages.inflight(key).is_some() {
                return None;
            }
            match key {
                PageKey::Symbol(symbol) => at(&pages.symbol(symbol), root)
                    .map(|page| SeedEntry::Symbol(symbol.clone(), page)),
                PageKey::Source(symbol) => at(&pages.source(symbol), root)
                    .map(|view| SeedEntry::Source(symbol.clone(), view)),
                PageKey::Package(package) => at(&pages.package(package), root)
                    .map(|dossier| SeedEntry::Package(package.clone(), dossier)),
                PageKey::Orbit => at(&pages.orbit(), root).map(SeedEntry::Orbit),
                PageKey::CargoSource(_) | PageKey::Search(_) | PageKey::Health | PageKey::Browse(_) => None,
            }
        };
        let kept = kept_keys(snapshot.route())
            .iter()
            .filter_map(current)
            .collect::<Vec<_>>();
        let capture = DisplayCapture::select(pages, snapshot.route(), root);
        let prepared = self.prepared.as_ref().filter(|display| display.source_matches_route(snapshot.route())
            && display.observation().cursor.as_slice() == root.revision().encode_control().as_ref()
            && display.observation().producer_epoch == root.producer_epoch()).cloned();
        (!kept.is_empty() || capture.is_some() || prepared.is_some()).then_some(PendingSave {
            sequence: self.next_write.fetch_add(1, Ordering::Relaxed) + 1,
            committed: self.committed.clone(), owner: None,
            file, root, pages: kept, capture, prepared,
        })
    }

    pub(super) fn checkpoint(&self, pages: &PageStore, snapshot: &AppSnapshot, owner: Option<super::OwnerAttachment>) -> Option<PendingSave> {
        let mut save = self.to_save(pages, snapshot, owner.is_some())?;
        save.owner = owner;
        Some(save)
    }

    /// Saves the launch snapshot now, on this thread (quit).
    ///
    /// # Errors
    /// The snapshot file's I/O error; nothing to save is `Ok(0)`.
    pub(super) fn save_now(
        &self,
        pages: &PageStore,
        snapshot: &AppSnapshot,
        owner_serving: bool,
    ) -> std::io::Result<usize> {
        self.to_save(pages, snapshot, owner_serving)
            .map_or(Ok(0), |mut save| {
                // New projection traversal is never performed on the UI
                // thread, including quit. Reuse only worker-prepared bytes.
                if save.capture.is_some() && save.prepared.is_none() {
                    // The requested route's new display has not been prepared.
                    // Keep the previous exact-scope file, rather than replacing
                    // it with only unrelated legacy support sections on quit.
                    return Ok(0);
                }
                save.capture = None;
                if save.pages.is_empty() && save.prepared.is_none() { return Ok(0); }
                save.write("on quit")
            })
    }

    /// Saves the launch snapshot once the pages have rested (a newer landing
    /// restarts the wait), encoding and writing off the UI thread.
    pub(super) fn save_at_rest(&mut self, cx: &mut Context<DataStore>) {
        if self.file.is_none() {
            return;
        }
        self.saving = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_IDLE).await;
            let Ok(Some(save)) = this.update(cx, |store, _| {
                store.keeper.checkpoint(&store.pages, &store.snapshot, store.current_owner_attachment())
            }) else {
                return;
            };
            let prepared = cx.background_spawn(async move {
                let display = save.capture.as_ref().and_then(DisplayCapture::prepare).map(Arc::new)
                    .or_else(|| save.prepared.clone());
                let mut save = save;
                save.capture = None;
                save.prepared = display.clone();
                if let Err(error) = save.write("at rest") {
                    eprintln!("backend-desktop: save {}: {error}", save.file.path().display());
                    return None;
                }
                display
            }).await;
            if let Some(prepared) = prepared {
                let _ = this.update(cx, |store, _| {
                    // A late worker cannot select a different destination or
                    // attach its old observation to newer current bytes.
                    let root = store.snapshot.key();
                    if store.owner_serving() && prepared.source_matches_route(store.snapshot.route())
                        && prepared.observation().cursor.as_slice() == root.revision().encode_control().as_ref()
                        && prepared.observation().producer_epoch == root.producer_epoch() {
                        store.keeper.prepared = Some(prepared);
                    }
                });
            }
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::pages::{Known, OrbitModel, PageValue};

    #[test]
    fn a_delayed_old_at_rest_packet_cannot_overwrite_a_new_close_checkpoint() {
        let directory = crate::host::scratch_base().join(format!("nx-close-fence-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("clock").as_nanos()));
        crate::host::private_dir(&directory).expect("fixture");
        let file = SnapshotFile::in_data(&directory);
        let mut keeper = SnapshotKeeper::default();
        keeper.file = Some(file.clone());
        let view = backend_library::view_state_root(&[("fence".into(), "close".into())]);
        let first = VersionedRoot::synthetic(view.clone(), 1);
        let last = VersionedRoot::synthetic(view, 2);
        let model = Arc::new(OrbitModel { indexed: Known::Known(Arc::from([])), projects: Known::Known(Arc::from([])),
            explore: Known::Known(Arc::from([])), tree: Known::Known(Arc::from([])) });
        let packet = |root, keeper: &SnapshotKeeper| {
            let mut pages = PageStore::default();
            assert!(pages.seed(SeedEntry::Orbit(model.clone()), root));
            keeper.to_save(&pages, &AppSnapshot::empty(root), true).expect("served packet")
        };
        let older = packet(first, &keeper);
        let newer = packet(last, &keeper);
        let (release, wait) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            wait.recv_timeout(Duration::from_secs(5)).expect("release delayed worker");
            older.write("delayed at rest")
        });
        assert!(newer.write("close checkpoint").expect("new final packet") > 0);
        let final_bytes = std::fs::read(file.path()).expect("final snapshot");
        release.send(()).expect("release old worker");
        assert_eq!(worker.join().expect("old worker").expect("obsolete packet skipped"), 0);
        assert_eq!(std::fs::read(file.path()).expect("still final"), final_bytes);
        assert!(file.read(&[PageKey::Orbit]).expect("read final snapshot").root.serves(last));
        std::fs::remove_dir_all(directory).expect("fixture removed");
    }

    #[test]
    fn quit_without_worker_prepared_destination_preserves_the_previous_private_file() {
        let root = VersionedRoot::synthetic(backend_library::view_state_root(&[("keeper".into(), "quit-preparation".into())]), 1);
        let dir = std::env::temp_dir().join(format!("nx-keeper-quit-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("time").as_nanos()));
        crate::host::private_dir(&dir).expect("private scratch");
        let file = SnapshotFile::in_data(&dir);
        let model = OrbitModel { indexed: Known::Known(Arc::from([])), projects: Known::Known(Arc::from([])),
            explore: Known::Known(Arc::from([])), tree: Known::Known(Arc::from([])) };
        file.write(root, &[SeedEntry::Orbit(Arc::new(model.clone()))]).expect("previous usable snapshot");
        let prior = std::fs::read(file.path()).expect("prior bytes");
        let mut pages = PageStore::default();
        let generation = pages
            .begin(&PageKey::Orbit, root)
            .expect("page generation admission")
            .expect("fresh read");
        pages.land(&PageKey::Orbit, generation, Ok(PageValue::Orbit(model)));
        let mut snapshot = AppSnapshot::empty(root);
        let mut session = snapshot.session().clone(); session.route = crate::navigation::Route::World;
        snapshot = snapshot.with_session(session);
        let keeper = SnapshotKeeper { file: Some(file.clone()), ..SnapshotKeeper::default() };
        assert!(keeper.to_save(&pages, &snapshot, true).expect("pending capture").capture.is_some());
        assert_eq!(keeper.save_now(&pages, &snapshot, true).expect("quit"), 0);
        assert_eq!(std::fs::read(file.path()).expect("preserved cache"), prior);
        assert!(file.read(&[PageKey::Orbit]).is_some(), "the preserved file remains useful at its exact scope");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn revoked_predecessors_cannot_be_saved_as_current_launch_pages() {
        let root = VersionedRoot::synthetic(backend_library::view_state_root(&[("keeper".into(), "owner-revoked".into())]), 1);
        let package = crate::shell::tests::dossier().package;
        let key = PageKey::Package(package.clone());
        let mut snapshot = AppSnapshot::empty(root);
        let mut session = snapshot.session().clone();
        session.route = crate::shell::kit::package_route(&package).expect("package route");
        snapshot = snapshot.with_session(session);
        let keeper = SnapshotKeeper {
            file: Some(SnapshotFile::in_data(std::path::Path::new("/unused/fixture/owner-revocation"))),
            ..SnapshotKeeper::default()
        };
        let mut pages = PageStore::default();
        let first = pages
            .begin(&key, root)
            .expect("page generation admission")
            .expect("first read");
        assert_eq!(
            pages.land(
                &key,
                first,
                Ok(PageValue::Package(crate::shell::tests::dossier()))
            ),
            crate::model::pages::Landing::Applied
        );
        assert!(keeper.to_save(&pages, &snapshot, true).is_some());
        assert!(keeper.to_save(&pages, &snapshot, false).is_none(), "gate loss is immediate even before slot revocation");
        assert!(pages.revoke_owner_read(&key));
        assert!(pages.package(&package).loaded_value().is_some(), "revocation retains predecessor bytes");
        assert!(keeper.to_save(&pages, &snapshot, true).is_none());
        let next = pages
            .begin(&key, root)
            .expect("page generation admission")
            .expect("same-root renewal");
        let _ = pages.cancel(&key);
        assert_eq!(
            pages.land(
                &key,
                next,
                Ok(PageValue::Package(crate::shell::tests::dossier()))
            ),
            crate::model::pages::Landing::Superseded
        );
        assert!(
            keeper.to_save(&pages, &snapshot, true).is_none(),
            "cancellation cannot save the predecessor as current"
        );
        let next = pages
            .begin(&key, root)
            .expect("page generation admission")
            .expect("fresh renewal");
        assert_eq!(
            pages.land(
                &key,
                next,
                Ok(PageValue::Package(crate::shell::tests::dossier()))
            ),
            crate::model::pages::Landing::Applied
        );
        assert!(keeper.to_save(&pages, &snapshot, true).is_some());
    }

    #[test]
    fn saved_release_claims_remain_seeded_until_the_worker_verifies_them() {
        use crate::model::pages::PackageRef;
        let dir = std::env::temp_dir().join(format!("nx-keeper-origin-{}", std::process::id()));
        crate::host::private_dir(&dir).expect("private scratch");
        let file = SnapshotFile::in_data(&dir);
        let root = VersionedRoot::synthetic(
            backend_library::view_state_root(&[("keeper".into(), "origin".into())]),
            1,
        );
        let pinned = PackageRef::parse("pkg:cargo/serde@1.0.0").expect("pin");
        let release = PackageRef::parse("pkg:cargo/serde@0.9.0")
            .expect("release")
            .with_release_origin(&pinned);
        let entry = SeedEntry::Package(release.clone(), Arc::new(crate::shell::tests::dossier()));
        let key = entry.key();
        assert!(
            file.read(&[]).is_none(),
            "prepare the build identity on the read path"
        );
        file.write(root, &[entry]).expect("snapshot");
        let seed = file.read(std::slice::from_ref(&key)).expect("seed");
        let mut keeper = SnapshotKeeper::default();
        let mut pages = PageStore::default();
        keeper.keep(
            &mut pages,
            VersionedRoot::unserved(),
            Keep {
                file,
                seed: Some(seed),
            },
        );
        keeper.settle(&mut pages, root);
        assert!(
            pages.is_seeded(&key),
            "matching root does not verify the saved original release tree"
        );
        assert!(
            pages
                .begin(&key, root)
                .expect("page generation admission")
                .is_some(),
            "a real worker read is still required"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_admitted_index_root_cannot_confirm_cached_docs_or_local_source_observations() {
        let root = VersionedRoot::synthetic(
            backend_library::view_state_root(&[("keeper".into(), "local-source".into())]),
            1,
        );
        let symbol = crate::shell::tests::symbol("LocalSource");
        let mut pages = PageStore::default();
        assert!(pages.seed(
            SeedEntry::Symbol(
                symbol.clone(),
                Arc::new(crate::shell::tests::page("LocalSource"))
            ),
            VersionedRoot::unserved()
        ));
        use crate::runtime::reads::{OutlineCache, PageReader as _, ReadContext, ReadRequest};
        let cancel = crate::runtime::actor::CancellationToken::new();
        let outlines = OutlineCache::default();
        let context = ReadContext {
            worker: 0,
            cancel: &cancel,
            outlines: &outlines,
            progress: None,
        };
        let PageValue::Source(source) = crate::shell::tests::Fixture
            .read(&ReadRequest::Source(symbol.clone()), &context)
            .expect("source page")
        else {
            panic!("source page family")
        };
        assert!(pages.seed(
            SeedEntry::Source(symbol.clone(), Arc::new(source)),
            VersionedRoot::unserved()
        ));
        let mut keeper = SnapshotKeeper::default();
        keeper.settle(&mut pages, root);
        assert!(pages.is_seeded(&PageKey::Symbol(symbol.clone())));
        assert!(
            pages
                .begin(&PageKey::Symbol(symbol.clone()), root)
                .expect("page generation admission")
                .is_some(),
            "docs require a fresh owner read too"
        );
        let source_key = PageKey::Source(symbol);
        assert!(
            pages.is_seeded(&source_key),
            "the index root cannot confirm a mutable local file"
        );
        assert!(
            pages
                .begin(&source_key, root)
                .expect("page generation admission")
                .is_some(),
            "restart renews the source observation on a worker"
        );
    }

    #[test]
    fn a_cancelled_stage_is_never_saved_as_a_complete_launch_page() {
        let root = VersionedRoot::synthetic(
            backend_library::view_state_root(&[("keeper".into(), "stage".into())]),
            1,
        );
        let snapshot = AppSnapshot::empty(root);
        let keeper = SnapshotKeeper {
            file: Some(SnapshotFile::in_data(std::path::Path::new("/tmp"))),
            ..SnapshotKeeper::default()
        };
        let model = OrbitModel {
            indexed: Known::Known(Arc::from([])),
            projects: Known::Known(Arc::from([])),
            explore: Known::Known(Arc::from([])),
            tree: Known::Known(Arc::from([])),
        };
        let mut pages = PageStore::default();
        let key = PageKey::Orbit;
        let first = pages
            .begin(&key, root)
            .expect("page generation admission")
            .expect("first read");
        assert_eq!(
            pages.stage(&key, first, PageValue::Orbit(model.clone())),
            crate::model::pages::Landing::Applied
        );
        assert!(
            keeper.to_save(&pages, &snapshot, true).is_none(),
            "an in-flight stage is not complete"
        );
        assert_eq!(pages.cancel(&key), Some(first));
        assert!(
            keeper.to_save(&pages, &snapshot, true).is_none(),
            "cancellation must not erase partial provenance"
        );
        let second = pages
            .begin(&key, root)
            .expect("page generation admission")
            .expect("second read");
        assert_eq!(
            pages.land(&key, second, Ok(PageValue::Orbit(model))),
            crate::model::pages::Landing::Applied
        );
        let saved = keeper
            .to_save(&pages, &snapshot, true)
            .expect("the completed page is savable");
        assert!(matches!(saved.pages.as_slice(), [SeedEntry::Orbit(_)]));
    }
}
