//! `IndexJobStore` — the packages this session has been asked to fetch and
//! index, and how far each one has got.
//!
//! # Why this is the first thing the Jobs dock has ever had to render
//!
//! `JobsPanel` was a placeholder, and its comment said why: "`EngineHandle::jobs()`
//! hands back an already-closed receiver (LIMITATIONS.md L37), so the engine's
//! job stream is a documented stub and there is nothing here for `lindsey` to
//! subscribe to. That is a backend gap."
//!
//! It still is — `EngineHandle::jobs()` still returns `Err(Unimplemented)`, and
//! this store does **not** pretend otherwise. What changed is that there is now
//! one kind of long-running work the GUI *starts itself*
//! ([`EngineHandle::index_purl`]) and therefore owns a stream for. So this
//! store renders exactly that and nothing else, and the panel says so: an empty
//! Jobs dock here means "no index has been requested", which is a true
//! statement about a set this store is authoritative over, rather than a claim
//! about every job in the engine.
//!
//! # Why the store owns the job list and the view does not
//!
//! §7.4 and §13.3: a view is handed pre-formatted strings and computes nothing
//! in `render`. An index job produces a burst of byte-count updates during the
//! download, and formatting "2.3 MB of 4.1 MB" per frame in `render` is exactly
//! the work that section forbids.

use gpui::{Context, EventEmitter, SharedString, Task};
use nudox_engine::wire::Gen;
use nudox_engine::{IndexEvent, IndexStage, Purl, SymbolKey};

use crate::bridge::drain::drain;

// ---------------------------------------------------------------------------
// Engine capability
// ---------------------------------------------------------------------------

/// The one engine capability this store needs.
///
/// A trait rather than a concrete `EngineHandle`, for the same reason as
/// `PackageEngine`/`SearchEngine`: the store must be testable against a double
/// with no Tokio runtime, no network and no producer — which for *this* store
/// is not a convenience but the only way to test it at all.
pub trait IndexEngine: 'static {
    /// Start indexing `purl`, returning the progress stream.
    ///
    /// The returned handle cancels the job when dropped, so the store holds it
    /// for the life of the job.
    fn index_purl(
        &self,
        purl: Purl,
        generation: Gen,
    ) -> (nudox_engine::StreamHandle, flume::Receiver<IndexEvent>);
}

/// The real engine satisfies the capability directly — a forwarding impl with
/// no conversion, because the moment a conversion appears here the two event
/// vocabularies have drifted.
impl IndexEngine for nudox_engine::EngineHandle {
    fn index_purl(
        &self,
        purl: Purl,
        generation: Gen,
    ) -> (nudox_engine::StreamHandle, flume::Receiver<IndexEvent>) {
        nudox_engine::EngineHandle::index_purl(self, purl, generation)
    }
}

// ---------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------

/// How one index job is going, in the terms the dock renders.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IndexJobState {
    /// Working. `detail` is the pre-formatted progress line.
    Running {
        /// A short verb: "Resolving", "Downloading", …
        stage: SharedString,
        /// "2.3 MB of 4.1 MB", or empty when there is nothing to count yet.
        detail: SharedString,
    },
    /// Finished. `detail` says how many symbols and how the bytes were
    /// verified.
    Done {
        /// "412 symbols · verified against the sha256 published by …"
        detail: SharedString,
        /// The package's root symbol, so the row can be clicked into.
        root: Option<SymbolKey>,
    },
    /// Failed. `detail` is the error message, `help` is what to do next.
    Failed {
        /// The failure, verbatim from the engine.
        detail: SharedString,
        /// The engine's own "what to do next", when it has one.
        ///
        /// Kept separate from `detail` rather than concatenated because they
        /// answer different questions and the panel gives them different
        /// weight — the same split `McpError::help` makes for agents.
        help: Option<SharedString>,
    },
}

/// One row in the Jobs dock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexJobRow {
    /// The canonical package URL, as typed back at the user.
    pub purl: SharedString,
    /// Where the job has got to.
    pub state: IndexJobState,
}

/// Emitted whenever the row list changes, so the dock re-renders.
pub struct IndexJobsChanged;

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

/// Every index job this session has started, newest first.
pub struct IndexJobStore<E: IndexEngine> {
    engine: E,
    rows: Vec<IndexJobRow>,
    /// One drain task and one cancel-on-drop handle per live job.
    ///
    /// Held, not read: dropping either cancels the job. They are dropped when
    /// the job reaches a terminal state, which is also when the engine-side
    /// work is over — so the drop cancels nothing that is still running.
    live: Vec<(nudox_engine::StreamHandle, Task<()>)>,
    generation: u64,
}

impl<E: IndexEngine> EventEmitter<IndexJobsChanged> for IndexJobStore<E> {}

impl<E: IndexEngine> IndexJobStore<E> {
    /// An empty store over `engine`.
    pub fn new(engine: E) -> Self {
        Self {
            engine,
            rows: Vec::new(),
            live: Vec::new(),
            generation: 1,
        }
    }

    /// The rows to render, newest first.
    pub fn rows(&self) -> &[IndexJobRow] {
        &self.rows
    }

    /// True when no index has ever been requested in this session.
    ///
    /// The panel needs this to say "no index has been requested" rather than
    /// "no jobs are running", which would be a claim about the engine's whole
    /// job plane — a plane that still does not exist.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Start indexing `purl`.
    ///
    /// Requesting a PURL that is already running moves it to the top of the
    /// list and starts nothing — the engine would happily run a second fetch
    /// and a second producer over the same sources, and the user pressing
    /// enter twice must not pay for that.
    pub fn start(&mut self, purl: Purl, cx: &mut Context<Self>) {
        let rendered: SharedString = purl.render().into();
        if let Some(existing) = self.rows.iter().position(|r| r.purl == rendered) {
            if matches!(self.rows[existing].state, IndexJobState::Running { .. }) {
                let row = self.rows.remove(existing);
                self.rows.insert(0, row);
                cx.emit(IndexJobsChanged);
                cx.notify();
                return;
            }
            // A finished or failed row is replaced: re-indexing after a failure
            // is a normal thing to do, and two rows for one package would make
            // the dock a history rather than a status.
            self.rows.remove(existing);
        }

        self.rows.insert(
            0,
            IndexJobRow {
                purl: rendered,
                state: IndexJobState::Running {
                    stage: "Resolving".into(),
                    detail: SharedString::default(),
                },
            },
        );

        let generation = Gen(self.generation);
        self.generation += 1;
        let (handle, rx) = self.engine.index_purl(purl, generation);
        let task = drain(cx, rx, |store, event, cx| store.apply(event, cx));
        self.live.push((handle, task));

        cx.emit(IndexJobsChanged);
        cx.notify();
    }

    /// Apply one engine event to the matching row.
    ///
    /// `pub` so a test can drive the store with a hand-built event sequence,
    /// which is how the adversarial cases below (out-of-order, unknown purl,
    /// terminal-then-more) are stated at all.
    pub fn apply(&mut self, event: IndexEvent, cx: &mut Context<Self>) {
        let (purl, state): (SharedString, IndexJobState) = match event {
            IndexEvent::Started { purl, .. } => (
                SharedString::from(purl.to_string()),
                IndexJobState::Running {
                    stage: "Resolving".into(),
                    detail: SharedString::default(),
                },
            ),
            IndexEvent::Stage {
                stage,
                received,
                total,
                ..
            } => {
                // `Stage` carries no purl: the stream belongs to one job, and
                // adding an identity to every progress tick would be a field
                // the engine has to keep correct for no reader. The store
                // resolves it to the one running row instead — see
                // `only_running_row`.
                let Some(index) = self.only_running_row() else {
                    return;
                };
                self.rows[index].state = IndexJobState::Running {
                    stage: stage_label(stage).into(),
                    detail: byte_detail(stage, received, total),
                };
                cx.emit(IndexJobsChanged);
                cx.notify();
                return;
            }
            IndexEvent::Indexed {
                purl,
                symbol_count,
                integrity,
                root,
                ..
            } => (
                SharedString::from(purl.to_string()),
                IndexJobState::Done {
                    detail: format!(
                        "{symbol_count} symbol{} · {}",
                        if symbol_count == 1 { "" } else { "s" },
                        integrity.summary(),
                    )
                    .into(),
                    root,
                },
            ),
            IndexEvent::Failed { error, .. } => {
                let Some(index) = self.only_running_row() else {
                    return;
                };
                self.rows[index].state = IndexJobState::Failed {
                    detail: error.to_string().into(),
                    help: error.help().map(SharedString::from),
                };
                cx.emit(IndexJobsChanged);
                cx.notify();
                return;
            }
            // `IndexEvent` is `#[non_exhaustive]`. A variant this build does not
            // know is not progress it can render and not a terminal state it may
            // mistake for one, so the row keeps saying what it last truthfully
            // said.
            _ => return,
        };

        if let Some(row) = self.rows.iter_mut().find(|r| r.purl == purl) {
            row.state = state;
        } else {
            // An event for a purl nobody asked for: append rather than drop, on
            // the same reasoning as `PackageStore` — the store must not silently
            // discard work the engine really did.
            self.rows.insert(0, IndexJobRow { purl, state });
        }
        cx.emit(IndexJobsChanged);
        cx.notify();
    }

    /// The index of the single running row, when there is exactly one.
    ///
    /// `IndexEvent::Stage` and `IndexEvent::Failed` carry no PURL, so a store
    /// with two jobs in flight cannot attribute them. Rather than guess — which
    /// would put one package's download progress on another package's row — it
    /// declines, and the ambiguous row simply keeps its last accurate state
    /// until a terminal event that *does* carry a PURL arrives.
    ///
    /// This is a real, if narrow, limitation and the honest place to record it:
    /// widening `IndexEvent::Stage` with the job's PURL is an engine change,
    /// and `Started` already carries it, so the fix is cheap when it is worth
    /// making.
    fn only_running_row(&self) -> Option<usize> {
        let mut running = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, r)| matches!(r.state, IndexJobState::Running { .. }));
        let first = running.next()?;
        if running.next().is_some() {
            return None;
        }
        Some(first.0)
    }
}

fn stage_label(stage: IndexStage) -> &'static str {
    match stage {
        IndexStage::Resolving => "Resolving",
        IndexStage::Downloading => "Downloading",
        IndexStage::Verifying => "Verifying",
        IndexStage::Extracting => "Extracting",
        IndexStage::Producing => "Producing documentation",
        IndexStage::Cached => "Already downloaded",
        // `IndexStage` is `#[non_exhaustive]`.
        _ => "Working",
    }
}

/// "2.3 MB of 4.1 MB", or empty when there is nothing to count.
///
/// Formatted here, in the store, and not in `render`: this is called once per
/// event burst, whereas `render` runs per frame (§13.3).
fn byte_detail(stage: IndexStage, received: u64, total: Option<u64>) -> SharedString {
    if stage != IndexStage::Downloading || received == 0 {
        return SharedString::default();
    }
    match total {
        Some(total) if total > 0 => {
            format!("{} of {}", human_bytes(received), human_bytes(total)).into()
        }
        _ => human_bytes(received).into(),
    }
}

fn human_bytes(n: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    let n = n as f64;
    if n >= MB {
        format!("{:.1} MB", n / MB)
    } else if n >= KB {
        format!("{:.0} kB", n / KB)
    } else {
        format!("{n:.0} B")
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_download_without_a_content_length_still_reports_what_arrived() {
        // A registry that does not advertise a length must not produce a blank
        // progress line — "1.2 MB" is less than "1.2 MB of 4.0 MB" and much
        // more than nothing.
        assert_eq!(
            &*byte_detail(IndexStage::Downloading, 1_258_291, None),
            "1.2 MB"
        );
        assert_eq!(
            &*byte_detail(IndexStage::Downloading, 1_258_291, Some(4_194_304)),
            "1.2 MB of 4.0 MB"
        );
    }

    #[test]
    fn a_stage_with_nothing_to_count_produces_no_progress_line_rather_than_zero() {
        // "0 B" during resolution would read as a stalled download.
        assert!(byte_detail(IndexStage::Resolving, 0, None).is_empty());
        assert!(byte_detail(IndexStage::Downloading, 0, Some(100)).is_empty());
        assert!(byte_detail(IndexStage::Producing, 999, None).is_empty());
    }

    #[test]
    fn every_stage_has_a_label_that_says_what_is_happening_not_what_it_is_called() {
        for (stage, expected) in [
            (IndexStage::Resolving, "Resolving"),
            (IndexStage::Downloading, "Downloading"),
            (IndexStage::Verifying, "Verifying"),
            (IndexStage::Extracting, "Extracting"),
            (IndexStage::Producing, "Producing documentation"),
            (IndexStage::Cached, "Already downloaded"),
        ] {
            assert_eq!(stage_label(stage), expected);
        }
    }
}
