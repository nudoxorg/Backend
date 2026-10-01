//! The launch snapshot's keeper (W-Open I2): seeds the pages the last launch
//! left, settles them against the first served root, and saves the route's
//! pages for the next launch, at rest and on quit.

use super::DataStore;
use crate::core::{Resource, VersionedRoot};
use crate::model::AppSnapshot;
use crate::model::pages::{PageKey, PageStore, SeedEntry};
use crate::runtime::snapshot::{Keep, SnapRoot, SnapshotFile, kept_keys};
use gpui::{AppContext as _, Context, Task};
use std::sync::Arc;
use std::time::Duration;

/// How long the pages rest before the launch snapshot is saved (I2).
const SAVE_IDLE: Duration = Duration::from_millis(1_500);

/// The route's pages, the root they are current at, and where they go.
pub(super) struct PendingSave {
    file: SnapshotFile,
    root: VersionedRoot,
    pages: Vec<SeedEntry>,
}

impl PendingSave {
    /// Writes them, and says how long it took.
    fn write(&self, when: &str) -> std::io::Result<usize> {
        let saving = std::time::Instant::now();
        let written = self.file.write(self.root, &self.pages)?;
        crate::runtime::trace::span("snapshot.write", saving, format_args!("{} pages, {written} bytes, {when}", self.pages.len()));
        Ok(written)
    }
}

/// Where the pages are saved, the root the seeded ones were read at, and the
/// pending save.
#[derive(Default)]
pub(super) struct SnapshotKeeper {
    /// The root the launch snapshot's pages were read at, until the first
    /// served root settles them (confirmed as they are, or revalidated).
    seed_root: Option<SnapRoot>,
    file: Option<SnapshotFile>,
    saving: Option<Task<()>>,
}

impl SnapshotKeeper {
    /// Seeds the launch snapshot's pages at `root` (the unserved root at
    /// launch) and remembers where to save them.
    pub(super) fn keep(&mut self, pages: &mut PageStore, root: VersionedRoot, keep: Keep) {
        if let Some(seed) = keep.seed {
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

    /// The first served root settles the seeded pages: at the root they were
    /// read at they are current as they are; otherwise their next fetch
    /// revalidates them quietly.
    pub(super) fn settle(&mut self, pages: &mut PageStore, root: VersionedRoot) {
        if root.is_unserved() {
            return;
        }
        let Some(seed) = self.seed_root.take() else {
            return;
        };
        if seed.serves(root) {
            // An alternate-release cache key carries a route claim about
            // another tree. The owner root alone cannot admit that claim:
            // its read worker must verify both manifests and the provider.
            let confirmed = pages.keys().into_iter()
                .filter(|key| !requires_origin_verification(key))
                .filter(|key| pages.confirm(key, root)).count();
            crate::runtime::trace::mark("snapshot.confirm", format_args!("{confirmed} pages at the served root"));
        } else {
            crate::runtime::trace::mark("snapshot.revalidate", "the owner serves a newer root");
        }
    }

    /// The route's pages as they are now, when all are current at a served
    /// root: what the next launch paints first.
    fn to_save(&self, pages: &PageStore, snapshot: &AppSnapshot) -> Option<PendingSave> {
        fn at<T>(resource: &Resource<T>, root: VersionedRoot) -> Option<Arc<T>> {
            (resource.is_loaded() && resource.value_root().is_some_and(|at| at.same_authority(root)))
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
            if pages.is_seeded(key) || pages.inflight(key).is_some() {
                return None;
            }
            match key {
                PageKey::Symbol(symbol) => at(&pages.symbol(symbol), root).map(|page| SeedEntry::Symbol(symbol.clone(), page)),
                PageKey::Source(symbol) => at(&pages.source(symbol), root).map(|view| SeedEntry::Source(symbol.clone(), view)),
                PageKey::Package(package) => at(&pages.package(package), root).map(|dossier| SeedEntry::Package(package.clone(), dossier)),
                PageKey::Orbit => at(&pages.orbit(), root).map(SeedEntry::Orbit),
                PageKey::Search(_) | PageKey::Health | PageKey::Browse(_) => None,
            }
        };
        let kept = kept_keys(snapshot.route()).iter().filter_map(current).collect::<Vec<_>>();
        (!kept.is_empty()).then_some(PendingSave { file, root, pages: kept })
    }

    /// Saves the launch snapshot now, on this thread (quit).
    ///
    /// # Errors
    /// The snapshot file's I/O error; nothing to save is `Ok(0)`.
    pub(super) fn save_now(&self, pages: &PageStore, snapshot: &AppSnapshot) -> std::io::Result<usize> {
        self.to_save(pages, snapshot).map_or(Ok(0), |save| save.write("on quit"))
    }

    /// Saves the launch snapshot once the pages have rested (a newer landing
    /// restarts the wait), encoding and writing off the UI thread.
    pub(super) fn save_at_rest(&mut self, cx: &mut Context<DataStore>) {
        if self.file.is_none() {
            return;
        }
        self.saving = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_IDLE).await;
            let Ok(Some(save)) = this.update(cx, |store, _| store.keeper.to_save(&store.pages, &store.snapshot)) else {
                return;
            };
            cx.background_spawn(async move {
                if let Err(error) = save.write("at rest") {
                    eprintln!("backend-desktop: save {}: {error}", save.file.path().display());
                }
            })
            .await;
        }));
    }
}

fn requires_origin_verification(key: &PageKey) -> bool {
    match key {
        PageKey::Symbol(symbol) | PageKey::Source(symbol) => symbol.release_origin().is_some(),
        PageKey::Package(package) => package.release_origin().is_some(),
        PageKey::Orbit | PageKey::Search(_) | PageKey::Health | PageKey::Browse(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::pages::{Known, OrbitModel, PageValue};

    #[test]
    fn saved_release_claims_remain_seeded_until_the_worker_verifies_them() {
        use crate::model::pages::PackageRef;
        let dir = std::env::temp_dir().join(format!("nx-keeper-origin-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch");
        let file = SnapshotFile::in_data(&dir);
        let root = VersionedRoot::synthetic(backend_library::view_state_root(&[("keeper".into(), "origin".into())]), 1);
        let pinned = PackageRef::parse("pkg:cargo/serde@1.0.0").expect("pin");
        let release = PackageRef::parse("pkg:cargo/serde@0.9.0").expect("release").with_release_origin(&pinned);
        let entry = SeedEntry::Package(release.clone(), Arc::new(crate::shell::tests::dossier()));
        let key = entry.key();
        assert!(file.read(&[]).is_none(), "prepare the build identity on the read path");
        file.write(root, &[entry]).expect("snapshot");
        let seed = file.read(std::slice::from_ref(&key)).expect("seed");
        assert!(seed.root.serves(root), "this regression must exercise the same-build confirmation path");
        let mut keeper = SnapshotKeeper::default();
        let mut pages = PageStore::default();
        keeper.keep(&mut pages, VersionedRoot::unserved(), Keep { file, seed: Some(seed) });
        keeper.settle(&mut pages, root);
        assert!(pages.is_seeded(&key), "matching root does not verify the saved original release tree");
        assert!(pages.begin(&key, root).is_some(), "a real worker read is still required");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_cancelled_stage_is_never_saved_as_a_complete_launch_page() {
        let root = VersionedRoot::synthetic(backend_library::view_state_root(&[("keeper".into(), "stage".into())]), 1);
        let snapshot = AppSnapshot::empty(root);
        let keeper = SnapshotKeeper { file: Some(SnapshotFile::in_data(std::path::Path::new("/tmp"))), ..SnapshotKeeper::default() };
        let model = OrbitModel {
            indexed: Known::Known(Arc::from([])),
            projects: Known::Known(Arc::from([])),
            explore: Known::Known(Arc::from([])),
            tree: Known::Known(Arc::from([])),
        };
        let mut pages = PageStore::default();
        let key = PageKey::Orbit;
        let first = pages.begin(&key, root).expect("first read");
        assert_eq!(pages.stage(&key, first, PageValue::Orbit(model.clone())), crate::model::pages::Landing::Applied);
        assert!(keeper.to_save(&pages, &snapshot).is_none(), "an in-flight stage is not complete");
        assert_eq!(pages.cancel(&key), Some(first));
        assert!(keeper.to_save(&pages, &snapshot).is_none(), "cancellation must not erase partial provenance");
        let second = pages.begin(&key, root).expect("second read");
        assert_eq!(pages.land(&key, second, Ok(PageValue::Orbit(model))), crate::model::pages::Landing::Applied);
        let saved = keeper.to_save(&pages, &snapshot).expect("the completed page is savable");
        assert!(matches!(saved.pages.as_slice(), [SeedEntry::Orbit(_)]));
    }
}
