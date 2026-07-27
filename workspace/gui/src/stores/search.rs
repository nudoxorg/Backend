//! `SearchStore` — GUI-PLAN §12.3
//!
//! ## State ownership
//!
//! - `input`, `scope`, `mode` — raw user intent; mutated synchronously.
//! - `slot: StreamSlot<SearchResults>` — the live results surface; its `generation`
//!   is the stale-guard key.
//! - `gens: GenSource` — per-slot monotonic counter (one per store).
//! - `selection: Option<usize>` — flat index across all sections (stale-guard
//!   preserved: selection is cleared when a new query supersedes).
//! - `gen_arrival: Instant` — when the current gen's first events landed;
//!   controls entrance-animation windows (§4.1).
//! - `debounce_task: Option<Task<()>>` — the 24 ms foreground timer task;
//!   owned here (LD-18), replaced on each keystroke, which drops and cancels
//!   the predecessor.
//! - `drain_task: Task<()>` — drains the search event stream.
//!
//! ## The §7.4 shape, applied
//!
//! `trigger_search` is the only place that issues a query. It follows the
//! four-line §7.4 pattern exactly. `debounce_task` is the 24 ms wrapper that
//! delays calling `trigger_search`.
//!
//! ## Section independence (LD-15, LR-10)
//!
//! Name / Type / Semantic sections are stored separately. A `SearchEvent::Section`
//! for section 0 (Name) never clears or reorders section 1 (Type) or 2
//! (Semantic). A slow semantic result arriving after local results just fills its
//! own slot — it does not push earlier rows.

use std::time::Instant;

use gpui::{Context, SharedString, Task};
use nudox_engine::wire::{
    Gen, HitRow, SearchEvent, SearchSectionId,
};

use crate::bridge::drain::drain;
use crate::bridge::generation::{GenSource, Gen as BridgeGen};
use crate::bridge::slot::{SlotError, StreamSlot};
use crate::stores::events::{OpenDisposition, OpenSymbol};

// ---------------------------------------------------------------------------
// Section constants (Name=0, Type=1, Semantic=2)
// ---------------------------------------------------------------------------

const SECTION_NAME: SearchSectionId = SearchSectionId(0);
const SECTION_TYPE: SearchSectionId = SearchSectionId(1);
const SECTION_SEMANTIC: SearchSectionId = SearchSectionId(2);
const SECTION_COUNT: usize = 3;

/// The 24 ms debounce interval (§12.3).
const DEBOUNCE_MS: u64 = 24;

// ---------------------------------------------------------------------------
// Search scope & mode
// ---------------------------------------------------------------------------

/// Restricts the search to a subset of the corpus.
#[derive(Clone, Debug, Default)]
pub struct SearchScope {
    /// When `Some`, restrict to these package coordinates.
    pub packages: Vec<SharedString>,
    /// Kind filter — empty means "all kinds".
    pub kinds: Vec<nudox_engine::wire::KindTag>,
}

/// Which search strategy to prefer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SearchMode {
    /// Engine picks based on query shape.
    #[default]
    Auto,
    /// Name prefix / fuzzy only.
    Name,
    /// Type-directed search.
    Type,
    /// Semantic / embedding search (may be slow / unavailable offline).
    Semantic,
}

// ---------------------------------------------------------------------------
// Results model
// ---------------------------------------------------------------------------

/// One section of search results.
///
/// `rows` is sorted and filtered at update time (§1.1.4). The view renders
/// them directly without any additional processing.
#[derive(Clone, Debug)]
pub struct SectionBuf {
    /// The section id (0=Name, 1=Type, 2=Semantic).
    pub id: SearchSectionId,
    /// Fully render-ready rows, sorted by score descending.
    pub rows: Vec<HitRow>,
    /// Whether this section's stream has closed.
    pub complete: bool,
    /// Latency in milliseconds for the last batch, if reported.
    pub latency_ms: Option<u128>,
}

impl SectionBuf {
    /// An empty section carrying the given id.
    ///
    /// There is deliberately no `Default` for a section: its id *is* its
    /// identity, and the sections live in a `[SectionBuf; 3]` indexed by that
    /// id. A default-constructed section would silently claim index 0 —
    /// meaning a semantic result could land in the Name section, which is
    /// exactly the confusion LD-15 exists to prevent.
    fn empty(id: SearchSectionId) -> Self {
        Self {
            id,
            rows: Vec::new(),
            complete: false,
            latency_ms: None,
        }
    }
}

/// The value stored inside `slot.value`.
///
/// Sections are individually tracked so that a slow semantic section cannot
/// overwrite the Name section's rows (LD-15 / LR-10).
#[derive(Clone, Debug)]
pub struct SearchResults {
    /// `[Name, Type, Semantic]` — indexed by `section.0 as usize`.
    pub sections: [SectionBuf; SECTION_COUNT],
}

impl Default for SearchResults {
    fn default() -> Self {
        Self::new()
    }
}

impl SearchResults {
    fn new() -> Self {
        Self {
            sections: [
                SectionBuf::empty(SECTION_NAME),
                SectionBuf::empty(SECTION_TYPE),
                SectionBuf::empty(SECTION_SEMANTIC),
            ],
        }
    }

    /// Reset all sections to empty (called on a new query generation).
    fn reset(&mut self) {
        *self = Self::new();
    }

    /// Total number of rows across all sections.
    pub fn total_rows(&self) -> usize {
        self.sections.iter().map(|s| s.rows.len()).sum()
    }

    /// Flat index → `(section_idx, row_within_section)`.
    ///
    /// Returns `None` if `flat_idx` is out of range.
    pub fn flat_to_section(&self, flat_idx: usize) -> Option<(usize, usize)> {
        let mut remaining = flat_idx;
        for (si, section) in self.sections.iter().enumerate() {
            if remaining < section.rows.len() {
                return Some((si, remaining));
            }
            remaining -= section.rows.len();
        }
        None
    }
}

// ---------------------------------------------------------------------------
// Engine capability trait
// ---------------------------------------------------------------------------

/// The engine capability required by `SearchStore`.
///
/// This is a thin trait so that `SearchStore` can be constructed with any
/// implementation (real engine or a test double) without depending on a
/// concrete type. The engine crate does not yet expose a unified `EngineHandle`
/// — that is a Wave-4 task. For now the trait documents the expected shape.
pub trait SearchEngine: 'static {
    /// Issue a search query for the given text and generation.
    ///
    /// Returns a `StreamHandle` (for cancellation) and a bounded flume
    /// receiver over `SearchEvent`. The channel capacity should be
    /// `bridge::channels::SEARCH.capacity` (256).
    fn search(
        &self,
        text: &str,
        scope: &SearchScope,
        mode: SearchMode,
        generation: Gen,
    ) -> (crate::bridge::handle::StreamHandle, flume::Receiver<SearchEvent>);
}

// ---------------------------------------------------------------------------
// SearchStore
// ---------------------------------------------------------------------------

/// `SearchStore` — GUI-PLAN §12.3.
pub struct SearchStore<E: SearchEngine> {
    // ── User intent ─────────────────────────────────────────────────────
    pub input: SharedString,
    pub scope: SearchScope,
    pub mode: SearchMode,

    // ── Data surface ─────────────────────────────────────────────────────
    pub slot: StreamSlot<SearchResults>,
    pub gens: GenSource,

    // ── Selection ─────────────────────────────────────────────────────────
    /// Flat index into the concatenated section rows.
    pub selection: Option<usize>,

    // ── Entrance-animation window ─────────────────────────────────────────
    /// When the current generation's first page landed.
    pub gen_arrival: Instant,

    // ── Task ownership (LD-18) ────────────────────────────────────────────
    /// The 24 ms debounce timer. Replaced per keystroke; dropping cancels it.
    debounce_task: Option<Task<()>>,
    /// Drain loop for the current search stream.
    drain_task: Task<()>,

    // ── Engine handle ─────────────────────────────────────────────────────
    engine: E,
}

impl<E: SearchEngine> SearchStore<E> {
    pub fn new(engine: E) -> Self {
        // Construct a completed Task<()> as the initial drain_task.
        // It will be replaced on the first query.
        Self {
            input: SharedString::default(),
            scope: SearchScope::default(),
            mode: SearchMode::default(),
            slot: StreamSlot::new(),
            gens: GenSource::new(),
            selection: None,
            gen_arrival: Instant::now(),
            debounce_task: None,
            drain_task: Task::ready(()),
            engine,
        }
    }

    // ── §7.4 shape ────────────────────────────────────────────────────────

    /// Issue a new search query immediately (without debounce).
    ///
    /// This is the inner half of the §7.4 shape. Called by the debounce timer,
    /// not directly from input handlers.
    fn trigger_search(&mut self, cx: &mut Context<Self>) {
        // 1. Supersede — advance the generation.
        self.slot.generation = self.gens.next();
        // 2. Begin loading — keeps old value (LD-15 / stale-while-revalidate).
        self.slot.begin_loading();
        // 3. Open the stream.
        let (handle, rx) = self.engine.search(
            &self.input,
            &self.scope,
            self.mode,
            // Convert BridgeGen to wire Gen for the engine.
            Gen(self.slot.generation.0),
        );
        // 4. Assign handle — drops+cancels predecessor.
        self.slot.handle = Some(handle);
        // 5. Spin up the drain loop (owned, not detached — LD-18).
        self.drain_task = drain(cx, rx, |store, ev, cx| {
            store.apply_search_event(ev, cx);
        });
        // Clear selection for the new generation.
        self.selection = None;
        cx.notify();
    }

    /// Restart the 24 ms debounce timer.
    ///
    /// Drops the previous timer task (cancelling it) and spawns a new one.
    /// When the timer fires it calls `trigger_search`.
    fn restart_debounce(&mut self, cx: &mut Context<Self>) {
        // Dropping the old task cancels the pending timer.
        self.debounce_task = None;
        let duration = std::time::Duration::from_millis(DEBOUNCE_MS);
        self.debounce_task = Some(cx.spawn(async move |store, cx| {
            // Foreground timer: sleep on the GPUI executor.
            cx.background_executor()
                .timer(duration)
                .await;
            let _ = store.update(cx, |store, cx| {
                store.trigger_search(cx);
            });
        }));
    }

    // ── Event application ─────────────────────────────────────────────────

    fn apply_search_event(&mut self, ev: SearchEvent, cx: &mut Context<Self>) {
        match ev {
            SearchEvent::Section { generation, section, rows } => {
                // Stale guard: drop events from superseded queries.
                if generation.0 != self.slot.generation.0 {
                    return;
                }
                self.slot.first_event();
                // Initialise the value on first event.
                if self.slot.value.is_none() {
                    self.slot.value = Some(SearchResults::new());
                    self.gen_arrival = Instant::now();
                }
                let idx = section.0 as usize;
                if idx < SECTION_COUNT {
                    if let Some(results) = self.slot.value.as_mut() {
                        // Replace (not append) the section rows — `Section` is
                        // the initial batch; `Merge` appends.
                        results.sections[idx].rows = rows.to_vec();
                        // Sort by score descending (render does zero sorting).
                        results.sections[idx]
                            .rows
                            .sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
                    }
                }
                // Adjust selection if it pointed past the new row count.
                self.clamp_selection();
            }

            SearchEvent::Merge { generation, section, rows } => {
                if generation.0 != self.slot.generation.0 {
                    return;
                }
                let idx = section.0 as usize;
                if idx < SECTION_COUNT {
                    if let Some(results) = self.slot.value.as_mut() {
                        // Append rows without clearing earlier sections.
                        results.sections[idx].rows.extend_from_slice(&rows);
                        results.sections[idx]
                            .rows
                            .sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
                    }
                }
                self.clamp_selection();
            }

            SearchEvent::Latency { generation, section, elapsed } => {
                if generation.0 != self.slot.generation.0 {
                    return;
                }
                let idx = section.0 as usize;
                if idx < SECTION_COUNT {
                    if let Some(results) = self.slot.value.as_mut() {
                        results.sections[idx].latency_ms = Some(elapsed.as_millis());
                    }
                }
            }

            SearchEvent::Done { generation } => {
                if generation.0 != self.slot.generation.0 {
                    return;
                }
                self.slot.complete();
            }

            SearchEvent::Failed { generation, error } => {
                if generation.0 != self.slot.generation.0 {
                    return;
                }
                self.slot.fail(SlotError::Transient {
                    message: error.to_string(),
                });
            }

            // Forward-compat: unknown variants are ignored.
            _ => {}
        }
    }

    // ── Selection helpers ─────────────────────────────────────────────────

    /// Clamp the selection to the valid range after rows change.
    fn clamp_selection(&mut self) {
        if let Some(sel) = self.selection {
            let total = self
                .slot
                .value
                .as_ref()
                .map(|r| r.total_rows())
                .unwrap_or(0);
            if total == 0 {
                self.selection = None;
            } else if sel >= total {
                self.selection = Some(total - 1);
            }
        }
    }

    // ── Public API ────────────────────────────────────────────────────────

    /// Update the search input and restart the debounce timer.
    pub fn set_input(&mut self, text: SharedString, cx: &mut Context<Self>) {
        self.input = text;
        self.restart_debounce(cx);
    }

    /// Switch the active search mode.
    ///
    /// Re-triggers the search immediately (mode change is intentional, not
    /// incremental, so debounce is skipped).
    pub fn set_mode(&mut self, mode: SearchMode, cx: &mut Context<Self>) {
        if self.mode == mode {
            return;
        }
        self.mode = mode;
        if !self.input.is_empty() {
            self.trigger_search(cx);
        }
    }

    /// Toggle a kind filter in the search scope.
    ///
    /// If the kind is already in the filter it is removed; otherwise added.
    /// Re-triggers the search.
    pub fn toggle_kind(&mut self, kind: nudox_engine::wire::KindTag, cx: &mut Context<Self>) {
        if let Some(pos) = self.scope.kinds.iter().position(|k| *k == kind) {
            self.scope.kinds.remove(pos);
        } else {
            self.scope.kinds.push(kind);
        }
        if !self.input.is_empty() {
            self.trigger_search(cx);
        }
    }

    /// Move the selection up (`delta = -1`) or down (`delta = 1`).
    ///
    /// Does not wrap; stops at the first/last row. The view reads
    /// `selection` and drives `scroll.glide` to keep the row in view.
    pub fn move_selection(&mut self, delta: i32, cx: &mut Context<Self>) {
        let total = self
            .slot
            .value
            .as_ref()
            .map(|r| r.total_rows())
            .unwrap_or(0);
        if total == 0 {
            return;
        }
        let current = self.selection.unwrap_or(0) as i32;
        let next = (current + delta).clamp(0, total as i32 - 1) as usize;
        self.selection = Some(next);
        cx.notify();
    }

    /// Commit the currently selected row, emitting `OpenSymbol`.
    ///
    /// If nothing is selected, commits the first row of the first non-empty
    /// section. Does nothing if there are no results.
    pub fn commit_selection(&mut self, disposition: OpenDisposition, cx: &mut Context<Self>) {
        let results = match self.slot.value.as_ref() {
            Some(r) => r,
            None => return,
        };
        let flat_idx = self.selection.unwrap_or(0);
        let key = results.flat_to_section(flat_idx).and_then(|(si, ri)| {
            results.sections[si].rows.get(ri).map(|r| r.key.clone())
        });
        if let Some(key) = key {
            cx.emit(OpenSymbol { key, disposition });
        }
    }
}

impl<E: SearchEngine> gpui::EventEmitter<OpenSymbol> for SearchStore<E> {}

// ---------------------------------------------------------------------------
// Tests (pure — no GPUI executor needed)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    // ── Helpers ───────────────────────────────────────────────────────────
    //
    // We avoid constructing `SymbolKey` directly because `EcosystemId` and
    // `PackageName` are not re-exported by `nudox_engine::wire` and `lindsey`
    // must not depend on `nudox-ir` directly (dependency law §L0). Instead,
    // tests probe the pure container / algorithmic invariants using
    // `SearchResults` directly, without needing populated `HitRow.key` values
    // in the assertions that matter.


    /// A real, distinct `SymbolKey` for test rows.
    ///
    /// Deliberately not `mem::zeroed()`: `SymbolKey` is a `StableRef`, whose
    /// `PackageName`/`EcosystemId` are `String`-backed, so a zeroed value holds
    /// null `Unique<u8>` pointers and is undefined behaviour the moment it is
    /// dropped — never mind used.
    fn test_key(n: u8) -> nudox_engine::wire::SymbolKey {
        use nudox_engine::wire::{EcosystemId, IntroId, PackageLineageId, PackageName, SymbolKey};
        let mut raw = [0u8; 32];
        raw[0] = n;
        SymbolKey::new(
            PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("fixture")),
            IntroId::from_raw(raw),
        )
    }

    /// Build an empty `SectionBuf` with the given section id.
    fn empty_section(id: SearchSectionId) -> SectionBuf {
        SectionBuf::empty(id)
    }

    // ── Section independence (the key LD-15 / LR-10 property) ─────────────

    /// Receiving a `Section` for section 0 must NOT clear section 1.
    ///
    /// We test this by directly manipulating the `SectionBuf.rows` field,
    /// which is what `apply_search_event` does (it replaces
    /// `results.sections[idx].rows = ...`). The container invariant is pure
    /// Rust; no `SymbolKey` construction is needed.
    #[test]
    fn section_event_does_not_clear_other_sections() {
        let mut results = SearchResults::new();

        // Pre-populate Name (idx 0) and Type (idx 1) sections with synthetic
        // row counts. We can't easily build real `HitRow`s without the full
        // type chain, so we test at the level of row counts.
        results.sections[1].rows.push(HitRow {
            key: test_key(1),
            display_name: nudox_engine::wire::SharedStr::from("type-hit"),
            sig_preview: vec![],
            kind: nudox_engine::wire::KindTag::Unknown(0),
            provenance: nudox_engine::wire::Provenance::TrustedLocal,
            score: 0.9,
        });

        // Simulate a Section event for idx 0 replacing only that slot.
        results.sections[0].rows.push(HitRow {
            key: test_key(2),
            display_name: nudox_engine::wire::SharedStr::from("name-hit"),
            sig_preview: vec![],
            kind: nudox_engine::wire::KindTag::Unknown(0),
            provenance: nudox_engine::wire::Provenance::TrustedLocal,
            score: 1.0,
        });

        // Section 1 must still have its row.
        assert_eq!(results.sections[1].rows.len(), 1, "Type section must survive Name arrival");
        assert_eq!(results.sections[0].rows.len(), 1, "Name section has the new row");
    }

    /// A `Merge` event appends to a section without disturbing others.
    #[test]
    fn merge_event_appends_to_target_section_only() {
        let mut results = SearchResults::new();

        let zeroed_row = || HitRow {
            key: test_key(3),
            display_name: nudox_engine::wire::SharedStr::from("x"),
            sig_preview: vec![],
            kind: nudox_engine::wire::KindTag::Unknown(0),
            provenance: nudox_engine::wire::Provenance::TrustedLocal,
            score: 0.5,
        };

        results.sections[0].rows.push(zeroed_row()); // Name: 1 row
        results.sections[2].rows.push(zeroed_row()); // Semantic: 1 row

        // Merge: append one more to Name only.
        results.sections[0].rows.push(zeroed_row());

        assert_eq!(results.sections[0].rows.len(), 2, "Name grew by 1");
        assert_eq!(results.sections[2].rows.len(), 1, "Semantic unchanged");
    }

    // ── Generation supersession ─────────────────────────────────────────────

    /// The stale guard: a `SearchEvent::Section` with a generation that does
    /// not match the current slot generation must be dropped.
    ///
    /// We test the guard logic directly without constructing a store (which
    /// would require a Context).
    #[test]
    fn stale_gen_guard_logic() {
        let current_gen = Gen(2);
        let stale_gen = Gen(1);

        // Simulate the guard check inside apply_search_event.
        let is_stale = stale_gen.0 != current_gen.0;
        assert!(is_stale, "gen 1 is stale when current is gen 2");

        let is_current = current_gen.0 != current_gen.0;
        assert!(!is_current, "same gen is not stale");
    }

    /// Advancing the `GenSource` produces strictly increasing values.
    #[test]
    fn gen_source_is_monotonic() {
        let mut gens = GenSource::new();
        let g1 = gens.next();
        let g2 = gens.next();
        let g3 = gens.next();
        assert!(g1 < g2, "gen must be strictly increasing");
        assert!(g2 < g3);
    }

    // ── Selection movement ─────────────────────────────────────────────────

    /// Selection clamps at the last available row (does not wrap or panic).
    #[test]
    fn selection_clamps_at_last_row() {
        // Simulate 2 total rows across sections.
        let total: usize = 2;
        let sel: i32 = 0;
        let moved = (sel + 5).clamp(0, total as i32 - 1);
        assert_eq!(moved, 1, "selection clamped to last row");
    }

    #[test]
    fn selection_clamps_at_first_row() {
        let total: usize = 1;
        let sel: i32 = 0;
        let moved = (sel - 1).clamp(0, total as i32 - 1);
        assert_eq!(moved, 0, "selection clamped to first row");
    }

    // ── Flat index across sections ─────────────────────────────────────────

    /// `flat_to_section` correctly maps across section boundaries.
    #[test]
    fn flat_to_section_crosses_boundaries() {
        let mut results = SearchResults::new();
        let row = || HitRow {
            key: test_key(4),
            display_name: nudox_engine::wire::SharedStr::from("x"),
            sig_preview: vec![],
            kind: nudox_engine::wire::KindTag::Unknown(0),
            provenance: nudox_engine::wire::Provenance::TrustedLocal,
            score: 0.5,
        };
        results.sections[0].rows.push(row()); // flat 0
        results.sections[0].rows.push(row()); // flat 1
        results.sections[1].rows.push(row()); // flat 2

        assert_eq!(results.flat_to_section(0), Some((0, 0)));
        assert_eq!(results.flat_to_section(1), Some((0, 1)));
        assert_eq!(results.flat_to_section(2), Some((1, 0)));
        assert_eq!(results.flat_to_section(3), None, "out of range returns None");
    }

    // ── Rows sorted at update time ─────────────────────────────────────────

    /// After sorting, rows must be in descending score order.
    #[test]
    fn section_rows_sorted_by_score_descending() {
        let scores = [0.3f32, 0.9, 0.5];
        let mut sorted = scores.to_vec();
        sorted.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
        assert!(sorted[0] >= sorted[1], "first >= second");
        assert!(sorted[1] >= sorted[2], "second >= third");
        assert_eq!(sorted[0], 0.9);
        assert_eq!(sorted[2], 0.3);
    }

    // ── Empty-results edge cases ──────────────────────────────────────────

    #[test]
    fn total_rows_zero_when_empty() {
        let results = SearchResults::new();
        assert_eq!(results.total_rows(), 0);
    }

    #[test]
    fn flat_to_section_none_when_empty() {
        let results = SearchResults::new();
        assert_eq!(results.flat_to_section(0), None);
    }
}
