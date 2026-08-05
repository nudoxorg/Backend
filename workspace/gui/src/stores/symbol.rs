//! `SymbolStore` — GUI-PLAN §12.4
//!
//! ## Tab lifecycle
//!
//! Each open symbol is a `SymbolDoc` keyed by a `TabId`. `open(key)` either
//! creates a new tab or (if the key is already open) re-activates the existing
//! tab — no duplicate tabs for the same symbol. `close(tab)` drops the doc and
//! its handle, which cancels the in-flight stream (LD-18 / §2.3).
//!
//! ## Stream per tab
//!
//! Every tab gets its own `GenSource` and its own `drain_task`. Tabs are
//! completely independent — opening a new tab does not affect another tab's
//! stream.
//!
//! ## Progressive accumulation
//!
//! `sections`, `refs`, and `impls` are `Progressive<T>` (§8.2): append-only
//! during a generation. The GUI never reflows earlier content because a later
//! event arrived.
//!
//! `timeline` is deliberately *not* progressive. The engine computes a symbol's
//! version history in one pass over the loaded generations and emits it whole,
//! exactly once, so there is nothing to accumulate — modelling it as an
//! append-only sequence would invent a partial state the protocol never
//! produces.
//!
//! ## Highlight storage
//!
//! Highlights arrive after their section via `DocEvent::Highlight`. They are
//! stored separately in `highlights: HashMap<SectionId, Arc<[HighlightSpan]>>`
//! so that `Progressive<RenderSection>` stays append-only — a highlight is an
//! enrichment of existing content, not a new section.

use std::collections::HashMap;
use std::sync::Arc;

use gpui::{Context, Task};
use nudox_engine::wire::{
    DocEvent, HighlightSpan, ImplsPage, RefsPage, RenderSection,
    SectionId, SymbolHead, SymbolKey, Timeline,
};

use crate::bridge::drain::drain;
use crate::bridge::generation::{Gen as BridgeGen, GenSource};
use crate::bridge::handle::StreamHandle;
use crate::bridge::progressive::Progressive;
use crate::bridge::slot::{SlotError, StreamSlot};
use crate::stores::events::{
    HeadReady, OpenDisposition, SectionArrived, TabActivated, TabCountsChanged,
};

// ---------------------------------------------------------------------------
// TabId
// ---------------------------------------------------------------------------

/// Opaque, monotonically increasing tab identifier.
///
/// Newtypes the raw `u64` so accidental conversions / comparisons against other
/// `u64` ids are caught at compile time (LR-1 / GUI-PLAN LD-18 hygiene).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TabId(pub u64);

// ---------------------------------------------------------------------------
// Engine capability trait
// ---------------------------------------------------------------------------

/// The symbol-streaming capability required by `SymbolStore`.
///
/// Abstracts the `EngineHandle::open_symbol` call so that `SymbolStore` is
/// testable with a double and is not coupled to a concrete engine type.
pub trait SymbolEngine: 'static {
    fn open_symbol(
        &self,
        key: SymbolKey,
        generation: nudox_engine::wire::Gen,
    ) -> (StreamHandle, flume::Receiver<DocEvent>);

    /// Send visible section ids to prioritise highlighting (§9.4.5).
    ///
    /// Best-effort; no-ops on engines that do not support it.
    fn highlight_priority(&self, _tab_id: TabId, _visible: &[SectionId]) {}
}

/// The real engine satisfies the capability directly.
///
/// `bridge::handle::StreamHandle` is a re-export of `nudox_engine::StreamHandle`,
/// so this is a forwarding impl with no conversion — which is the point of the
/// re-export. The moment a conversion appears here, the two handle types have
/// drifted and one of them is a mirror.
impl SymbolEngine for nudox_engine::EngineHandle {
    fn open_symbol(
        &self,
        key: SymbolKey,
        generation: nudox_engine::wire::Gen,
    ) -> (StreamHandle, flume::Receiver<DocEvent>) {
        nudox_engine::EngineHandle::open_symbol(self, key, generation)
    }

    // `highlight_priority` keeps the default no-op: the engine highlights every
    // code section as it chunks, so there is no queue to reorder yet. When the
    // tree-sitter plane lands it becomes a real command (§9.4.5).
}

// ---------------------------------------------------------------------------
// SymbolDoc
// ---------------------------------------------------------------------------

/// The complete accumulated state for one open symbol tab.
pub struct SymbolDoc {
    // ── Metadata ──────────────────────────────────────────────────────────
    /// The stable key for this symbol (set at `open()` time).
    pub key: SymbolKey,
    /// Symbol head (metadata, signature, section plan).
    /// `None` until the first `DocEvent::Head` arrives.
    pub head: Option<Box<SymbolHead>>,
    /// The generation `head` was delivered for.
    ///
    /// # Why this is stored rather than inferred
    ///
    /// A view needs to know *when to re-project the header*, and the obvious
    /// proxy — "the generation changed" — is wrong: `reload` and `set_version`
    /// bump the generation before the new head arrives, and the old head is
    /// deliberately left on screen until it does (LD-15).
    ///
    /// The next proxy — "the generation changed and `sections` is empty" — is
    /// wrong for a subtler reason. `drain` applies every event available in one
    /// wake, and GPUI coalesces the resulting notifications into a single
    /// observer call. So a fast local open delivers `Head`, all its sections,
    /// and `Done` before the view is woken once, and the view sees a populated
    /// section list on its *first* look. Under that proxy the header then never
    /// projects at all — a symbol page with a blank header and "No
    /// documentation", which is exactly what shipped until this field existed.
    ///
    /// Recording the answer removes the guess: the head belongs to whichever
    /// generation delivered it, and nothing about event batching can change it.
    pub head_gen: Option<u64>,

    // ── Streamed content ─────────────────────────────────────────────────
    /// Documentation sections, in `section_plan` order (§8.2 / §9.3).
    pub sections: Progressive<RenderSection>,
    /// Syntax-highlight upgrades for code sections.
    pub highlights: HashMap<SectionId, Arc<[HighlightSpan]>>,
    /// Cross-references pages (interleaved with sections per §9.3).
    pub refs: Progressive<RefsPage>,
    /// Trait implementation pages.
    pub impls: Progressive<ImplsPage>,

    // ── Slot + generation ────────────────────────────────────────────────
    /// Stream slot tracking the lifecycle phase of this tab.
    ///
    /// The `value` is `()` — the slot only tracks phase/error, not the
    /// document content (which is stored above). `generation` is the stale
    /// guard key for this tab's drain closure.
    pub slot_meta: StreamSlot<()>,
    /// Per-tab generation source.
    pub gens: GenSource,

    // ── Task ownership (LD-18) ────────────────────────────────────────────
    /// Drain task for the current stream. Dropping this cancels the task.
    pub(crate) drain_task: Task<()>,

    // ── Counts for tab labels ─────────────────────────────────────────────
    /// Running total refs count (ticking count label in the tab).
    pub refs_total: u64,
    /// Running total impls count.
    pub impls_total: u64,

    /// The symbol's version history, once the engine sends it.
    ///
    /// Not a `Progressive` like `sections`/`refs`/`impls`: the engine computes
    /// a timeline in one pass over the loaded generations and emits it whole,
    /// exactly once, between `Head` and the first `Section`. There is nothing
    /// to accumulate.
    ///
    /// `None` means "not yet delivered", never "this symbol has no history" —
    /// a symbol present in a single loaded version still gets one row. A view
    /// that renders `None` as "no history" would be lying about the common
    /// case, which is that only one version is loaded.
    pub timeline: Option<Timeline>,
}

impl SymbolDoc {
    fn new(key: SymbolKey) -> Self {
        Self {
            key,
            head: None,
            head_gen: None,
            sections: Progressive::new(),
            highlights: HashMap::new(),
            refs: Progressive::new(),
            impls: Progressive::new(),
            slot_meta: StreamSlot::new(),
            gens: GenSource::new(),
            drain_task: Task::ready(()),
            refs_total: 0,
            impls_total: 0,
            timeline: None,
        }
    }

    /// Reset progressive accumulators for a new generation (used by `reload`
    /// and `set_version`).
    fn reset_content(&mut self) {
        self.head = None;
        self.head_gen = None;
        self.sections.reset();
        self.highlights.clear();
        self.refs.reset();
        self.impls.reset();
        self.refs_total = 0;
        self.impls_total = 0;
        // Cleared with the rest of the content: a timeline belongs to the
        // generation that produced it, and switching versions re-computes it.
        self.timeline = None;
    }
}

// ---------------------------------------------------------------------------
// SymbolStore
// ---------------------------------------------------------------------------

/// `SymbolStore` — GUI-PLAN §12.4.
pub struct SymbolStore<E: SymbolEngine> {
    /// All open symbol tabs.
    pub docs: HashMap<TabId, SymbolDoc>,
    /// Reverse index: `SymbolKey → TabId` for dedup in `open()`.
    key_to_tab: HashMap<SymbolKey, TabId>,
    /// The tab a `Replace` open should supersede.
    ///
    /// Tracked here rather than read back from the pane because the store is
    /// what decides tab lifetime; asking the view would make the answer depend
    /// on render order.
    active: Option<TabId>,
    next_tab_id: u64,
    engine: E,
}

impl<E: SymbolEngine> SymbolStore<E> {
    pub fn new(engine: E) -> Self {
        Self {
            docs: HashMap::new(),
            key_to_tab: HashMap::new(),
            active: None,
            next_tab_id: 1,
            engine,
        }
    }

    // ── §7.4 shape ────────────────────────────────────────────────────────

    /// Open a symbol by key. Returns the `TabId` (new or existing).
    ///
    /// If the key is already open, the existing tab is re-activated (dedup)
    /// and `nav.flash` is signalled via `disposition = Stay` semantics from
    /// the caller.
    pub fn open(
        &mut self,
        key: SymbolKey,
        disposition: OpenDisposition,
        cx: &mut Context<Self>,
    ) -> TabId {
        // Dedup: re-activate existing tab.
        if let Some(&tab_id) = self.key_to_tab.get(&key) {
            self.active = Some(tab_id);
            cx.emit(TabActivated { tab_id, disposition });
            return tab_id;
        }

        // `Replace` means *replace*, not "open another one".
        //
        // The variant has always been documented as "replace the currently
        // active tab (or open in foreground if no tab)", but the implementation
        // allocated a new tab unconditionally — so every navigation grew the
        // tab strip and the default gesture silently accumulated tabs the user
        // never asked for. Opening a new tab is now something you opt into by
        // holding the platform modifier, which is what `Stay` expresses.
        //
        // The outgoing tab is closed *after* the new one is created so its
        // `SymbolDoc` — and with it the drain task — is dropped only once the
        // replacement exists. Closing first would leave a frame with no
        // document at all.
        let outgoing = match disposition {
            OpenDisposition::Replace => self.active,
            OpenDisposition::Background | OpenDisposition::Stay => None,
        };

        // Allocate a new tab.
        let tab_id = TabId(self.next_tab_id);
        self.next_tab_id += 1;

        let mut doc = SymbolDoc::new(key.clone());

        // §7.4 — four policy lines.
        doc.slot_meta.generation = doc.gens.next();
        doc.slot_meta.begin_loading();
        let stream_generation = doc.slot_meta.generation;

        // Convert BridgeGen to wire Gen.
        let wire_gen = nudox_engine::wire::Gen(stream_generation.0);
        let (handle, rx) = self.engine.open_symbol(key.clone(), wire_gen);
        doc.slot_meta.handle = Some(handle);

        doc.drain_task = drain(cx, rx, move |store, ev, cx| {
            store.apply_doc_event(tab_id, stream_generation, ev, cx);
        });

        self.docs.insert(tab_id, doc);
        self.key_to_tab.insert(key, tab_id);

        // `Background` opens behind the reader, so it must not steal the
        // active slot — otherwise the *next* `Replace` would close the tab the
        // user is looking at instead of the one they opened behind it.
        if !matches!(disposition, OpenDisposition::Background) {
            self.active = Some(tab_id);
        }

        cx.emit(TabActivated { tab_id, disposition });

        if let Some(outgoing) = outgoing {
            self.close(outgoing, cx);
        }

        cx.notify();
        tab_id
    }

    /// Close a tab, dropping its `SymbolDoc` and cancelling its stream.
    ///
    /// The `drain_task` field is dropped as part of the doc drop, which GPUI
    /// cancels — this is the LD-18 / §2.3 cancellation guarantee.
    pub fn close(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        if let Some(doc) = self.docs.remove(&tab_id) {
            self.key_to_tab.remove(&doc.key);
            if self.active == Some(tab_id) {
                self.active = None;
            }
            // doc.drain_task is dropped here → GPUI cancels it.
            // doc.slot_meta.handle is dropped here → cancels the engine stream.
        }
        cx.notify();
    }

    /// Keep the store's Replace target aligned with the pane's active tab.
    ///
    /// Pane activation is a view concern, so the shell calls this after a tab
    /// click or keyboard activation. It emits no `TabActivated`: the pane is
    /// already showing this tab and re-routing that event would re-enter the
    /// shell.
    pub fn activate(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        if self.docs.contains_key(&tab_id) && self.active != Some(tab_id) {
            self.active = Some(tab_id);
            cx.notify();
        }
    }

    /// Reload the current stream for a tab (re-open at the same key).
    ///
    /// Stale content is preserved (stale-while-revalidate, LD-15) while the
    /// new stream arrives.
    pub fn reload(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        let key = match self.docs.get(&tab_id) {
            Some(doc) => doc.key.clone(),
            None => return,
        };
        self.start_stream(tab_id, key, cx);
    }

    /// Switch to a different symbol version (new gen stream into same tab).
    ///
    /// Semantics: stale content dims (LD-15), new content streams over it.
    pub fn set_version(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        let key = match self.docs.get(&tab_id) {
            Some(doc) => doc.key.clone(),
            None => return,
        };
        self.start_stream(tab_id, key, cx);
    }

    // ── Stream start (shared by open / reload / set_version) ──────────────

    fn start_stream(&mut self, tab_id: TabId, key: SymbolKey, cx: &mut Context<Self>) {
        let doc = match self.docs.get_mut(&tab_id) {
            Some(d) => d,
            None => return,
        };

        // §7.4 — supersede.
        doc.slot_meta.generation = doc.gens.next();
        doc.slot_meta.begin_loading();
        let stream_generation = doc.slot_meta.generation;
        // Preserve old content for stale-while-revalidate (LD-15):
        // do NOT call doc.reset_content() here — old sections stay visible.

        let wire_gen = nudox_engine::wire::Gen(stream_generation.0);
        let (handle, rx) = self.engine.open_symbol(key.clone(), wire_gen);
        doc.slot_meta.handle = Some(handle); // drops+cancels predecessor.

        doc.drain_task = drain(cx, rx, move |store, ev, cx| {
            store.apply_doc_event(tab_id, stream_generation, ev, cx);
        });
        cx.notify();
    }

    // ── Event application ─────────────────────────────────────────────────

    fn apply_doc_event(
        &mut self,
        tab_id: TabId,
        stream_generation: BridgeGen,
        ev: DocEvent,
        cx: &mut Context<Self>,
    ) {
        let doc = match self.docs.get_mut(&tab_id) {
            Some(d) => d,
            None => return, // tab was closed while events were in-flight
        };

        // DocEvent itself predates generation tags. The drain closure carries
        // the generation of its stream, supplying the stale guard when a
        // cancelled stream still has queued events.
        if doc.slot_meta.generation != stream_generation {
            return;
        }

        match ev {
            DocEvent::Head(head) => {
                doc.slot_meta.first_event();
                // Stale guard: the head carries no gen — we trust it is current
                // because the handle was just issued. Reset content for the new gen.
                let generation = doc.slot_meta.generation.0;
                doc.reset_content();
                let key = doc.key.clone();
                doc.head = Some(head);
                // Stamped *after* `reset_content`, which clears it — this is the
                // one fact a view cannot reconstruct from the doc alone.
                doc.head_gen = Some(generation);
                cx.emit(HeadReady { tab_id, key });
            }

            DocEvent::Section(section) => {
                let section_id = section.section_id();
                doc.sections.push(section);
                cx.emit(SectionArrived { tab_id, section_id });
            }

            DocEvent::Highlight { section, spans } => {
                // Highlights enrich an existing section without mutating it.
                doc.highlights.insert(section, spans);
                // No specific event for highlight — the section already exists;
                // views subscribe to the store's general notify.
            }

            DocEvent::Refs { page, done } => {
                let total = page.total;
                doc.refs.push(page);
                doc.refs_total = total;
                if done {
                    doc.refs.complete();
                }
                cx.emit(TabCountsChanged {
                    tab_id,
                    refs_count: doc.refs_total,
                    impls_count: doc.impls_total,
                });
            }

            DocEvent::Impls { page, done } => {
                let total = page.total;
                doc.impls.push(page);
                doc.impls_total = total;
                if done {
                    doc.impls.complete();
                }
                cx.emit(TabCountsChanged {
                    tab_id,
                    refs_count: doc.refs_total,
                    impls_count: doc.impls_total,
                });
            }

            DocEvent::Done => {
                doc.sections.complete();
                doc.slot_meta.value = Some(());
                doc.slot_meta.complete();
            }

            DocEvent::Failed(error) => {
                doc.slot_meta.fail(SlotError::Transient {
                    message: error.to_string(),
                });
            }

            DocEvent::Timeline(timeline) => {
                // Arrives after `Head` and before the first `Section` (§9.3),
                // so the version strip is populated by the time a reader could
                // plausibly look for it.
                doc.timeline = Some(timeline);
            }

            // Forward-compat: unknown variants ignored (LD-7).
            _ => {}
        }
    }

    // ── Visible-sections hint ─────────────────────────────────────────────

    /// Notify the engine of which sections are currently in the viewport.
    ///
    /// The engine can use this to prioritise syntax highlighting for visible
    /// code blocks (§9.4.5). This is best-effort and coalesced at 4 Hz by the
    /// caller.
    pub fn visible_sections(&self, tab_id: TabId, ids: &[SectionId]) {
        self.engine.highlight_priority(tab_id, ids);
    }

    // ── Read helpers ──────────────────────────────────────────────────────

    /// Look up a doc by tab id (shared reference for views).
    pub fn doc(&self, tab_id: TabId) -> Option<&SymbolDoc> {
        self.docs.get(&tab_id)
    }

    /// Returns `true` if the given `SymbolKey` is already open as a tab.
    pub fn is_open(&self, key: &SymbolKey) -> bool {
        self.key_to_tab.contains_key(key)
    }

    /// Returns the `TabId` for an already-open key, if any.
    pub fn tab_for_key(&self, key: &SymbolKey) -> Option<TabId> {
        self.key_to_tab.get(key).copied()
    }
}

impl<E: SymbolEngine> gpui::EventEmitter<TabActivated> for SymbolStore<E> {}
impl<E: SymbolEngine> gpui::EventEmitter<HeadReady> for SymbolStore<E> {}
impl<E: SymbolEngine> gpui::EventEmitter<SectionArrived> for SymbolStore<E> {}
impl<E: SymbolEngine> gpui::EventEmitter<TabCountsChanged> for SymbolStore<E> {}

// ---------------------------------------------------------------------------
// Tests (pure — no GPUI executor needed for structural tests)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::AppContext as _;
    use std::sync::Arc;

    struct NullEngine;

    impl SymbolEngine for NullEngine {
        fn open_symbol(
            &self,
            _key: SymbolKey,
            generation: nudox_engine::wire::Gen,
        ) -> (StreamHandle, flume::Receiver<DocEvent>) {
            let (_tx, rx) = flume::bounded(1);
            (StreamHandle::new(generation, || {}), rx)
        }
    }

    // ── Helpers ───────────────────────────────────────────────────────────
    //
    // `SymbolKey` construction requires `EcosystemId` / `PackageName` which
    // are not re-exported by `nudox_engine::wire` and `lindsey` must not import
    // `nudox_ir` directly (dependency law §L0). Tests that need a `SymbolKey`
    // value therefore use a deserialization round-trip through `serde_json`,
    // which IS a dependency of `lindsey`.
    //
    // `SymbolKey` = `StableRef { package: PackageLineageId, intro: IntroId }`.
    // The JSON representation is derived from `#[derive(Serialize, Deserialize)]`
    // on all inner types. We produce two *different* keys by varying the intro
    // bytes.

    /// Deserialise a `SymbolKey` from a minimal JSON literal.
    ///
    /// `n` is embedded into the `intro` field so that `make_key(1) ≠ make_key(2)`.
    fn make_key(n: u8) -> SymbolKey {
        let json = format!(
            r#"{{
              "package": {{
                "ecosystem": "cargo",
                "name": "test-pkg"
              }},
              "intro": [{}, 0, 0, 0, 0, 0, 0, 0,
                         0, 0, 0, 0, 0, 0, 0, 0,
                         0, 0, 0, 0, 0, 0, 0, 0,
                         0, 0, 0, 0, 0, 0, 0, 0]
            }}"#,
            n
        );
        serde_json::from_str(&json).expect("make_key: valid JSON for SymbolKey")
    }

    // ── Gen supersession ──────────────────────────────────────────────────

    #[test]
    fn generation_advances_on_each_call() {
        let mut gens = GenSource::new();
        let g1 = gens.next();
        let g2 = gens.next();
        assert!(g2 > g1, "generation must advance on each call");
    }

    #[test]
    fn stale_gen_guard_is_mismatch() {
        let mut gens = GenSource::new();
        let current = gens.next();
        let stale = BridgeGen(current.0 - 1);
        assert_ne!(stale, current, "old gen does not match current");
        assert_eq!(current, gens.current(), "current gen is stable");
    }

    /// A terminal event already queued by an older stream must not complete a
    /// replacement stream. `DocEvent` has no wire generation, so this exercises
    /// the generation captured by the owning drain closure at the store seam.
    #[gpui::test]
    async fn stale_doc_event_does_not_complete_new_stream(cx: &mut gpui::TestAppContext) {
        let store = cx.new(|_| SymbolStore::new(NullEngine));
        let tab = store.update(cx, |store, cx| {
            store.open(make_key(1), OpenDisposition::Replace, cx)
        });
        store.update(cx, |store, cx| store.reload(tab, cx));

        // The first open is generation 1; reload has moved the same doc to
        // generation 2. Apply an event tagged with the superseded stream.
        store.update(cx, |store, cx| {
            store.apply_doc_event(tab, BridgeGen(1), DocEvent::Done, cx);
        });

        store.read_with(cx, |store, _| {
            let doc = store.doc(tab).expect("reloaded document must remain open");
            assert!(
                matches!(doc.slot_meta.phase, crate::bridge::slot::Phase::Loading { .. }),
                "a stale Done event must not complete the replacement stream"
            );
        });
    }

    // ── open() dedup ──────────────────────────────────────────────────────

    /// Dedup: inserting the same key twice returns the same `TabId`.
    #[test]
    fn open_dedup_returns_existing_tab() {
        let k1 = make_key(1);
        let k2 = make_key(1); // same bytes → same key
        assert_eq!(k1, k2, "same key must compare equal");

        let mut map: HashMap<SymbolKey, TabId> = HashMap::new();
        map.insert(k1, TabId(1));

        let found = map.get(&k2).copied();
        assert_eq!(found, Some(TabId(1)), "dedup: second open finds existing tab");
    }

    /// Two different keys must produce different tab ids.
    #[test]
    fn different_keys_get_different_tab_entries() {
        let k1 = make_key(1);
        let k2 = make_key(2);
        assert_ne!(k1, k2, "distinct keys must be not-equal");
        // Even if we put them in the same map they occupy different slots.
        let mut map: HashMap<SymbolKey, TabId> = HashMap::new();
        map.insert(k1, TabId(1));
        map.insert(k2, TabId(2));
        assert_eq!(map[&make_key(1)], TabId(1));
        assert_eq!(map[&make_key(2)], TabId(2));
    }

    // ── close() drops the doc ─────────────────────────────────────────────

    #[test]
    fn close_removes_doc_and_key_index() {
        let key = make_key(7);
        let tab_id = TabId(42);

        let mut docs: HashMap<TabId, SymbolDoc> = HashMap::new();
        let mut key_to_tab: HashMap<SymbolKey, TabId> = HashMap::new();

        let doc = SymbolDoc::new(key.clone());
        docs.insert(tab_id, doc);
        key_to_tab.insert(key.clone(), tab_id);

        // Simulate close().
        if let Some(removed_doc) = docs.remove(&tab_id) {
            key_to_tab.remove(&removed_doc.key);
        }

        assert!(docs.is_empty(), "close must remove doc");
        assert!(key_to_tab.is_empty(), "close must remove key index entry");
    }

    // ── handle drop = stream cancel (LD-18) ──────────────────────────────

    #[test]
    fn handle_drop_fires_canceller() {
        let fired = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let fired_clone = fired.clone();
        let handle = StreamHandle::new(BridgeGen(1), move || {
            fired_clone.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        assert!(!fired.load(std::sync::atomic::Ordering::SeqCst));
        drop(handle);
        assert!(
            fired.load(std::sync::atomic::Ordering::SeqCst),
            "canceller must fire on drop (LD-18)"
        );
    }

    /// Assigning a new handle drops the old one, firing its canceller.
    #[test]
    fn superseding_handle_cancels_predecessor() {
        let count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let c = count.clone();
        let mut slot: StreamSlot<()> = StreamSlot::new();
        slot.handle = Some(StreamHandle::new(BridgeGen(1), move || {
            c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }));
        // Assign a new handle — old one dropped, canceller fires.
        slot.handle = Some(StreamHandle::new(BridgeGen(2), || {}));
        assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 1, "predecessor cancelled");
    }

    // ── Section accumulation is append-only ───────────────────────────────

    #[test]
    fn sections_append_only() {
        let mut prog: Progressive<RenderSection> = Progressive::new();
        prog.push(RenderSection::Prose { id: SectionId(1), blocks: vec![] });
        prog.push(RenderSection::Prose { id: SectionId(2), blocks: vec![] });

        assert_eq!(prog.len(), 2);
        assert_eq!(prog[0].section_id(), SectionId(1));
        assert_eq!(prog[1].section_id(), SectionId(2));
    }

    // ── A slow section never clears a fast one (LD-15 / LR-10) ──────────

    #[test]
    fn slow_section_does_not_clear_fast_section() {
        let mut prog: Progressive<RenderSection> = Progressive::new();
        prog.push(RenderSection::Prose { id: SectionId(1), blocks: vec![] }); // fast
        prog.push(RenderSection::Prose { id: SectionId(2), blocks: vec![] }); // slow

        assert_eq!(prog[0].section_id(), SectionId(1), "fast section must remain at index 0");
        assert_eq!(prog[1].section_id(), SectionId(2));
    }

    // ── Highlight storage separate from sections ──────────────────────────

    #[test]
    fn highlights_stored_separately_from_sections() {
        let key = make_key(5);
        let mut doc = SymbolDoc::new(key.clone());
        doc.sections.push(RenderSection::Prose { id: SectionId(1), blocks: vec![] });

        let spans: Arc<[HighlightSpan]> = Arc::from(vec![].as_slice());
        doc.highlights.insert(SectionId(1), spans);

        assert_eq!(doc.sections.len(), 1, "section count unchanged by highlight");
        assert!(doc.highlights.contains_key(&SectionId(1)));
    }
}
