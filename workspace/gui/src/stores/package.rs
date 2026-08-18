//! `PackageStore` — what the corpus actually contains (GUI-LOCAL-PLAN §L10).
//!
//! # Why a store and not a counter on the status bar
//!
//! The status bar is a *view*: §13.3 gives it pre-formatted strings and forbids
//! it from computing anything in `render`. The count of loaded packages is real
//! state arriving on a channel over tens of seconds, so it belongs in a store
//! that owns the drain (§7.4) and emits when it changes. The status bar then
//! renders a `SharedString` it was handed, which is the whole point of the
//! split.
//!
//! # Pending rows are seeded, not inferred
//!
//! The engine reports packages as they *finish*. It has no "started" event and
//! no completion event, so a store that only listened to the channel could not
//! tell "still lowering axum" from "corpus is empty" — and would show
//! `no packages` for the thirty-odd seconds rust-analyzer takes on a real
//! crate. That reads as a broken app.
//!
//! The fix is that the caller already knows what it asked for. [`PackageStore`]
//! is seeded at construction with the requested package names as
//! [`PackageStatus::Pending`] rows, so the very first frame says
//! `loading axum…` truthfully. Events then resolve those rows in place. The
//! store never invents a package it was not told about — an event for an
//! unseeded name appends a new row rather than being dropped, because the
//! fixture corpus loads packages nobody requested by name.

use gpui::{Context, EventEmitter, SharedString, Task};
use nudox_engine::{
    PackageLoadEvent, SymbolKey,
    wire::{EcosystemId, Gen, PackageLineageId, PackageName, VersionEvent, VersionList},
};

use crate::bridge::drain::drain;
use crate::stores::events::PackagesChanged;

/// Read-only package data consumed by package-oriented GUI views.
///
/// The GUI projects these rows into its own render models; it does not access
/// engine IR or fabricate package names and paths in a view.
pub trait PackageAccess: EventEmitter<PackagesChanged> + 'static {
    /// The current package snapshot, in request-then-arrival order.
    fn rows(&self) -> Vec<PackageRow>;

    /// Every loaded generation of a package, newest first.
    fn versions(&self, package: &PackageLineageId) -> VersionList;

    /// Switch the corpus to a loaded generation.
    fn select_version(
        &self,
        package: PackageLineageId,
        version: &str,
        generation: Gen,
    ) -> flume::Receiver<VersionEvent>;

    /// Preformatted corpus summary for compact package surfaces.
    fn summary_label(&self) -> SharedString;
}

// ---------------------------------------------------------------------------
// Engine capability trait
// ---------------------------------------------------------------------------

/// The one engine capability this store needs.
///
/// Mirrors `SymbolEngine`/`SearchEngine`: a trait rather than a concrete
/// `EngineHandle` so the store is testable against a double without a Tokio
/// runtime or a producer.
pub trait PackageEngine: 'static {
    /// Subscribe to package load results.
    ///
    /// The engine guarantees this replays packages that loaded *before* the
    /// call, so a late subscriber sees a complete picture — the store does not
    /// have to race engine startup.
    fn packages(&self) -> flume::Receiver<PackageLoadEvent>;

    /// Every loaded generation of `package`, newest first.
    ///
    /// Synchronous — the version list is already resident in memory; see
    /// `nudox_engine::versions` for the locking rationale.  An unloaded
    /// package returns an empty `VersionList`.
    fn versions(&self, package: &PackageLineageId) -> VersionList;

    /// Switch which generation of `package` the corpus serves.
    ///
    /// Returns a one-shot receiver that fires `VersionEvent::Switched` when the
    /// corpus write has landed, or `VersionEvent::NotLoaded` when the requested
    /// version is not held.
    fn select_version(
        &self,
        package: PackageLineageId,
        version: &str,
        generation: Gen,
    ) -> flume::Receiver<VersionEvent>;
}

/// The real engine satisfies the capability directly — a forwarding impl with
/// no conversion, for the same reason as `SymbolEngine`: the moment a
/// conversion appears here, the two event types have drifted.
impl PackageEngine for nudox_engine::EngineHandle {
    fn packages(&self) -> flume::Receiver<PackageLoadEvent> {
        nudox_engine::EngineHandle::packages(self)
    }

    fn versions(&self, package: &PackageLineageId) -> VersionList {
        nudox_engine::EngineHandle::versions(self, package)
    }

    fn select_version(
        &self,
        package: PackageLineageId,
        version: &str,
        generation: Gen,
    ) -> flume::Receiver<VersionEvent> {
        nudox_engine::EngineHandle::select_version(self, package, version, generation)
    }
}

// ---------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------

/// Where one package is in its lifecycle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PackageStatus {
    /// Requested, not yet reported. Seeded by the caller.
    Pending,
    /// Lowered and in the corpus.
    Ready {
        /// How many symbols it contributed.
        symbols: u64,
    },
    /// The producer failed. The corpus keeps running (LR-10).
    Failed {
        /// Operator-facing reason.
        message: SharedString,
    },
}

/// One package known to this session.
#[derive(Clone, Debug)]
pub struct PackageRow {
    pub name: SharedString,
    pub status: PackageStatus,
    /// Manifest provenance fields; `None` means unknown, not empty.
    pub metadata: nudox_engine::PackageMetadata,
    /// The generation reported by the latest successful load event.
    pub current_version: Option<SharedString>,
    /// The lineage identifier for this package (ecosystem + name).
    ///
    /// Stored here so the project panel can call `engine.versions(lineage)` at
    /// display time without reconstructing the key from strings.  `None` for
    /// rows that were seeded by the caller but have not yet received a
    /// `PackageLoadEvent::Loaded` — a pending row has no ecosystem yet.
    pub lineage: Option<PackageLineageId>,
    /// The package's root symbol — its crate / top-level module.
    ///
    /// `None` until the package reports `Ready`. A pending row has nothing to
    /// navigate to yet, which is precisely why activating one must be a no-op
    /// rather than an error.
    pub root: Option<SymbolKey>,
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

/// Tracks every package the session has asked for or been told about.
pub struct PackageStore<E: PackageEngine> {
    engine: E,
    rows: Vec<PackageRow>,
    /// Owning drain task — dropped with the store, which cancels it (LD-18).
    _drain: Option<Task<()>>,
}

impl<E: PackageEngine> PackageStore<E> {
    /// A store seeded with the names the caller requested.
    ///
    /// Pass an empty slice for the fixture corpus, which loads faster than the
    /// first frame and therefore never needs a pending state.
    pub fn new(engine: E, requested: &[String], cx: &mut Context<Self>) -> Self {
        let rows = requested
            .iter()
            .map(|name| PackageRow {
                name: SharedString::from(name.clone()),
                status: PackageStatus::Pending,
                metadata: nudox_engine::PackageMetadata::default(),
                current_version: None,
                lineage: None, // unknown until the Loaded event arrives
                root: None,
            })
            .collect();

        let mut store = Self {
            engine,
            rows,
            _drain: None,
        };
        store.subscribe(cx);
        store
    }

    /// Start draining engine events into `rows`.
    fn subscribe(&mut self, cx: &mut Context<Self>) {
        let rx = self.engine.packages();
        self._drain = Some(drain(cx, rx, |store: &mut Self, event, cx| {
            store.apply(event);
            cx.emit(PackagesChanged);
        }));
    }

    /// Fold one engine event into the row set.
    ///
    /// Resolution is by name: a `Pending` row seeded by the caller becomes
    /// `Ready`/`Failed` in place, keeping its position so the list does not
    /// reorder under the user. An unrecognised name appends.
    fn apply(&mut self, event: PackageLoadEvent) {
        let (name, status, lineage, current_version, root, metadata) = match event {
            PackageLoadEvent::Loaded {
                name,
                ecosystem,
                version,
                symbol_count,
                root,
                metadata,
            } => {
                let lid =
                    PackageLineageId::new(EcosystemId::new(&*ecosystem), PackageName::new(&*name));
                (
                    name,
                    PackageStatus::Ready {
                        symbols: symbol_count,
                    },
                    Some(lid),
                    version.map(SharedString::from),
                    root,
                    metadata,
                )
            }
            PackageLoadEvent::LoadFailed { name, error, .. } => (
                name,
                PackageStatus::Failed {
                    message: SharedString::from(error.to_string()),
                },
                None,
                None,
                None,
                nudox_engine::PackageMetadata::default(),
            ),
            // `PackageLoadEvent` is `#[non_exhaustive]`; a variant added later
            // is not a reason to lose the rows we already have.
            _ => return,
        };

        let name = SharedString::from(name.to_string());
        match self.rows.iter_mut().find(|r| r.name == name) {
            Some(row) => {
                row.status = status;
                // A later event must not clear a lineage or root we already
                // know: a failure after a successful load should not silently
                // make the package unnavigable.
                if lineage.is_some() {
                    row.lineage = lineage;
                }
                if current_version.is_some() {
                    row.current_version = current_version;
                }
                if root.is_some() {
                    row.root = root;
                }
                row.metadata = metadata;
            }
            None => self.rows.push(PackageRow {
                name,
                status,
                metadata,
                lineage,
                current_version,
                root,
            }),
        }
    }

    /// Every known package, in request-then-arrival order.
    pub fn rows(&self) -> &[PackageRow] {
        &self.rows
    }

    /// Every loaded generation of `package`, newest first.
    ///
    /// Delegates to the engine synchronously.  Returns an empty list when the
    /// package has not finished loading yet, or was never requested.
    pub fn versions(&self, package: &PackageLineageId) -> VersionList {
        self.engine.versions(package)
    }

    /// Switch which generation of `package` the corpus serves.
    ///
    /// Returns a one-shot receiver; fire-and-observe with `drain`.
    pub fn select_version(
        &self,
        package: PackageLineageId,
        version: &str,
        generation: Gen,
    ) -> flume::Receiver<VersionEvent> {
        self.engine.select_version(package, version, generation)
    }

    /// How many packages are in the corpus and queryable.
    pub fn ready_count(&self) -> usize {
        self.rows
            .iter()
            .filter(|r| matches!(r.status, PackageStatus::Ready { .. }))
            .count()
    }

    /// How many failed to load.
    pub fn failed_count(&self) -> usize {
        self.rows
            .iter()
            .filter(|r| matches!(r.status, PackageStatus::Failed { .. }))
            .count()
    }

    /// The status-bar string (§13.3 — formatted here, once, never in `render`).
    ///
    /// Failures are appended rather than replacing the count, because "2
    /// packages" while a third silently failed is the kind of half-truth that
    /// makes a missing symbol look like a search bug.
    pub fn summary_label(&self) -> SharedString {
        let ready = self.ready_count();
        let failed = self.failed_count();
        let pending: Vec<&PackageRow> = self
            .rows
            .iter()
            .filter(|r| r.status == PackageStatus::Pending)
            .collect();

        // Nothing has resolved yet: name what we are waiting for. Naming it is
        // the difference between "the app is working" and "the app is broken".
        if ready == 0 && failed == 0 {
            return match pending.len() {
                0 => SharedString::from("no packages"),
                1 => SharedString::from(format!("loading {}…", pending[0].name)),
                n => SharedString::from(format!("loading {n} packages…")),
            };
        }

        let mut label = match ready {
            0 => String::new(),
            1 => "1 package".to_owned(),
            n => format!("{n} packages"),
        };
        if !pending.is_empty() {
            if !label.is_empty() {
                label.push_str(" · ");
            }
            label.push_str(&format!("{} loading", pending.len()));
        }
        if failed > 0 {
            if !label.is_empty() {
                label.push_str(" · ");
            }
            label.push_str(&format!("{failed} failed"));
        }
        SharedString::from(label)
    }
}

impl<E: PackageEngine> EventEmitter<PackagesChanged> for PackageStore<E> {}

impl<E: PackageEngine> PackageAccess for PackageStore<E> {
    fn rows(&self) -> Vec<PackageRow> {
        self.rows().to_vec()
    }

    fn versions(&self, package: &PackageLineageId) -> VersionList {
        self.versions(package)
    }

    fn select_version(
        &self,
        package: PackageLineageId,
        version: &str,
        generation: Gen,
    ) -> flume::Receiver<VersionEvent> {
        self.select_version(package, version, generation)
    }

    fn summary_label(&self) -> SharedString {
        self.summary_label()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use nudox_engine::wire::SharedStr;

    /// A double that hands out a channel the test controls.
    struct StubEngine {
        rx: flume::Receiver<PackageLoadEvent>,
    }

    impl PackageEngine for StubEngine {
        fn packages(&self) -> flume::Receiver<PackageLoadEvent> {
            self.rx.clone()
        }

        fn versions(&self, _package: &PackageLineageId) -> VersionList {
            VersionList {
                package: _package.clone(),
                versions: std::sync::Arc::from(vec![]),
            }
        }

        fn select_version(
            &self,
            package: PackageLineageId,
            version: &str,
            generation: Gen,
        ) -> flume::Receiver<VersionEvent> {
            let (tx, rx) = flume::bounded(1);
            let version = nudox_engine::wire::SharedStr::from(version);
            let _ = tx.try_send(VersionEvent::NotLoaded {
                generation,
                package,
                version,
            });
            rx
        }
    }

    fn store_with(
        requested: &[&str],
    ) -> (flume::Sender<PackageLoadEvent>, PackageStore<StubEngine>) {
        let (tx, rx) = flume::bounded(32);
        let names: Vec<String> = requested.iter().map(|s| (*s).to_owned()).collect();
        // Construct without a Context: `subscribe` is the only part that needs
        // one, and these tests exercise `apply`/`summary_label` directly.
        let store = PackageStore {
            engine: StubEngine { rx },
            rows: names
                .iter()
                .map(|name| PackageRow {
                    name: SharedString::from(name.clone()),
                    status: PackageStatus::Pending,
                    metadata: nudox_engine::PackageMetadata::default(),
                    current_version: None,
                    lineage: None,
                    root: None,
                })
                .collect(),
            _drain: None,
        };
        (tx, store)
    }

    fn loaded(name: &str, symbols: u64) -> PackageLoadEvent {
        PackageLoadEvent::Loaded {
            name: SharedStr::from(name),
            ecosystem: SharedStr::from("cargo"),
            version: None,
            symbol_count: symbols,
            // No root: these tests exercise status folding and label
            // formatting, not navigation. `root_is_retained_across_a_later_event`
            // below is the one that supplies a real key.
            root: None,
            metadata: nudox_engine::PackageMetadata::default(),
        }
    }

    /// A stable `SymbolKey` standing in for a package's root module.
    fn sample_key() -> SymbolKey {
        use nudox_engine::wire::IntroId;
        SymbolKey::new(
            PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("axum")),
            IntroId::from_raw([1; 32]),
        )
    }

    fn loaded_with_root(name: &str, symbols: u64, root: SymbolKey) -> PackageLoadEvent {
        PackageLoadEvent::Loaded {
            name: SharedStr::from(name),
            ecosystem: SharedStr::from("cargo"),
            version: None,
            symbol_count: symbols,
            root: Some(root),
            metadata: nudox_engine::PackageMetadata::default(),
        }
    }

    fn loaded_version(name: &str, version: &str, symbols: u64) -> PackageLoadEvent {
        PackageLoadEvent::Loaded {
            name: SharedStr::from(name),
            ecosystem: SharedStr::from("cargo"),
            version: Some(version.to_owned()),
            symbol_count: symbols,
            root: None,
            metadata: nudox_engine::PackageMetadata::default(),
        }
    }

    fn failed(name: &str, why: &str) -> PackageLoadEvent {
        PackageLoadEvent::LoadFailed {
            name: SharedStr::from(name),
            ecosystem: SharedStr::from("cargo"),
            error: SharedStr::from(why),
        }
    }

    #[test]
    fn an_empty_store_says_no_packages() {
        let (_tx, store) = store_with(&[]);
        assert_eq!(store.summary_label(), "no packages");
    }

    /// The bug this store exists to fix: a real crate takes ~30s to lower, and
    /// "no packages" for 30s reads as a broken app.
    #[test]
    fn a_pending_package_is_named_while_it_loads() {
        let (_tx, store) = store_with(&["axum"]);
        assert_eq!(store.summary_label(), "loading axum…");
    }

    #[test]
    fn several_pending_packages_are_counted() {
        let (_tx, store) = store_with(&["axum", "tower"]);
        assert_eq!(store.summary_label(), "loading 2 packages…");
    }

    #[test]
    fn a_seeded_row_resolves_in_place() {
        let (_tx, mut store) = store_with(&["axum"]);
        store.apply(loaded("axum", 3060));
        assert_eq!(store.rows().len(), 1, "must resolve, not append");
        assert_eq!(store.ready_count(), 1);
        assert_eq!(store.summary_label(), "1 package");
    }

    /// The fixture corpus loads packages nobody named; they must still appear.
    #[test]
    fn an_unrequested_package_appends() {
        let (_tx, mut store) = store_with(&[]);
        store.apply(loaded("fixture_one", 12));
        store.apply(loaded("fixture_two", 7));
        assert_eq!(store.ready_count(), 2);
        assert_eq!(store.summary_label(), "2 packages");
    }

    #[test]
    fn a_failure_is_never_hidden_behind_a_success() {
        let (_tx, mut store) = store_with(&["axum", "tower"]);
        store.apply(loaded("axum", 3060));
        store.apply(failed("tower", "oracle failed"));
        assert_eq!(store.summary_label(), "1 package · 1 failed");
    }

    #[test]
    fn loading_and_ready_coexist_in_the_label() {
        let (_tx, mut store) = store_with(&["axum", "tower"]);
        store.apply(loaded("axum", 3060));
        assert_eq!(store.summary_label(), "1 package · 1 loading");
    }

    #[test]
    fn total_failure_is_reported_rather_than_shown_as_empty() {
        let (_tx, mut store) = store_with(&["axum"]);
        store.apply(failed("axum", "oracle failed"));
        assert_eq!(store.summary_label(), "1 failed");
        assert_eq!(store.ready_count(), 0);
    }

    #[test]
    fn a_repeated_event_does_not_duplicate_a_row() {
        let (_tx, mut store) = store_with(&[]);
        store.apply(loaded("axum", 10));
        store.apply(loaded("axum", 3060));
        assert_eq!(store.rows().len(), 1);
        assert_eq!(
            store.rows()[0].status,
            PackageStatus::Ready { symbols: 3060 },
        );
    }

    #[test]
    fn sequential_generations_update_the_current_visible_version() {
        let (_tx, mut store) = store_with(&[]);
        store.apply(loaded_version("axum", "0.7.9", 10));
        store.apply(loaded_version("axum", "0.8.1", 20));

        assert_eq!(store.rows().len(), 1);
        assert_eq!(
            store.rows()[0].current_version.as_deref(),
            Some("0.8.1"),
            "the visible version must follow the latest generation"
        );
    }

    /// A `Ready` event carries the package's root; the panel navigates to it.
    #[test]
    fn a_ready_package_records_its_root() {
        let (_tx, mut store) = store_with(&["axum"]);
        assert!(
            store.rows()[0].root.is_none(),
            "pending row has no root yet"
        );

        store.apply(loaded_with_root("axum", 4220, sample_key()));

        assert_eq!(store.rows()[0].root, Some(sample_key()));
    }

    /// Minimum provenance repro: a loaded event must preserve metadata already
    /// available from manifest extraction, or an explicit unknown value.
    #[test]
    fn a_loaded_package_exposes_provenance_metadata_to_the_panel() {
        let (_tx, mut store) = store_with(&[]);
        store.apply(PackageLoadEvent::Loaded {
            name: SharedStr::from("axum"),
            ecosystem: SharedStr::from("cargo"),
            version: Some("0.8.4".to_owned()),
            symbol_count: 4220,
            root: None,
            metadata: nudox_engine::PackageMetadata {
                description: Some("HTTP primitives".to_owned()),
                repository: Some("https://github.com/tokio-rs/axum".to_owned()),
                license: Some("MIT".to_owned()),
                owner: Some("tokio-rs".to_owned()),
                dependencies: vec!["hyper".to_owned(), "tower".to_owned()],
                release_date: Some("2026-08-01".to_owned()),
                homepage: Some("https://docs.rs/axum".to_owned()),
                coverage: Some(nudox_engine::PackageCoverage {
                    documented: 8,
                    total: 10,
                }),
            },
        });

        let row = &store.rows()[0];
        assert_eq!(row.metadata.description.as_deref(), Some("HTTP primitives"));
        assert_eq!(
            row.metadata.repository.as_deref(),
            Some("https://github.com/tokio-rs/axum")
        );
        assert_eq!(row.metadata.license.as_deref(), Some("MIT"));
        assert_eq!(row.metadata.owner.as_deref(), Some("tokio-rs"));
        assert_eq!(row.metadata.dependencies, ["hyper", "tower"]);
        assert_eq!(row.metadata.release_date.as_deref(), Some("2026-08-01"));
        assert_eq!(
            row.metadata.homepage.as_deref(),
            Some("https://docs.rs/axum")
        );
        assert_eq!(
            row.metadata.coverage,
            Some(nudox_engine::PackageCoverage {
                documented: 8,
                total: 10,
            })
        );
    }

    /// A later failure must not erase a root we already learned.
    ///
    /// Otherwise a transient failure after a successful load silently makes the
    /// package unnavigable — which presents exactly like "clicking the package
    /// does nothing", the bug this field exists to fix.
    #[test]
    fn a_later_failure_does_not_clear_a_known_root() {
        let (_tx, mut store) = store_with(&[]);
        store.apply(loaded_with_root("axum", 4220, sample_key()));
        store.apply(failed("axum", "producer died"));

        assert_eq!(
            store.rows()[0].root,
            Some(sample_key()),
            "root must survive a later failure",
        );
        assert!(matches!(
            store.rows()[0].status,
            PackageStatus::Failed { .. }
        ));
    }
}
