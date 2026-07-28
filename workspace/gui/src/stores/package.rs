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
use nudox_engine::PackageLoadEvent;

use crate::bridge::drain::drain;
use crate::stores::events::PackagesChanged;

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
}

/// The real engine satisfies the capability directly — a forwarding impl with
/// no conversion, for the same reason as `SymbolEngine`: the moment a
/// conversion appears here, the two event types have drifted.
impl PackageEngine for nudox_engine::EngineHandle {
    fn packages(&self) -> flume::Receiver<PackageLoadEvent> {
        nudox_engine::EngineHandle::packages(self)
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
        let (name, status) = match event {
            PackageLoadEvent::Loaded {
                name, symbol_count, ..
            } => (name, PackageStatus::Ready {
                symbols: symbol_count,
            }),
            PackageLoadEvent::LoadFailed { name, error, .. } => (
                name,
                PackageStatus::Failed {
                    message: SharedString::from(error.to_string()),
                },
            ),
            // `PackageLoadEvent` is `#[non_exhaustive]`; a variant added later
            // is not a reason to lose the rows we already have.
            _ => return,
        };

        let name = SharedString::from(name.to_string());
        match self.rows.iter_mut().find(|r| r.name == name) {
            Some(row) => row.status = status,
            None => self.rows.push(PackageRow { name, status }),
        }
    }

    /// Every known package, in request-then-arrival order.
    pub fn rows(&self) -> &[PackageRow] {
        &self.rows
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
    }

    fn store_with(
        requested: &[&str],
    ) -> (
        flume::Sender<PackageLoadEvent>,
        PackageStore<StubEngine>,
    ) {
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
}
