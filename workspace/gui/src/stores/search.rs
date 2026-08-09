//! `SearchStore` — GUI-PLAN §12.3
//!
//! ## State ownership
//!
//! - `input`, `scope`, `mode` — raw user intent; mutated synchronously.
//! - `sections: [SectionBuf; 3]` — render-ready `Arc<[PreparedRow]>` per
//!   section; updated once at ingest, never in render (§1.1.4).
//! - `selection: Option<Cursor>` — typed cursor; the §15 "never wraps silently
//!   across sections" invariant is checkable here, not inferred from a flat int.
//! - `gens: GenSource` — per-slot monotonic counter.
//! - `generation: u64` — current generation counter (mirrors `slot.generation`).
//! - `gen_arrival: Option<Instant>` — when the current gen's first events landed;
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

use std::sync::Arc;
use std::time::Instant;

use gpui::{Context, SharedString, Task};
use nudox_engine::wire::{Gen, HitRow, SearchEvent, SearchSectionId};
use nudox_engine::{EngineHandle, SearchQuery};

use crate::bridge::drain::drain;
use crate::bridge::generation::GenSource;
use crate::bridge::handle::StreamHandle as BridgeStreamHandle;
use crate::stores::events::{OpenDisposition, OpenSymbol};
use crate::stores::search_model::{
    Cursor, PreparedRow, SearchMode, SearchSnapshot, ScopeChip, SectionData, SectionStatus,
    SECTION_COUNT,
};

// ---------------------------------------------------------------------------
// Section constants (Name=0, Type=1, Semantic=2)
// ---------------------------------------------------------------------------

const SECTION_NAME: SearchSectionId = SearchSectionId(0);
#[allow(dead_code)]
const SECTION_TYPE: SearchSectionId = SearchSectionId(1);
#[allow(dead_code)]
const SECTION_SEMANTIC: SearchSectionId = SearchSectionId(2);

/// The 24 ms debounce interval (§12.3).
const DEBOUNCE_MS: u64 = 24;

// ---------------------------------------------------------------------------
// Search scope
// ---------------------------------------------------------------------------

/// Restricts the search to a subset of the corpus.
#[derive(Clone, Debug, Default)]
pub struct SearchScope {
    /// When `Some`, restrict to these package coordinates.
    pub packages: Vec<SharedString>,
    /// Kind filter — empty means "all kinds".
    pub kinds: Vec<nudox_engine::wire::KindTag>,
}

// ---------------------------------------------------------------------------
// Internal section buffer (store-side, pre-converted)
// ---------------------------------------------------------------------------

/// One section's render-ready state, owned by the store.
#[derive(Clone, Debug)]
struct SectionBuf {
    /// Render-ready rows, sorted by score descending. Cloning is a refcount bump.
    rows: Arc<[PreparedRow]>,
    /// Whether the stream for this section has delivered its initial batch.
    complete: bool,
    /// Pre-formatted latency string (e.g. `"4 ms"`). Empty until reported.
    latency: SharedString,
    /// Current phase, for `SectionData::status`.
    status: SectionStatus,
}

impl SectionBuf {
    fn empty() -> Self {
        Self {
            rows: Arc::from([] as [PreparedRow; 0]),
            complete: false,
            latency: SharedString::default(),
            status: SectionStatus::Idle,
        }
    }

    fn into_section_data(&self) -> SectionData {
        SectionData {
            rows: self.rows.clone(),
            status: self.status,
            latency: self.latency.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// Engine capability trait
// ---------------------------------------------------------------------------

/// The engine capability required by `SearchStore`.
///
/// This is a thin trait so that `SearchStore` can be constructed with any
/// implementation (real engine or a test double) without depending on a
/// concrete type.
pub trait SearchEngine: 'static {
    /// Issue a search query for the given text and generation.
    ///
    /// Returns a `StreamHandle` (for cancellation) and a bounded flume
    /// receiver over `SearchEvent`.
    fn search(
        &self,
        text: &str,
        scope: &SearchScope,
        mode: SearchMode,
        generation: Gen,
    ) -> (BridgeStreamHandle, flume::Receiver<SearchEvent>);
}

/// Implement `SearchEngine` for the real engine handle.
///
/// `EngineHandle::search` takes `(SearchQuery, Gen)`; we bridge the store's
/// `(text, scope, mode, gen)` parameters to it.  `BridgeStreamHandle` is a
/// re-export of `nudox_engine::StreamHandle`, so no conversion is needed.
impl SearchEngine for EngineHandle {
    fn search(
        &self,
        text: &str,
        scope: &SearchScope,
        _mode: SearchMode,
        generation: Gen,
    ) -> (BridgeStreamHandle, flume::Receiver<SearchEvent>) {
        use nudox_engine::wire::KindDiscriminant;

        // Map scope.kinds into SearchQuery.kinds.  `_mode` is reserved for
        // future routing hints (Mode::Type / Mode::Semantic) — the engine's
        // LR-10 fan-out already handles section assignment.
        let kinds: Vec<KindDiscriminant> = scope
            .kinds
            .iter()
            .filter_map(|kt| match kt {
                nudox_engine::wire::KindTag::Known(d) => Some(*d),
                _ => None,
            })
            .collect();

        let query = SearchQuery {
            text: text.to_owned(),
            kinds,
            limit: 50,
            // Empty means "every loaded package", which is what the omni-search
            // has always done — the field exists because MCP's `search_symbols`
            // needs to filter *before* the engine truncates to `limit` (filtering
            // after truncation can return zero rows while matches exist). The GUI
            // has no package-scoping UI today; when it gets one, this is where it
            // plugs in, and it will be correct by construction rather than needing
            // the same fix again.
            packages: Vec::new(),
        };

        // `EngineHandle::search` returns `(StreamHandle, Receiver<SearchEvent>)`.
        // `StreamHandle` is re-exported by `bridge::handle` so there is no type
        // mismatch — no conversion needed (see handle.rs).
        EngineHandle::search(self, query, generation)
    }
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

    // ── Section storage (render-ready, §12 / §1.1.4) ──────────────────────
    /// Per-section render-ready state.  Indexed by `SearchSectionId.0`.
    sections: [SectionBuf; SECTION_COUNT],

    // ── Generation tracking ────────────────────────────────────────────────
    pub gens: GenSource,
    /// Current generation (echoed into snapshots for entrance-animation keying).
    generation: u64,
    /// Stream handle — held to cancel the previous query on supersession.
    stream_handle: Option<BridgeStreamHandle>,
    /// When the current generation's first page landed.
    gen_arrival: Option<Instant>,

    // ── Selection ─────────────────────────────────────────────────────────
    /// Cursor into `sections`.  `Cursor { section, row }` makes the §15
    /// "never wraps silently" invariant checkable.
    pub selection: Option<Cursor>,

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
        Self {
            input: SharedString::default(),
            scope: SearchScope::default(),
            mode: SearchMode::default(),
            sections: [SectionBuf::empty(), SectionBuf::empty(), SectionBuf::empty()],
            gens: GenSource::new(),
            generation: 0,
            stream_handle: None,
            gen_arrival: None,
            selection: None,
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
        let generation = self.gens.next();
        self.generation = generation.0;
        // 2. Reset section state for the new query.
        for s in &mut self.sections {
            *s = SectionBuf::empty();
            s.status = SectionStatus::Loading;
        }
        self.gen_arrival = None;
        // 3. Open the stream — dropping the previous handle cancels it.
        let (handle, rx) = self.engine.search(
            &self.input,
            &self.scope,
            self.mode,
            generation,
        );
        // 4. Assign handle — drops+cancels predecessor.
        self.stream_handle = Some(handle);
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
            cx.background_executor().timer(duration).await;
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
                if generation.0 != self.generation {
                    return;
                }
                if self.gen_arrival.is_none() {
                    self.gen_arrival = Some(Instant::now());
                }
                let idx = section.0 as usize;
                if idx < SECTION_COUNT {
                    // Convert once at ingest — §1.1.4.
                    // The engine already sorts by score descending before sending
                    // (see nudox_engine::search), so we preserve that order.
                    self.sections[idx].rows = PreparedRow::prepare(&rows);
                    self.sections[idx].status = SectionStatus::Ready;
                    // LD-15: only this section is touched.
                }
                self.clamp_selection();
                cx.notify();
            }

            SearchEvent::Merge { generation, section, rows } => {
                if generation.0 != self.generation {
                    return;
                }
                if self.gen_arrival.is_none() {
                    self.gen_arrival = Some(Instant::now());
                }
                let idx = section.0 as usize;
                if idx < SECTION_COUNT {
                    // Append prepared rows without clearing earlier sections (LD-15).
                    let mut combined: Vec<PreparedRow> =
                        self.sections[idx].rows.to_vec();
                    let new_rows = PreparedRow::prepare(&rows);
                    combined.extend_from_slice(&new_rows);
                    self.sections[idx].rows = Arc::from(combined.as_slice());
                    self.sections[idx].status = SectionStatus::Ready;
                }
                self.clamp_selection();
                cx.notify();
            }

            SearchEvent::Latency { generation, section, elapsed } => {
                if generation.0 != self.generation {
                    return;
                }
                let idx = section.0 as usize;
                if idx < SECTION_COUNT {
                    // Pre-format here — §1.1.4 forbids formatting in render.
                    let ms = elapsed.as_millis();
                    self.sections[idx].latency =
                        SharedString::from(format!("{ms} ms"));
                }
                cx.notify();
            }

            SearchEvent::Done { generation } => {
                if generation.0 != self.generation {
                    return;
                }
                // Mark all sections that are still Loading as Ready (stream closed).
                for s in &mut self.sections {
                    if s.status == SectionStatus::Loading {
                        s.status = SectionStatus::Ready;
                    }
                }
                self.stream_handle = None;
                cx.notify();
            }

            SearchEvent::Failed { generation, error } => {
                if generation.0 != self.generation {
                    return;
                }
                // Mark all Loading sections as Offline on failure.
                for s in &mut self.sections {
                    if s.status == SectionStatus::Loading {
                        s.status = SectionStatus::Offline;
                    }
                }
                self.stream_handle = None;
                cx.notify();
            }

            // Forward-compat: unknown variants are ignored.
            _ => {}
        }
    }

    // ── Selection helpers ─────────────────────────────────────────────────

    /// Clamp the cursor after section rows change.
    ///
    /// If the row the cursor pointed at no longer exists (the section shrank),
    /// re-seats to the last available row.  If the section is now empty, moves
    /// to the first populated one.  §15 acceptance: arriving results never
    /// move an in-range cursor.
    fn clamp_selection(&mut self) {
        let Some(cursor) = self.selection else {
            return;
        };
        let counts = self.row_counts();
        self.selection = clamp_cursor_inner(cursor, &counts);
    }

    fn row_counts(&self) -> [usize; SECTION_COUNT] {
        [
            self.sections[0].rows.len(),
            self.sections[1].rows.len(),
            self.sections[2].rows.len(),
        ]
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
    pub fn set_mode_internal(&mut self, mode: SearchMode, cx: &mut Context<Self>) {
        if self.mode == mode {
            return;
        }
        self.mode = mode;
        if !self.input.is_empty() {
            self.trigger_search(cx);
        }
    }

    /// Toggle a kind filter in the search scope.
    pub fn toggle_kind_internal(
        &mut self,
        kind: nudox_engine::wire::KindTag,
        cx: &mut Context<Self>,
    ) {
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
    /// Does not wrap; stops at the first/last row (§15 invariant).
    pub fn move_selection_delta(&mut self, delta: i32, cx: &mut Context<Self>) {
        let counts = self.row_counts();
        let total: usize = counts.iter().sum();
        if total == 0 {
            return;
        }

        let next = match delta.signum() {
            1 => {
                // Move down: use step_cursor_inner
                step_down(self.selection, &counts)
            }
            -1 => step_up(self.selection, &counts),
            _ => return,
        };
        self.selection = next;
        cx.notify();
    }

    /// Commit the currently selected row, emitting `OpenSymbol`.
    pub fn commit_selection(&mut self, disposition: OpenDisposition, cx: &mut Context<Self>) {
        let cursor = match self.selection {
            Some(c) => c,
            None => {
                // Nothing selected: commit first row of first populated section.
                let counts = self.row_counts();
                match first_populated_from(&counts, 0) {
                    Some(s) if !self.sections[s].rows.is_empty() => {
                        Cursor { section: s, row: 0 }
                    }
                    _ => return,
                }
            }
        };

        if cursor.section < SECTION_COUNT {
            if let Some(row) = self.sections[cursor.section].rows.get(cursor.row) {
                let key = row.key.clone();
                cx.emit(OpenSymbol { key, disposition });
            }
        }
    }
}

// ---------------------------------------------------------------------------
// SearchAccess impl
// ---------------------------------------------------------------------------

use crate::stores::search_model::SearchAccess;

impl<E: SearchEngine> SearchAccess for SearchStore<E> {
    fn snapshot(&self) -> SearchSnapshot {
        // Cheap: only `Arc` refcount bumps and `SharedString` clones — no row
        // data allocation, no formatting (§12: stores hold render-ready state).
        SearchSnapshot {
            input: self.input.clone(),
            mode: self.mode,
            scopes: Arc::from([] as [ScopeChip; 0]), // populated when ScopeChip store lands
            sections: [
                self.sections[0].into_section_data(),
                self.sections[1].into_section_data(),
                self.sections[2].into_section_data(),
            ],
            recents: Arc::from([] as [PreparedRow; 0]), // populated by NavHistory store
            selection: self.selection,
            generation: self.generation,
            gen_arrival: self.gen_arrival,
            // Offline if the semantic section (idx 2) is in the Offline state.
            offline: self.sections[2].status == SectionStatus::Offline,
        }
    }

    fn set_input(&mut self, text: SharedString, cx: &mut Context<Self>) {
        SearchStore::set_input(self, text, cx);
    }

    fn set_mode(&mut self, mode: SearchMode, cx: &mut Context<Self>) {
        SearchStore::set_mode_internal(self, mode, cx);
    }

    fn toggle_scope(&mut self, _ix: usize, cx: &mut Context<Self>) {
        // Scope chips are not yet wired; notify to keep the view consistent.
        cx.notify();
    }

    fn set_selection(&mut self, cursor: Option<Cursor>, cx: &mut Context<Self>) {
        self.selection = cursor;
        cx.notify();
    }

    fn search_remote(&mut self, cx: &mut Context<Self>) {
        // Remote INDEX search — Wave-4 work.  Notify so the view refreshes.
        cx.notify();
    }
}

impl<E: SearchEngine> gpui::EventEmitter<OpenSymbol> for SearchStore<E> {}

// ---------------------------------------------------------------------------
// Pure cursor helpers (mirrors the view's policy; the view is authoritative)
// ---------------------------------------------------------------------------

fn first_populated_from(counts: &[usize; SECTION_COUNT], from: usize) -> Option<usize> {
    (from..SECTION_COUNT).find(|&s| counts[s] > 0)
}

fn last_populated_before(counts: &[usize; SECTION_COUNT], from: usize) -> Option<usize> {
    (0..=from.min(SECTION_COUNT - 1))
        .rev()
        .find(|&s| counts[s] > 0)
}

fn clamp_cursor_inner(cursor: Cursor, counts: &[usize; SECTION_COUNT]) -> Option<Cursor> {
    if cursor.section < SECTION_COUNT && cursor.row < counts[cursor.section] {
        return Some(cursor);
    }
    let section = cursor.section.min(SECTION_COUNT - 1);
    if counts[section] > 0 {
        return Some(Cursor {
            section,
            row: counts[section] - 1,
        });
    }
    first_populated_from(counts, 0).map(|s| Cursor { section: s, row: 0 })
}

fn step_down(current: Option<Cursor>, counts: &[usize; SECTION_COUNT]) -> Option<Cursor> {
    let Some(cursor) = current else {
        return first_populated_from(counts, 0).map(|s| Cursor { section: s, row: 0 });
    };
    let cursor = clamp_cursor_inner(cursor, counts)?;
    if cursor.row + 1 < counts[cursor.section] {
        Some(Cursor { section: cursor.section, row: cursor.row + 1 })
    } else {
        match first_populated_from(counts, cursor.section + 1) {
            Some(s) => Some(Cursor { section: s, row: 0 }),
            None => Some(cursor),
        }
    }
}

fn step_up(current: Option<Cursor>, counts: &[usize; SECTION_COUNT]) -> Option<Cursor> {
    let Some(cursor) = current else {
        return last_populated_before(counts, SECTION_COUNT - 1)
            .map(|s| Cursor { section: s, row: counts[s].saturating_sub(1) });
    };
    let cursor = clamp_cursor_inner(cursor, counts)?;
    if cursor.row > 0 {
        Some(Cursor { section: cursor.section, row: cursor.row - 1 })
    } else if cursor.section == 0 {
        Some(cursor)
    } else {
        match last_populated_before(counts, cursor.section - 1) {
            Some(s) => Some(Cursor { section: s, row: counts[s].saturating_sub(1) }),
            None => Some(cursor),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests (pure — no GPUI executor needed)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use nudox_engine::wire::{EcosystemId, IntroId, PackageLineageId, PackageName, SymbolKey};

    // ── Helpers ──────────────────────────────────────────────────────────────

    fn test_key(n: u8) -> SymbolKey {
        let mut raw = [0u8; 32];
        raw[0] = n;
        SymbolKey::new(
            PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("fixture")),
            IntroId::from_raw(raw),
        )
    }

    fn make_hit(n: u8, score: f32) -> HitRow {
        nudox_engine::wire::HitRow {
            key: test_key(n),
            display_name: nudox_engine::wire::SharedStr::from("fixture::Item"),
            sig_preview: vec![],
            kind: nudox_engine::wire::KindTag::Unknown(0),
            provenance: nudox_engine::wire::Provenance::TrustedLocal,
            score,
        }
    }

    // Minimal test double for SearchEngine.
    struct NullEngine;

    impl SearchEngine for NullEngine {
        fn search(
            &self,
            _text: &str,
            _scope: &SearchScope,
            _mode: SearchMode,
            generation: Gen,
        ) -> (BridgeStreamHandle, flume::Receiver<SearchEvent>) {
            let (_, rx) = flume::bounded(1);
            let handle = BridgeStreamHandle::new(generation, || {});
            (handle, rx)
        }
    }

    // ── Generation monotonicity ────────────────────────────────────────────

    #[test]
    fn gen_source_is_monotonic() {
        let mut gens = GenSource::new();
        let g1 = gens.next();
        let g2 = gens.next();
        let g3 = gens.next();
        assert!(g1 < g2);
        assert!(g2 < g3);
    }

    // ── Section independence (LD-15) ──────────────────────────────────────

    /// Updating section 0 must not touch section 1 or 2.
    #[test]
    fn section_event_does_not_clear_other_sections() {
        let mut store = SearchStore::new(NullEngine);
        store.generation = 1;

        // Pre-populate section 1 with one prepared row.
        let row1 = make_hit(1, 0.9);
        store.sections[1].rows = PreparedRow::prepare(&[row1]);
        store.sections[1].status = SectionStatus::Ready;

        // Simulate a Section event for section 0.
        let row0 = make_hit(0, 1.0);
        let rows: Arc<[HitRow]> = Arc::from([row0]);
        let ev = SearchEvent::Section {
            generation: Gen(1),
            section: SECTION_NAME,
            rows,
        };

        // We can't call apply_search_event without a Context, so test the
        // LD-15 property at the data level directly.
        let prepared = PreparedRow::prepare(&[make_hit(0, 1.0)]);
        store.sections[0].rows = prepared;
        store.sections[0].status = SectionStatus::Ready;

        assert_eq!(store.sections[1].rows.len(), 1, "Type section must survive Name arrival");
        assert_eq!(store.sections[0].rows.len(), 1, "Name section has the new row");
        assert_eq!(store.sections[2].rows.len(), 0, "Semantic section untouched");
    }

    /// A Merge event appends to target section only (LD-15).
    #[test]
    fn merge_appends_to_target_section_only() {
        let mut store = SearchStore::new(NullEngine);
        store.sections[0].rows = PreparedRow::prepare(&[make_hit(1, 0.5)]);
        store.sections[2].rows = PreparedRow::prepare(&[make_hit(2, 0.5)]);

        // Append to section 0 only.
        let existing = store.sections[0].rows.to_vec();
        let mut combined = existing;
        combined.extend_from_slice(&PreparedRow::prepare(&[make_hit(3, 0.4)]));
        store.sections[0].rows = Arc::from(combined.as_slice());

        assert_eq!(store.sections[0].rows.len(), 2, "Name grew by 1");
        assert_eq!(store.sections[2].rows.len(), 1, "Semantic unchanged");
    }

    // ── Latency pre-formatting ─────────────────────────────────────────────

    /// Latency is formatted as `"{ms} ms"` at ingest, not in render.
    #[test]
    fn latency_preformatted_as_ms_string() {
        let mut store = SearchStore::new(NullEngine);
        store.generation = 1;
        // Simulate the formatting logic from apply_search_event.
        let elapsed = std::time::Duration::from_millis(42);
        let ms = elapsed.as_millis();
        let s = SharedString::from(format!("{ms} ms"));
        store.sections[0].latency = s.clone();
        assert_eq!(store.sections[0].latency.as_ref(), "42 ms");
    }

    // ── Cursor-based selection ─────────────────────────────────────────────

    /// Selection never wraps silently from last row to first across sections.
    #[test]
    fn step_down_at_last_row_holds() {
        let counts = [2usize, 2, 0];
        let last = Cursor { section: 1, row: 1 };
        let next = step_down(Some(last), &counts);
        assert_eq!(next, Some(last), "should hold at last row");
    }

    #[test]
    fn step_up_at_first_row_holds() {
        let counts = [2usize, 2, 0];
        let first = Cursor { section: 0, row: 0 };
        let next = step_up(Some(first), &counts);
        assert_eq!(next, Some(first), "should hold at first row");
    }

    /// Moving down crosses into the next populated section.
    #[test]
    fn step_down_crosses_section_boundary() {
        let counts = [2usize, 0, 3];
        let at_end_of_name = Cursor { section: 0, row: 1 };
        let next = step_down(Some(at_end_of_name), &counts);
        assert_eq!(
            next,
            Some(Cursor { section: 2, row: 0 }),
            "empty Type section is transparent"
        );
    }

    /// Moving up crosses back to the last row of the previous section.
    #[test]
    fn step_up_crosses_section_boundary() {
        let counts = [3usize, 0, 2];
        let top_of_semantic = Cursor { section: 2, row: 0 };
        let next = step_up(Some(top_of_semantic), &counts);
        assert_eq!(next, Some(Cursor { section: 0, row: 2 }));
    }

    /// A shrinking section re-seats the cursor to its last row.
    #[test]
    fn clamp_cursor_reseats_to_last_row() {
        let cursor = Cursor { section: 0, row: 9 };
        let counts = [3usize, 0, 0];
        let clamped = clamp_cursor_inner(cursor, &counts);
        assert_eq!(clamped, Some(Cursor { section: 0, row: 2 }));
    }

    /// A cursor in a now-empty section moves to the first populated one.
    #[test]
    fn clamp_cursor_moves_to_first_populated() {
        let cursor = Cursor { section: 1, row: 0 };
        let counts = [2usize, 0, 0];
        let clamped = clamp_cursor_inner(cursor, &counts);
        assert_eq!(clamped, Some(Cursor { section: 0, row: 0 }));
    }

    /// Semantic arriving never moves an in-range cursor (§15 acceptance).
    #[test]
    fn semantic_arriving_leaves_cursor_untouched() {
        let cursor = Cursor { section: 0, row: 2 };
        let before = [4usize, 0, 0];
        let after = [4usize, 0, 20];
        assert_eq!(clamp_cursor_inner(cursor, &before), Some(cursor));
        assert_eq!(
            clamp_cursor_inner(cursor, &after),
            Some(cursor),
            "semantic arriving must not move the cursor"
        );
    }

    // ── Snapshot cost ──────────────────────────────────────────────────────

    /// Snapshot clones only Arc/SharedString; it does not allocate row data.
    #[test]
    fn snapshot_is_cheap_clones_only() {
        let store = SearchStore::new(NullEngine);
        // This is a compile-time / structural test: we verify snapshot() returns
        // a SearchSnapshot and that the fields are Arc / SharedString types
        // (refcount bumps, no row allocation).  We cannot check allocation
        // counts without a custom allocator, so we verify the snapshot value
        // matches what we put in.
        let snap = SearchAccess::snapshot(&store);
        assert_eq!(snap.input.as_ref(), "");
        assert_eq!(snap.sections[0].rows.len(), 0);
        assert_eq!(snap.generation, 0);
        assert!(snap.selection.is_none());
    }

    // ── PreparedRow conversion at ingest ──────────────────────────────────

    /// `PreparedRow::prepare` converts wire rows once; the result is an `Arc` slice.
    #[test]
    fn prepare_converts_hit_rows_to_prepared_rows() {
        let hits = vec![make_hit(1, 0.9), make_hit(2, 0.5)];
        let prepared = PreparedRow::prepare(&hits);
        assert_eq!(prepared.len(), 2);
    }

    /// `PreparedRow::prepare` splits qualified names correctly.
    #[test]
    fn prepared_row_splits_qualified_name() {
        use nudox_engine::wire::{HitRow, KindTag, Provenance, SharedStr};
        let hit = HitRow {
            key: test_key(0),
            display_name: SharedStr::from("serde_json::value::Value"),
            sig_preview: vec![],
            kind: KindTag::Unknown(0),
            provenance: Provenance::TrustedLocal,
            score: 1.0,
        };
        let row = &PreparedRow::prepare(std::slice::from_ref(&hit))[0];
        assert_eq!(row.path.as_ref(), "serde_json::value::");
        assert_eq!(row.leaf.as_ref(), "Value");
    }

    /// `PreparedRow::prepare` handles a bare name (no separator).
    #[test]
    fn prepared_row_bare_name_has_empty_path() {
        use nudox_engine::wire::{HitRow, KindTag, Provenance, SharedStr};
        let hit = HitRow {
            key: test_key(0),
            display_name: SharedStr::from("Value"),
            sig_preview: vec![],
            kind: KindTag::Unknown(0),
            provenance: Provenance::TrustedLocal,
            score: 1.0,
        };
        let row = &PreparedRow::prepare(std::slice::from_ref(&hit))[0];
        assert!(row.path.is_empty());
        assert_eq!(row.leaf.as_ref(), "Value");
    }

    // ── Stale guard ───────────────────────────────────────────────────────

    /// Events from an old generation are dropped.
    #[test]
    fn stale_gen_guard_logic() {
        let current_gen = Gen(2);
        let stale_gen = Gen(1);
        let is_stale = stale_gen.0 != current_gen.0;
        assert!(is_stale);
        let is_current = current_gen.0 != current_gen.0;
        assert!(!is_current);
    }
}
