# R3 — GPUI reference-app pattern catalogue

Sources (read-only study, no cargo/build run):
- **hummingbird** — `~/Downloads/hummingbird` — a GPUI music player. 244 `.rs` files under `src/`.
- **zeron** — `~/Downloads/zeron` — a GPUI multi-device coding-agent controller. The big UI crate is `crates/ui/src` (huge files: `shell.rs` ~13.4k lines, `transcript.rs` ~9.7k, `motion.rs` ~1.2k, `pickers.rs` ~5.5k). `ARCHITECTURE.md` and `crates/proto/src/view.rs` are the orientation docs.

**Naming correction, read this first:** the task brief assumed `crates/harness` and `crates/preview` are zeron's UI screenshot/testing harness. They are not — in this checkout `crates/harness` is the AI-coding-agent subprocess driver (Claude Code/Codex/Cursor JSON-RPC clients) and `crates/preview` is a WebRTC-style device-relay/signaling crate for remote control. Neither touches GPUI rendering. The actual visual-testing/screenshot machinery lives at `crates/ui/src/appshots.rs` (+ `appshots/macos.rs`, `appshots/shortcut.rs` — which is itself a **product feature**, not a test tool: user-triggered screenshots of *other* apps attached to agent prompts) and, more importantly, `crates/ui/examples/*-fixture.rs` plus 100+ `#[gpui::test]` unit tests inside `crates/ui/src/*.rs`. Section 10 below covers the real mechanism.

Verdict key: **ADOPT** = copy near-verbatim into the docs reader. **ADAPT** = same idea, different shape needed. **IGNORE** = not relevant to a docs reader.

---

## 1. App/state architecture

### hummingbird: many small `Global`s, field-level `Entity<T>` granularity

Hummingbird does **not** have one root app-state struct. It registers several `Global`s (`Models`, `PlaybackInfo`, theme, settings, etc.) via `cx.set_global`/`impl Global for X`, and — critically — the hot, independently-changing fields of playback state are each their **own** `Entity`, not fields of a plain struct:

```rust
// src/ui/models.rs:143-157
pub struct PlaybackInfo {
    pub position: Entity<u64>,
    pub duration: Entity<u64>,
    pub playback_state: Entity<PlaybackState>,
    pub current_track: Entity<Option<CurrentTrack>>,
    pub shuffling: Entity<bool>,
    pub repeating: Entity<RepeatState>,
    pub stop_after_current: Entity<bool>,
    pub volume: Entity<f64>,
    pub prev_volume: Entity<f64>,
    pub sample_rate: Entity<u32>,
}
impl Global for PlaybackInfo {}
```

A view that renders the seek bar can `cx.observe(&playback_info.position, ...)` and only that view re-renders on the ~10Hz position tick; a view rendering the track title observes `current_track` and never re-renders on position ticks. This is the mechanism by which hummingbird avoids over-rendering: granularity is pushed down to the **field**, not the screen. There are 10 separate `impl Global for X` types in total (`Models`, `PlaybackInfo`, settings, theme, scan interface, playback interface, power manager, spectrum analyzer, OS media-key controller, ...) — each cross-cutting subsystem is its own global rather than one shared root. Bridging a non-GPUI async source (a `tokio::sync::watch::Receiver`) into this world still follows the same rule — fold into an entity, then `cx.notify()`:
```rust
// src/ui/models.rs:356-366
cx.spawn(async move |cx| {
    while discord_status_rx.changed().await.is_ok() {
        let status = discord_status_rx.borrow_and_update().clone();
        discord_rpc_model.update(cx, |current, cx| { *current = status; cx.notify(); });
    }
}).detach();
```

Routing/navigation is a genuine hand-rolled router, not just `.when()` flags — worth studying in full for a docs reader (sidebar tree → document pane navigation is structurally the same problem). `src/ui/library.rs` keeps a `NavigationHistory` (back/forward stack, capped at 100 entries, oldest non-"key" page evicted first) and a `switcher_model: Entity<NavigationHistory>` that acts as the router's event bus:
```rust
// src/ui/library.rs:260-283 — "current page" is a materialized enum holding the live child Entity, not a route id
enum LibraryView {
    Album(Entity<AlbumView>), Tracks(Entity<TrackView>), Release(Entity<ReleaseView>),
    Playlist(Entity<PlaylistView>), Artists(Entity<ArtistView>),
    ArtistDetail(Entity<ArtistDetailView>), Files(Entity<FilesView>),
}
```
`Library::new` (`src/ui/library.rs:390-513`) subscribes to `switcher_model` and that `cx.subscribe` closure **is** the router: on every navigation message it first saves the outgoing view's scroll offset into a `ScrollStateStorage`, mutates the history stack, then calls a single dispatch point `make_view()` (`library.rs:342-378`) that constructs a **brand-new** child entity for the destination — there is no cross-page component reuse, only scroll position survives the round trip (threaded back into the freshly built view). A separate `LibrarySection` enum tracks which top-level tab the user is in, independent of the detail-page stack, so a master/detail split-view is *derived* from the history rather than modeled as its own state. No separate OS `Window`s are used for in-app navigation — only Settings and the Equalizer open real second windows.

Elsewhere, coordination between global entities still uses `cx.observe`/`cx.subscribe` at setup time, e.g. re-deriving the active theme when settings change (`src/ui/theme.rs:992` `cx.observe(&settings_model, move |_, cx| {...})`), and the top-level shell renders a fixed layout, conditionally mounting modal children with `.when(bool, ...)`:

```rust
// src/ui/app.rs:164-175
.when(show_about, |this| { this.child(about_dialog(...)) })
.when(show_missing_folder_dialog, |this| { this.child(self.missing_folder_dialog.clone()) })
.when(show_corrupt_settings_dialog, |this| { this.child(self.corrupt_settings_dialog.clone()) })
.child(self.toast_layer.clone()),
```
Library sub-pages (`src/ui/library/*_view.rs`) are switched by an enum-tagged "current view" read out of a global, not separate `Window`s.

### zeron: one immutable-ish `Entity<AppState>` + a pure "view" derivation layer

zeron's whole shell renders from a single top-level entity, explicitly documented as such:

```rust
// crates/ui/src/state.rs:1-16 (doc comment)
//! App state: the engine connection, entity lists, and the selected chat's
//! transcript — one gpui [`Entity`] the whole shell renders from.
//! ...
//! Once an [`RpcClient`] exists, its `call`/`subscribe` futures are
//! runtime-agnostic (tokio channels), so subscription pumps run on gpui's own
//! executor via `cx.spawn` and fold each frame into the entity with
//! `this.update(...)` + `cx.notify()`.
//! Pure logic (sort order, staleness, gate phase) lives in free functions
//! with unit tests; rendering reads them.
```

The "pure logic" half is its own crate module, `zeron-proto::view`, explicitly designed so every consuming frontend derives identical results from the same immutable snapshot instead of recomputing/duplicating it:

```rust
// crates/proto/src/view.rs:1-16
//! Frontend-agnostic view logic: the derivations every viewport needs and
//! none of them should own — sort orders, staleness gating, sidebar
//! grouping, the boot gate, relative times.
//! This lives in `proto` rather than in the viewport crate so the rules stay
//! pure and independently testable: the same workspace doc must produce the
//! same row order on every surface...
//! Everything in this module is pure.
pub const SESSION_STALE_MS: i64 = 45_000;
pub fn effective_indicator(session: Option<&Session>, now: DateTime<Utc>) -> Indicator { ... }
```

Underneath, the actual source of truth is a Loro CRDT doc; `zeron-doc`'s "mirror" applies incremental diffs into typed structs without full re-hydration per change (per `ARCHITECTURE.md` §2.3), and `AppState` (a gpui `Entity`) is the UI-thread projection of that mirror, updated by folding subscription frames with `cx.notify()`.

Two more mechanisms sharpen the "one entity, avoid over-render" story:

**(a) Typed-event narrowcasting on top of the same entity.** Coarse observers use `cx.observe` (react to *any* change); anything that would otherwise re-run on every unrelated mutation instead `cx.subscribe`s to a specific `EventEmitter` variant:
```rust
// crates/ui/src/transcript.rs:3007-3019
let observe = cx.observe(&state, |this: &mut Self, _, cx| this.sync(cx));
let text_changes = cx.subscribe(&state, |this: &mut Self, state, event: &crate::state::TranscriptTextChanged, cx| {
    let doc_id = this.doc_override.as_deref().or_else(|| state.read(cx).selected_chat.as_deref());
    if doc_id == Some(event.doc_id.as_str()) { this.sync(cx); }
});
```
`AppState` emits `TranscriptTextChanged { doc_id }` selectively for streaming-token appends (`state.rs:1242-1264`) so only the transcript instance actually displaying that `doc_id` re-syncs.

**(b) Dirty-flag gating *before* `cx.notify()` is even called** — a memoized "did this actually change" comparison so a heartbeat that only bumps a timestamp never triggers a repaint:
```rust
// crates/ui/src/state.rs:978-999 (elided)
fn apply_sessions_at(&mut self, sessions: Vec<Session>, now: DateTime<Utc>) -> bool {
    let presentation: Vec<_> = sessions.iter().map(|s| { let mut m = s.clone(); m.updated_at = DateTime::<Utc>::UNIX_EPOCH; m }).collect();
    let changed = self.session_presentation.as_ref() != Some(&presentation) || self.session_presence_presentation != presence;
    self.session_presentation = Some(presentation);
    self.sessions = sessions;
    changed // caller only calls cx.notify() when this is true
}
```
wired through a generic `spawn_watch<T>` driver (`state.rs:2410-2468`) shared by every RPC-pushed collection (sessions, devices, sidebar prefs).

One caveat worth naming: entity granularity is **per functional surface** (transcript, composer, each diff view, each file surface, each terminal — each its own `Entity` with its own `Subscription`), not per-row — the sidebar's row list is deliberately *not* entity-ized; `render_active_rows` (`shell/spaces.rs:3611-3736`) recomputes a plain `Vec<ActiveChatRow>` fresh on every `Shell` render pass, riding the same coarse observer as everything else in the shell. Only the transcript and file-tree lists are virtualized `Entity`+`ListState` pairs (§3).

### Verdict
- **ADOPT (zeron's structure)**: one root `Entity<AppState>` holding an immutable-per-frame snapshot, with all derived/sorted/filtered views computed by **pure, independently unit-tested functions** in a shared module (their `proto::view`) rather than inline in render methods. This maps directly onto "single immutable app snapshot" and gives you free testability without booting GPUI.
- **ADOPT**: typed-event narrowcasting (`cx.subscribe` to a specific `EventEmitter` payload like `TranscriptTextChanged{doc_id}`) for any hot, frequent mutation (streaming tokens, live scroll position) so only the one subtree that cares re-syncs, while everything else stays on a coarse `cx.observe`. Pair it with dirty-flag gating (`apply_sessions_at`'s memoized-comparison-before-`cx.notify()` pattern) for any RPC-pushed collection that includes noisy fields (timestamps, heartbeats) irrelevant to rendering.
- **ADAPT (hummingbird's field-level `Entity` granularity)**: a single snapshot entity risks over-rendering if the whole app re-renders on every doc-tree mutation. Adapt by giving expensive/hot subtrees (e.g., "current scroll position," "search-highlight state," "TOC active heading") their own child `Entity`s that observe slices of the snapshot, so a scroll-position tick doesn't invalidate the whole document body.
- **ADOPT**: hummingbird's `cx.subscribe`-closure-as-router pattern — one event bus entity (`switcher_model`), one dispatch point (`make_view`) that always constructs a fresh child entity for the destination, with only *derived/cheap* state (scroll offset) explicitly threaded through. This is a clean shape for a docs reader's page/section navigation even under a single-snapshot model: the snapshot changes "current doc path," a subscriber on that path builds the new document view.
- **IGNORE**: hummingbird's many-independent-`Global`s style as the *storage* strategy — it doesn't fit an immutable-snapshot model and gets unwieldy as global count grows (settings, theme, models, MMB services, etc. all separately globals). Take the router idea, not the global-soup storage.

---

## 2. Async

### hummingbird: tokio mpsc for backend pipelines, `cx.spawn` + `cx.background_executor` for UI-thread bridging, viewer-gated frame rate

The library scanner is pure tokio, off GPUI entirely (`src/library/scan/execution.rs:13` imports `mpsc::{Receiver, UnboundedSender, WeakSender}`; `execution.rs:356` spawns a periodic checkpoint task with `tokio::spawn`). The UI-facing async work goes through `cx.spawn`. The clearest example is the equalizer's background spectrum analyzer, which also demonstrates **audience-gated polling** — it halves/quarters its own tick rate when no view is looking:

```rust
// src/ui/equalizer/spectrum.rs:73-113 (elided)
cx.spawn({
    async move |cx| {
        let mut analyzer = Analyzer::new(tap);
        loop {
            let frame_ms = if viewers.load(Ordering::Relaxed) == 0 { PARKED_FRAME_MS } else { FRAME_MS };
            cx.background_executor().timer(Duration::from_millis(frame_ms)).await;
            let frame = if analyzer.rings_have_data() {
                let (analyzer_back, frame) = cx.background_executor()
                    .spawn(async move { let frame = analyzer.tick(rate, frame_ms); (analyzer, frame) })
                    .await;
                analyzer = analyzer_back; frame
            } else { analyzer.tick(rate, frame_ms) };
            let Some((pre, post, clipping)) = frame else { continue };
            data.update(cx, |data, cx| { data.pre = Rc::new(pre); data.post = Rc::new(post); cx.notify(); });
        }
    }
}).detach();
```
Constants: `FRAME_MS = 33` (30fps cap while a viewer is open), `PARKED_FRAME_MS = 250` (parked when nobody's watching). CPU-heavy work (the FFT `analyzer.tick`) is explicitly hopped to `cx.background_executor().spawn(...)` (a thread-pool future), separate from the timer loop itself — so the async loop's own await points stay cheap. Results land back on the UI thread via `data.update(cx, |data, cx| { ...; cx.notify() })`, the standard GPUI apply-on-entity pattern. Toasts use the same drain-a-channel-into-an-entity shape, but with a tokio `UnboundedReceiver` instead of a raw ring buffer (`src/ui/toasts.rs:33-38`: `cx.spawn(async move |this, cx| { while let Some(toast) = receiver.recv().await { this.update(cx, |layer, cx| layer.push(toast, cx)).ok(); } })`).

Hummingbird runs **two separate executors for two separate jobs**: a dedicated, lazily-built multi-thread Tokio runtime, distinct from GPUI's own, for blocking/IO work (DB queries, image decode, directory walks):
```rust
// src/main.rs:47-54
static RUNTIME: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread().enable_all()
        .worker_threads(2).max_blocking_threads(12).build().unwrap()
});
```
while GPUI's own `cx.background_executor()` handles lightweight timers/polling that end in a `cx.spawn` UI update. A genuinely nice low-latency trick sits at the boundary between the two: background work writes into an `Arc<OnceLock<T>>` "bridge" that is polled **both** by the eventual `.update()` callback *and* directly at paint time, so a decoded image can appear on the very next frame even before the full `cx.spawn → entity.update → cx.notify` round trip lands:
```rust
// src/ui/components/managed_image.rs:117-129 (elided)
let handle = crate::RUNTIME.spawn(async move {
    let result = key.retrieve(pool, thumb).await;
    bridge_clone.set(result.clone().ok().flatten()).ok();
    result
});
cx.spawn(async move |this, cx| { let result = handle.await.unwrap(); /* ...entity.update... */ })
```
`request_layout` checks the bridge directly before falling back to the slower entity-update path (`managed_image.rs:237-255`); the same idiom backs lazy directory listing (`files_view/loader.rs:17`, `DirBridge`).

**Debounce/coalescing**, three different mechanisms depending on the source: (1) search-as-you-type uses a **notify-channel + drain-all-then-tick** loop rather than a fixed delay — keystrokes call `sender.try_send(())` (coalescing, drops if a notification is already pending), and the poll loop drains every pending notification before doing one match pass (`src/ui/components/palette/finder.rs:120-173`); (2) filesystem-watch coalescing for the library scanner uses the `notify-debouncer-full` crate directly with a 2s window (`src/library/scan/watch.rs:24,39-43`); (3) rescan *requests* (not raw FS events) are merged structurally into a pending struct rather than time-windowed:
```rust
// src/library/scan/control.rs:167-190 (elided)
match pending {
    Some(merged) => { merged.paths.extend(paths); merged.respect_record &= respect_record; merged.recursive |= recursive; }
    None => *pending = Some(PendingRescan { paths, respect_record, recursive }),
}
```
Equalizer edits apply synchronously to the model on every drag tick but debounce the **disk write** on the trailing edge by replacing a stored `Task` each time — see cancellation below.

**Cancellation of stale work:** no generation-counter/`AbortHandle` scheme anywhere. Cancellation is purely structural: a `Task<()>` returned by `cx.spawn(...)` and stored (not `.detach()`-ed) in a struct field is dropped — and therefore cancelled — the instant a new one replaces it in that field:
```rust
// src/ui/equalizer/view.rs:238-267 (elided) — trailing-edge debounced save
self.save_task = Some(cx.spawn(async move |this, cx| {
    cx.background_executor().timer(Duration::from_millis(500)).await;
    this.update(cx, |this, cx| { this.settings.update(cx, |s, cx| save_settings(cx, s)); }).ok();
}));
```
The same shape debounces a 3-second "reset armed" window (`equalizer/view.rs:206-224`). The other half of "cancellation" is simply that `WeakEntity::update` returns `Err` once the entity is gone, so stale background work becomes a silent no-op instead of needing an explicit abort.

**Progress reporting:** scan progress is a `tokio::sync::mpsc::UnboundedReceiver<ScanEvent>` drained by a `cx.spawn` loop that writes straight into a `scan_state` entity (`src/library/scan/control.rs:132-156`), the same "background channel → drain loop → `entity.update` + `cx.notify()`" shape used everywhere else in this section.

### zeron: tokio-backed RPC, `cx.spawn` fold-into-entity, and deterministic-clock-aware timers

`state.rs`'s doc comment (quoted in §1) states the bridging rule directly: subscription pumps run on gpui's executor via `cx.spawn`, each frame folded with `this.update(...)` + `cx.notify()`. One extra wrinkle worth stealing: zeron's own animation clock (`motion.rs`, see §4) checks whether it's running under gpui's **test** scheduler before using a platform-precise timer source, so background timer loops behave deterministically under `#[gpui::test]`:

```rust
// crates/ui/src/motion.rs:143-152
let mut precise_clock = if cx.background_executor().scheduler_executor().scheduler().as_test().is_none() {
    windows_pulse::Clock::new(PULSE_TICK)
} else {
    // A deterministic scheduler must own its timers and wakeups; an OS
    // thread cannot schedule its thread-local tasks safely.
    None
};
```
This "detect the test scheduler, don't fight it" rule is the load-bearing trick that lets 100+ `#[gpui::test]`s exercise real debounce/animation code paths deterministically (see §10).

zeron has no `AbortHandle`/`CancellationToken` anywhere in its UI crate either — it relies on the same "drop the `Task`" idiom as hummingbird, reinforced by an explicit **generation counter** compared when a result lands (belt-and-suspenders, since the `Task` should already be dead):
```rust
// crates/ui/src/files/mod.rs:820-902 (elided)
let generation = self.tree.generation();
let task = cx.spawn(async move |this, cx| {
    let result = client.list_directory_snapshot(request.clone(), &cached_paths).await;
    this.update(cx, |surface, cx| {
        if surface.tree.generation() != generation { return; } // a newer request already landed or superseded this one
        /* apply result, cx.notify() */
    });
});
```
For file-watch coalescing specifically, zeron skips time-debouncing entirely and instead detects a **sequence gap** to force a full resync rather than trust an out-of-order/dropped event:
```rust
// crates/ui/src/files/watch.rs:152-154
pub(super) fn sequence_needs_resync(previous: Option<u64>, next: u64) -> bool {
    previous.is_some_and(|previous| next != previous.saturating_add(1))
}
```

### Verdict
- **ADOPT**: `cx.spawn` async loop + `cx.background_executor().spawn(...)` for CPU-bound work + `entity.update(cx, |s, cx| { ...; cx.notify() })` to land results — this is the correct, idiomatic GPUI shape for background doc loading/search-indexing/markdown parsing in a docs reader.
- **ADOPT**: viewer-gated tick rate (hummingbird's `viewers.load(Ordering::Relaxed) == 0 → slower poll`) for anything that polls in the background while its view may not be mounted (e.g., file-watcher-driven doc reload).
- **ADOPT**: check `scheduler().as_test().is_none()` before wiring any platform-precise timer, specifically so background timer loops stay controllable via `cx.executor().advance_clock()` in tests (see §10) instead of becoming untestable wall-clock code.
- **ADAPT**: hummingbird's tokio-mpsc-for-heavy-pipelines / GPUI-executor-for-UI-glue split is the right instinct for a docs reader too (e.g., tokio for filesystem watching + markdown/search indexing, GPUI executor only for the "apply result to entity" hop).
- **ADOPT**: hummingbird's "dropped `Task` = cancellation" idiom (store the `Task` in a struct field, overwrite it to cancel the previous one) for any debounced write/save — simpler than a generation counter or `AbortHandle` and idiomatic GPUI.
- **ADOPT**: the notify-channel-coalesce-then-drain-all loop (`try_send` + `while try_recv().is_ok() {}` before one recompute pass) for search-as-you-type or any burst-prone input, and hummingbird's `Arc<OnceLock<T>>` background→paint "bridge" for shaving a frame of latency off any async result that's cheap to poll speculatively (e.g. a doc's rendered-markdown cache) — both apply directly to a docs reader's live search and incremental-render paths.
- **ADOPT**: zeron's generation-counter-checked-on-landing pattern for any request whose response can arrive after a newer request already superseded it (directory/file loads, search) — belt-and-suspenders on top of "drop the `Task`," cheap insurance against a `Task` that was queued but not yet polled at cancellation time.
- **ADAPT**: sequence-gap-triggers-full-resync (zeron's file watcher) is the right idea specifically for any subscription over an ordered stream (e.g. a live-reload watch on the currently open doc) where a dropped event must not silently desync the view — a docs reader's file-watcher-driven live reload should use the same check rather than a fixed debounce window.

---

## 3. Lists

### hummingbird: `uniform_list`/`uniform_grid` for fixed-height rows, `ListState` only for the variable-height command-palette

Library/queue/playlist views use `uniform_list` (fixed row height, no per-item measurement) almost everywhere: `src/ui/queue.rs:1034` (`uniform_list("queue", queue_len, move |range, _, cx| {...})`), `src/ui/library/files_view.rs:532`, `src/ui/library/playlist_view.rs:895`, `src/ui/components/table.rs:625` (backs the sortable track table), plus a dedicated `uniform_grid` component (`src/ui/components/uniform_grid.rs:406`) for the album-art grid (`src/ui/library/artist_detail_view.rs:732`). The one place needing variable-height rows (the fuzzy-finder / command palette, where result rows can carry a subtitle) drops to the lower-level, per-item-measured `ListState`:
```rust
// src/ui/components/palette/finder.rs:465-468
fn make_list_state(total_count: Option<usize>) -> ListState {
    match total_count {
        Some(count) => ListState::new(count, ListAlignment::Top, px(300.0)),
        None => ListState::new(0, ListAlignment::Top, px(64.0)),
    }
}
```

Row-reuse inside `uniform_list` is a small, explicit utility shared by every table/finder view — a per-index cache of child `Entity`s, get-or-inserted each render and pruned when a new render cycle's index range no longer covers an old one:
```rust
// src/ui/util.rs:14-53 (elided) — prune_views: evict cached rows outside the currently-rendered range
pub fn prune_views<T: Render>(views_model: &Entity<FxHashMap<usize, Entity<T>>>, render_counter: &Entity<usize>, current: usize, cx: &mut App) -> bool { ... }
```
used inside the virtualized closure itself: `div().w_full().child(create_or_retrieve_view(&views_model, idx, |cx| TableItem::new(...), cx))` (`src/ui/components/table.rs:625-658`). Scroll-position restore threads through the router (§1): each page type stashes its offset into a `ScrollStateStorage` right before `Library`'s `cx.subscribe` router swaps pages, and the freshly-constructed replacement view is seeded from it — i.e. scroll position survives a full row-cache teardown/rebuild on every navigation, because it's tracked as separate, tiny, explicitly-threaded state rather than something the row cache itself needs to preserve.

One notable **negative finding**: hummingbird's library/track/album/queue list views have **no** arrow-key/PageUp/PageDown/Home/End keyboard navigation at all (verified: no `ArrowUp`/`ArrowDown`/`on_key_down` hits in `table.rs`/`track_view.rs`/`track_listing.rs`). Full keyboard list navigation exists only in two special-cased components: the command-palette finder (custom `Previous`/`Next`/`Accept` events, each also calling `list_state.scroll_to_reveal_item(idx)`) and the generic `Dropdown` popup (full Up/Down/Home/End/Tab/Enter bound in a `"Dropdown"` key context, §8) — i.e. keyboard list nav was built once, generically, for overlays, and never extended to the primary content lists.

### zeron: `ListState` is the default (variable-height chat transcript), `uniform_list` only for fixed-height pickers

zeron's core content — the chat transcript — is inherently variable-height (markdown blocks, code, images, tool calls), so it uses `gpui::ListState` pervasively: `crates/ui/src/transcript.rs:2723` (`list: ListState`), `crates/ui/src/history.rs:1342`, `crates/ui/src/changes.rs:1660` (diff list), `crates/ui/src/files/markdown_preview.rs:127`. `uniform_list` shows up only where rows truly are fixed-height, e.g. the command-palette catalog list (`crates/ui/src/pickers.rs:3479`).

Two things worth lifting straight out of `transcript.rs`:

**1. Content-addressed scroll restore** (survives content reflow, not just index-stable lists — directly relevant to a doc whose sections can resize as images/code blocks finish loading):
```rust
// crates/ui/src/transcript.rs:2538-2554
struct ViewportAnchor { row_id: SharedString, entry_id: SharedString, fallback_ix: usize, offset_in_row: Pixels }
impl ViewportAnchor {
    fn capture(rows: &[Row], scroll_top: ListOffset) -> Option<Self> {
        let fallback_ix = scroll_top.item_ix.min(rows.len().checked_sub(1)?);
        let row = &rows[fallback_ix];
        Some(Self { row_id: row.id.clone(), entry_id: row.entry_id.clone(),
            fallback_ix, offset_in_row: scroll_top.offset_in_item })
    }
    fn resolve_exact(&self, rows: &[Row]) -> Option<ListOffset> {
        let item_ix = rows.iter().position(|row| row.id == self.row_id)?;
        Some(ListOffset { item_ix, offset_in_item: self.offset_in_row })
    }
}
```
`resolve()` (line 2558) falls back from exact row-id match to "nearest surviving row in the same message entry" when a streaming block reshapes and the exact row disappears — restoring "roughly where you were" instead of snapping to the top.

**2. A synced-scroll minimap/rail**, structurally identical to what a docs-reader TOC-rail needs:
```rust
// crates/ui/src/rail.rs:1-8
//! MessageRail (feature-inventory §1.8): a left vertical minimap of the user's
//! prompts. The active tick brightens, hover grows the tick and shows a
//! preview card (prompt + reply opening), click smooth-scrolls the transcript
//! to that row. Hidden below a 48rem container width.
//! Pure logic (tick extraction, active detection, width gate, previews) lives
//! in free functions with unit tests; rendering is an `impl Transcript`
//! extension since the rail shares the transcript's rows and `ListState`.
```
`files/tree.rs` also drives a **custom** scrollbar off `ListState`'s own measurement APIs rather than the platform scrollbar: `ListState::viewport_bounds`, `ListState::max_offset_for_scrollbar`, `ListState::scroll_px_offset_for_scrollbar` (`crates/ui/src/files/tree.rs:69-76`). The tree uses a **fixed** row height (`TREE_ROW_HEIGHT = 27.0`, `with_uniform_item_height`) unlike the transcript's measured rows, and implements genuine tree keyboard semantics rather than flat list nav — up/down linear, left collapses-then-jumps-to-parent, right expands-then-descends-into-child (`files/tree.rs:385-442`), each keystroke also calling `tree_list.scroll_to_reveal_item(index)`.

**Splice vs. remeasure — the detail that makes scroll-anchor restore actually work.** `Transcript::sync()` (`transcript.rs:4519-4549`) diffs the old/new row array by `(id, version)` (`diff_rows`, `transcript.rs:1719-1735`) and picks between two different `ListState` update calls depending on what changed: **`ListState::splice`** only when the row *count* changes (rows inserted/removed), and **`ListState::remeasure_items`** when only content changed at the same count (e.g. a "live" streaming block settling into its final shape). This distinction matters because `splice` resets affected rows to "unmeasured," which — combined with a naive scroll-restore — would clobber the very anchor you're trying to preserve; `remeasure_items` re-measures in place without disturbing anchoring. A docs reader re-rendering a section after an edit (same block count, different content) should call the "remeasure" equivalent, not a full splice.

### Verdict
- **ADOPT**: `ListState` (not `uniform_list`) as the default for the doc body, since rendered markdown sections are variable-height; reserve `uniform_list`/`uniform_grid` for genuinely fixed-height chrome (a TOC sidebar list, a search-results list).
- **ADOPT**: the `ViewportAnchor` capture/resolve-by-id-with-nearest-fallback pattern for scroll restore — a docs reader that re-renders a page after a live-reload or a font-metrics change needs exactly this, not a raw index.
- **ADOPT**: the splice-vs-remeasure distinction — treat "row count changed" and "row content changed at the same count" as two different `ListState` operations, since conflating them (always splicing) is what breaks scroll-anchor restore in the first place.
- **ADOPT**: `files/tree.rs`'s real tree keyboard semantics (left/right = collapse-to-parent/expand-to-child, not just up/down) for a docs reader's own sidebar file/heading tree — directly reusable, since a docs TOC is structurally a tree, not a flat list.
- **ADOPT**: the `MessageRail` minimap is essentially a TOC-rail; the "hidden below N rem container width" responsive collapse and "click smooth-scrolls to that row" behavior transfers directly.
- **ADAPT**: zeron's `ListState`-based custom scrollbar (`files/tree.rs`) — build the docs reader's scrollbar the same way if it needs a minimap-style scrollbar (heading ticks on the track) rather than the OS default.
- **ADOPT**: hummingbird's `create_or_retrieve_view`/`prune_views` per-index child-`Entity` cache for any `uniform_list` row that itself needs to be a stateful `Entity` (not just a stateless render closure) — a generically reusable ~40-line utility, not domain-specific.
- **Gap to deliberately avoid repeating**: build keyboard list navigation (arrow keys, Home/End, PageUp/Down) into the docs reader's primary content lists (TOC, search results) from day one — hummingbird built it once for overlays and never extended it, leaving its main content browsing mouse-only.

---

## 4. Animation & motion

This is the single richest section in either codebase — **zeron ships a real animation kit** (`crates/ui/src/motion.rs`, 1164 lines) that a heavy-animation docs reader should study closely.

### The core problem zeron already hit and fixed: naive `with_animation(...repeating...)` pins the GPU/CPU

```rust
// crates/ui/src/motion.rs:44-51
/// The loaders used to run as gpui `with_animation(...repeating...)` elements,
/// which request a redraw every display frame for as long as they are
/// mounted — one Working session row pinned the whole window at 120Hz
/// (measured 36% CPU on an M-series laptop, with the always-hot Metal
/// pipeline holding hundreds of MB of graphics buffers). A shared 30fps
/// clock is visually equivalent for these chunky cell waves at a quarter of
/// the redraws, and a window with no spinner mounted schedules nothing at all.
const PULSE_TICK: Duration = Duration::from_millis(33); // ~30fps
const PULSE_LEASE: Duration = Duration::from_millis(300);
```
The fix is a single shared **lease-based clock** (`PulseClock`, a `Global`): any animated view calls `pulse_lease(view_id, cx)` each paint to renew a 300ms lease; a single background task ticks at 30fps, notifies only the leased `EntityId`s, and **stops scheduling entirely once the lease map is empty** (`crates/ui/src/motion.rs:29-190`, `clock.running = false` when idle). A `stride` field lets some views tick at 15Hz (`pulse_delta_slow`) off the *same* clock/epoch instead of running a second timer. This directly generalizes to "N docs-reader loading spinners / streaming-token cursors must not each run their own 120Hz animation."

### A hand-rolled CSS `cubic-bezier` evaluator, not GPUI's built-in eases

```rust
// crates/ui/src/motion.rs:200-260 (elided)
pub struct CubicBezier { pub x1: f32, pub y1: f32, pub x2: f32, pub y2: f32 }
impl CubicBezier {
    fn solve_t_for_x(&self, x: f32) -> f32 { /* Newton–Raphson, bisection fallback */ }
}
```
This exists so the port could match the original web app's exact Tailwind/CSS easing curves (`docs/research/mugen-pretext.md`-style parity). A doc-reader porting an existing web design system's easing curves should do the same rather than approximating with GPUI's stock eases.

### A named, reusable motion catalogue instead of ad hoc `with_animation` calls everywhere

```rust
// crates/ui/src/motion.rs:370-409 (elided)
pub const FADE_IN: MotionSpec = MotionSpec::new(500, EASE_OUT_EXPO);
pub const MENU_IN: MotionSpec = MotionSpec::new(140, EASE);
pub const DIALOG_IN: MotionSpec = MotionSpec::new(180, EASE);
pub const SPLASH_OUT: MotionSpec = MotionSpec::new(500, EASE).with_delay(150);
pub const RESIZE: MotionSpec = MotionSpec::new(200, EASE_OUT);
pub const HOVER_FADE: MotionSpec = MotionSpec::new(150, EASE_TAILWIND);

pub fn fade_in<E: Styled + IntoElement + 'static>(id: impl Into<ElementId>, element: E) -> AnimationElement<E> {
    element.with_animation(id, FADE_IN.animation(), |el, t| el.relative().opacity(t).top(px(4.0 * (1.0 - t))))
}
```
(`crates/ui/src/motion.rs:501-529`.) `translateY` is implemented as a relative `top` inset rather than a real transform, with a documented reason: "taffy applies relative insets after layout, so — like a CSS transform — siblings never move" (`motion.rs:23-26`); GPUI (at the pinned rev) has no `scale` transform for `div`s, so scale-based entrances (`menu_in`/`dialog_in`) are **approximated** with fade+translate (`motion.rs:26-27`) — a real limitation to know about before designing a docs reader's transition language around scale.

### Hover transitions that don't snap — a hand-driven tween, deliberately *not* `with_animation`

GPUI's `.hover()` style snaps instantly (applies the frame the pointer enters); the original zeron web app had 150ms CSS `transition-colors` on every interactive element. Rather than fight `with_animation`'s per-element-id clock (which **replays from 0 on remount** — a documented footgun, `motion.rs:547-549`), zeron built a tiny manual tween keyed by string, living in a `thread_local` (not a `Global`, specifically so free-function element builders don't need `cx` threaded through every signature):
```rust
// crates/ui/src/motion.rs:625-635 (doc comment) + 737-770 (elided)
// gpui `.hover()` styles snap by construction... The original zeron puts
// Tailwind `transition-colors` (150ms, cubic-bezier(0.4,0,0.2,1)) on every
// interactive wash, so hover states FADE. This is the manual-drive tween...
pub fn hover_t(key: &str) -> f32 { HOVER_FADES.with(|f| f.borrow_mut().value_at(key, Instant::now())) }
pub fn set_hover(key: &str, hovered: bool, reduced: bool) { ... }
pub fn hover_listener(key: impl Into<SharedString>) -> impl Fn(&bool, &mut Window, &mut App) {
    move |hovered, window, cx| { set_hover(&key, *hovered, reduced_motion(cx)); window.refresh(); }
}
pub fn hover_fades_active() -> bool { /* once-per-frame tick: prune settled/unmounted, report "still animating" */ }
```
Entries are pruned by a **liveness stamp**: every read stamps a frame counter, and `hover_fades_active()` (called once per window frame from the shell's render tail) drops any entry not read for a full frame — i.e. its element unmounted mid-hover and will never send a "leave" event (`motion.rs:663-668`). This is a genuinely non-obvious, well-engineered solution to a real GPUI limitation.

### The unifying mechanism: one boolean OR-gate decides whether another frame is requested at all

zeron's manual-tween systems (`WidthTween`, hover fades, resize bounce, composer-dock spring, background-artwork fade) are all driven by the *same* single choke point at the window's root render, rather than each independently scheduling its own repaint:
```rust
// crates/ui/src/shell.rs:11010-11016 (elided)
if self.motion_active.get() | motion::hover_fades_active() {
    window.request_animation_frame();
}
```
Every manual-drive subsystem sets `self.motion_active` (a `Cell<bool>`) instead of calling `cx.notify()` directly:
```rust
// crates/ui/src/shell.rs:5020-5034 (elided) — eval_tween, called from render, not from a timer
self.motion_active.set(true);
motion::lerp(from, to, RESIZE.progress(raw))
```
This is the concrete answer to "how do you keep N independently-animating manual tweens from each scheduling their own redraw": collapse them all into one `bool` (or in this case two, OR'd) checked once per frame, so the cost of "is anything still animating" is O(1) regardless of how many tween systems exist. Also explains *why* zeron avoids `with_animation` for anything whose element-id path can remount:
```rust
// crates/ui/src/shell.rs:959-965
/// A oneshot width tween (200ms ease-out), driven MANUALLY from render via
/// [`Shell::eval_tween`] — never through a `with_animation` wrapper. gpui keys
/// an animation element's start time by its full global element-id path, so a
/// wrapper that mounts/remounts (route swap, or an ancestor animation keyed by
/// a fresh epoch) silently REPLAYS the tween from t=0.
```

### A real critically-damped spring, not just eased tweens

Besides the CSS-style `MotionSpec`/`CubicBezier` catalogue, zeron also has a genuine physical spring for the composer's hero-canvas↔docked-at-bottom transition (`composer_dock.rs`), distinct from the ease-based `WidthTween`:
```rust
// crates/ui/src/composer_dock.rs:32-43
pub fn advance(&mut self, target: f32, seconds: f32, duration: f32) {
    self.target = target;
    let omega = 12.0 / duration;
    let displacement = self.value - target;
    let c = self.velocity + omega * displacement;
    let decay = (-omega * seconds).exp();
    self.value = target + (displacement + c * seconds) * decay;
    self.velocity = (self.velocity - omega * c * seconds) * decay;
    if !self.active() { *self = Self::new(target); }
}
```
The owning `Element`'s `prepaint` reads *measured* real layout bounds and applies the spring's offset via `window.with_element_offset`, so the animation is driven off actual geometry rather than a guessed endpoint (matching the module's own opening comment: "a resize never substitutes a guessed endpoint," `composer_dock.rs:2`) — worth copying if the docs reader animates a panel whose target size depends on content it hasn't measured yet (e.g. a collapsing/expanding TOC pane).

A real bug worth knowing about ahead of time: the hand-rolled `CubicBezier` solver can produce `y` values a hair outside `[0,1]` from `f32` rounding, and `with_animation`'s delta **asserts** its input is in range and panics if not — zeron clamps the bezier's output explicitly and has a regression test pinned to that exact panic (`motion.rs:859-876`).

### Reduced motion is one global flag, honored automatically

```rust
// crates/ui/src/motion.rs:18-21
//! Reduced motion: gpui's `App::reduce_motion` flag is honored *automatically*
//! by every `with_animation` element — oneshot animations snap to their end
//! state, repeating ones to their start state, and no frames are scheduled.
```
zeron's own hand-rolled systems (`PulseClock`, `HoverFades`) explicitly check `cx.reduce_motion()`/a passed `reduced` flag themselves since they bypass `with_animation` (`motion.rs:118`, `HoverFades::set_at`).

### hummingbird: `with_animation` reserved for one-shots; continuous motion is a hand-rolled, self-terminating `cx.on_next_frame` loop

`with_animation`/`Animation` appear at only two call sites in the whole app, both short, declarative, one-shot tweens — the toast auto-dismiss progress bar:
```rust
// src/ui/toasts.rs:242-247
div().h(px(2.0)).w_full().bg(track).with_animation(
    ElementId::from(("toast-progress", id)),
    Animation::new(duration),
    |this, delta| this.w(relative(1.0 - delta)),
)
```
(gated by `.filter(|_| !reduced_motion)` at the call site) and a "forward navigation" peek-in overlay keyed by a generation counter specifically so re-triggering it (pressing Back again) restarts the animation even though the base `ElementId` string is unchanged (`src/ui/library/nav_buttons.rs:121-133`, `("library-forward-peek", peek_generation)`) — the same remount/replay concern zeron's `WidthTween` doc calls out, solved here by *embracing* the replay via a changing id instead of avoiding `with_animation`.

**Everything else continuous or multi-step is a hand-rolled, self-terminating `cx.on_next_frame` loop** — a genuinely important pattern, independently convergent with zeron's "one gate decides whether to request another frame" philosophy (§ above), but shaped differently: instead of one shared boolean checked at the window root, each animating view owns its *own* stop condition and reschedules only itself:
```rust
// src/ui/lyrics.rs:333-374 (elided)
fn schedule_follow_frame(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    if self.follow_frame_scheduled { return; }
    self.follow_frame_scheduled = true;
    cx.on_next_frame(window, |this, window, cx| {
        this.follow_frame_scheduled = false;
        let reduced_motion = cx.global::<SettingsGlobal>().model.read(cx).interface.reduced_motion;
        this.advance_animations(window, cx, reduced_motion);
    });
}
fn advance_animations(&mut self, window: &mut Window, cx: &mut Context<Self>, reduced_motion: bool) {
    let mut changed = false;
    changed |= self.advance_follow_animation(window, cx, reduced_motion);
    changed |= self.advance_line_emphasis_animation(reduced_motion);
    if !reduced_motion && self.needs_animation_frame() { self.schedule_follow_frame(window, cx); } // <- the self-terminating check
    if changed { cx.notify(); }
}
```
The stop condition is an explicit predicate naming every reason the loop must keep running — once all are false, the loop simply doesn't reschedule and costs nothing:
```rust
// src/ui/lyrics.rs:532-537
fn needs_animation_frame(&self) -> bool {
    self.line_emphasis_started_at.is_some() || self.follow_pending
        || self.scroll_follow.is_active() || self.has_recent_user_interaction()
}
```
The identical loop shape drives the queue's auto-scroll-to-now-playing (`src/ui/queue.rs:776-789`), additionally pausing while the user is hovering or drag-reordering — interaction always wins over the animation. The easing/lerp math itself is centralized in a tiny, reusable `Instant`-based state machine shared by both call sites rather than duplicated:
```rust
// src/ui/scroll_follow.rs (SmoothScrollFollow::advance, elided)
let progress = (animation.started_at.elapsed().as_secs_f32() / self.duration.as_secs_f32()).clamp(0.0, 1.0);
let eased_progress = ease_out_cubic(progress); // 1.0 - (1.0 - progress).powi(3)
let current_scroll_top = animation.start_scroll_top + (animation.target_scroll_top - animation.start_scroll_top) * eased_progress;
scroll_handle.set_offset(gpui::Point { x: current_offset.x, y: -current_scroll_top });
```
with explicit `snap()`/`jump_to()` variants that reduced-motion call sites use instead of `animate_to()`.

The equalizer's spectrum/curve view redraws are driven by data changes (`cx.notify()` from the background analyzer, §2), not a frame-loop animation at all — it never hits the "always-hot repeating animation" problem zeron found, simply because nothing in hummingbird runs a long-lived *cosmetic* repeating animation like a pulsing loader; the closest analog (the analyzer's own viewer-gated poll rate, §2) solves the same class of problem one level down, in the data-production loop rather than the render loop.

**Throttling a continuous drag callback** (the seek/volume `Slider`, a raw `Element` — see §5) uses a plain `Instant` comparison rather than firing `on_change` on every `MouseMoveEvent`:
```rust
// src/ui/components/slider.rs:207-224 (elided)
let now = Instant::now();
if now.duration_since(state.1) >= min_interval { (func_move.borrow_mut())(value, window, cx); state.1 = now; }
```
(`min_interval` is caller-configurable via `.change_interval(...)`.) Ordinary hover/press color changes use GPUI's declarative `.hover(|s| ...)`/`.active(|s| ...)` style callbacks with no animation loop at all — the hand-rolled loops are reserved specifically for continuous/multi-frame motion (scroll-follow, line-emphasis fade), a clear "cheap state-swap vs. expensive continuous motion" split.

### Verdict
- **ADOPT, highest priority (two complementary patterns, not one)**: zeron's `PulseClock` lease/stride pattern for *many independent, low-frequency cosmetic* animations sharing one clock (streaming-cursor blink, skeleton shimmer, spinners), **and** hummingbird's `cx.on_next_frame` self-terminating RAF loop (`needs_animation_frame()` predicate) for *one view's own continuous, multi-property* motion (scroll-follow, emphasis fades). These solve different shapes of the same underlying problem — "don't schedule frames nobody needs" — and a docs reader will want both: the shared clock for streaming-token cursors across many open sections, the self-terminating loop for a single document's smooth-scroll-to-heading.
- **ADOPT**: zeron's single boolean OR-gate (`motion_active.get() | hover_fades_active()`) checked once at the window root to decide whether to call `window.request_animation_frame()` at all — the mechanism that lets an arbitrary number of independent manual-tween subsystems share one cheap "is anything animating" check instead of each separately triggering repaints.
- **ADOPT**: the named `MotionSpec` catalogue (`FADE_IN`, `MENU_IN`, `DIALOG_IN`, `RESIZE`, `HOVER_FADE`, ...) as constants instead of magic durations/eases scattered through render code.
- **ADOPT**: the manual `hover_t`/`set_hover`/`hover_fades_active` tween pattern for any hover transition that must not snap and must not replay-on-remount — this is a real GPUI gap the docs reader will hit immediately with hover-highlighted headings/links.
- **ADOPT**: a real critically-damped spring (zeron's `composer_dock.rs` `Glide`) for any panel/pane transition whose target size depends on content not yet measured (a collapsing TOC pane, an expanding code block) — driven off *measured* layout in `prepaint`, never a guessed endpoint.
- **ADOPT**: hummingbird's `SmoothScrollFollow` — a small, reusable `Instant`-based animate/snap/jump state machine — as the shape for any "smooth-scroll-to-X with a reduced-motion snap fallback" need (jump to a linked heading, scroll to search result).
- **ADAPT**: `CubicBezier` — only worth porting if the docs reader is matching an existing design system's exact easing curves; otherwise GPUI's built-in eases are fine. If you do port it, clamp its output to `[0,1]` from the start — `with_animation` panics on an out-of-range delta, and zeron hit this for real (regression test at `motion.rs:859-876`).
- **IGNORE**: nothing in this section anymore — hummingbird's animation code turned out to be a first-class, independently-convergent pattern (the RAF self-terminating loop), not merely "textbook `with_animation`."
- **Known limitation to design around**: no `scale` transform for `div`s at the GPUI rev both apps pin — plan entrance/exit motion around opacity + relative-position insets, not scale, unless you confirm a newer GPUI adds it.

---

## 5. Custom drawing

### hummingbird: a full custom `Element` for the EQ graph, `PathBuilder` + `paint_path`/`paint_quad` throughout

`EqGraph` (`src/ui/equalizer/graph.rs:233`) is not a `canvas()` closure — it's a real `impl Element` with its own `RequestLayoutState`/`PrepaintState`:
```rust
// src/ui/equalizer/graph.rs:350-395 (elided)
impl Element for EqGraph {
    type RequestLayoutState = ();
    type PrepaintState = EqGraphPrepaint;
    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>,
        window: &mut Window, cx: &mut App) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = px(420.0).into();
        (window.request_layout(style, [], cx), ())
    }
    fn prepaint(&mut self, id: Option<&GlobalElementId>, _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>, _: &mut (), window: &mut Window, cx: &mut App) -> EqGraphPrepaint {
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        // ...computes plot bounds, shapes axis-label text, builds hover/drag state...
```
`prepaint` does the expensive part once (measuring text via `window.text_system()`, computing curve sample points, building an `EqGraphPrepaint` struct that also owns a `Hitbox` for mouse interaction), and `paint` (line 546) does the actual drawing with `PathBuilder`:
```rust
// src/ui/equalizer/graph.rs:624-669 (elided)
let mut fill = PathBuilder::fill();
/* ...append curve points... */
window.paint_path(path, curve_fill);
let mut edge = PathBuilder::stroke(px(1.5));
window.paint_path(path, edge_color);
let mut stroke = PathBuilder::stroke(px(2.0));
window.paint_path(path, stroke_color);
```
plus `window.paint_quad(quad(...))` calls for grid lines/backgrounds (lines 577, 593, 604, 683, 691...). There's also a smaller custom-drawn `knob.rs` (rotary EQ-band knob, `PathBuilder::stroke` at line 344) and a genuine `canvas()` element — used purely for **hit-testing**, not visuals, in `src/ui/components/window_chrome.rs:51-85` (an invisible full-window overlay that resolves a resize-cursor edge under the mouse and calls `window.set_cursor_style(...)`, described fully in §7). Free functions `curve_path`/`spectrum_path` (`graph.rs:150,163`) build `PathBuilder` geometry from raw sample arrays — reusable across the pre/post spectrum overlay and the filter-response curve; `spectrum_path` specifically fits a **Catmull-Rom spline through the raw samples, converted to cubic Béziers**, rather than a straight-segment polyline, for visual smoothness at low sample density:
```rust
// src/ui/equalizer/graph.rs:163-189 (elided)
let ctrl_a = vertex((x1 + (x2 - x0) / 6.0, y1 + (y2 - y0) / 6.0));
let ctrl_b = vertex((x2 - (x3 - x1) / 6.0, y2 - (y3 - y1) / 6.0));
builder.cubic_bezier_to(vertex((x2, y2)), ctrl_a, ctrl_b);
```
Expensive per-pixel curve sampling (one Biquad filter evaluation per column per enabled band) is **memoized inside the raw `Element`** via `window.with_optional_element_state`, invalidated only when the inputs that actually matter change:
```rust
// src/ui/equalizer/graph.rs:405-461 (elided)
let stale = match &state_ref.cache {
    Some(cache) => cache.config != *config || cache.selected != selected || cache.width != plot_width
        || cache.height != plot_height || cache.scale != scale || cache.rate != rate,
    None => true,
};
```
— the same "memoize inside the element, not the entity" technique reused for `Slider`'s drag state and `ManagedImage`'s decode handle (§11).

### zeron: blur/frost as a paint-layer effect, verified by pixel sampling rather than eyeballing

zeron's visual "frost" (glassmorphism) effect (`crates/ui/src/frost.rs`) and its correctness are validated computationally in the `browser-fixture` example rather than only visually:
```rust
// crates/ui/examples/browser-fixture.rs:68-119 (elided)
fn validate_blur(directory: &Path, region: (f64, f64, f64, f64), window_width: f32) -> anyhow::Result<()> {
    let baseline = image::open(directory.join("browser-blur-baseline-dark.png"))?.to_rgb8();
    // samples a small pixel region, computes local edge-energy (sum of |pixel - right| + |pixel - below|)
    // and mean luminance across baseline (sharp checkerboard) vs the captured blur screenshot
    anyhow::ensure!(dark.0 < sharp * 0.035 && light.0 < sharp * 0.035, "menu tint did not blur checkerboard edges");
    anyhow::ensure!((light.1 - solid.1).abs() > 3., "menu is opaque instead of showing the live page backdrop");
```
i.e., render a known checkerboard pattern behind the blurred surface, screenshot it, and assert the edge-energy dropped by a threshold — an automatable, CI-friendly way to regression-test a blur/shadow/gradient effect instead of relying on a human diffing screenshots. Directly reusable for a docs reader if it ships frosted/blurred chrome.

Two more custom `Element`s are directly relevant to a docs reader with a glass/frosted sidebar or scroll-fading content pane:

**`edge_fade.rs`** (427 lines) fades content at a scroll edge — but deliberately *not* via a gradient-colored overlay div, because over a see-through blurred backdrop "what is behind the window" isn't a paintable color at all. Instead it's a custom `Element` (`EdgeFaded`) that calls gpui's own per-pixel edge-fade primitive around the child's paint:
```rust
// crates/ui/src/edge_fade.rs:178-186 (elided)
fn paint(&mut self, ..., bounds: Bounds<Pixels>, ..., window: &mut Window, cx: &mut App) {
    window.with_edge_fade(Some(fade), |window| self.child.paint(window, cx));
}
```
A real, documented timing bug is worth inheriting the fix for: the fade amount must be computed at **paint time**, not render time, because "prepaint clamps the offset for this frame" and render-time gating would read the *previous* frame's stale scroll offset (`edge_fade.rs:112-118`).

**`frost.rs`** (177 lines) layers a native in-scene backdrop blur for floating cards on top of window-level OS blur, and — like `edge_fade.rs` — had to become a custom `Element` specifically to fix a real bug: gpui orders paint primitives by *type* within an order group (quads, then icons, then images), so a naive frosted card could have a later hover repaint elsewhere reassign its own quads to render *below* the blur, making washes/borders flicker away intermittently (a real user-reported bug, documented inline). The fix wraps the blur+child in one paint layer so ordering is pinned together:
```rust
// crates/ui/src/frost.rs:72-93 (elided)
window.paint_layer(bounds, |window| {
    window.paint_backdrop_blur(bounds, Corners::all(px(self.corner_radius)), px(self.blur_radius));
    self.child.paint(window, cx);
});
```
Neither GPUI app draws an arc primitive directly — both `context_usage.rs`'s progress ring and hummingbird's `knob.rs` stroke an **arc as a manually-sampled polyline/Bézier path** via `PathBuilder`, because gpui paths have no native arc.

### Verdict
- **ADOPT**: the full `impl Element` (`request_layout`/`prepaint`/`paint`) shape — not `canvas()` — for anything that needs a `Hitbox` plus custom geometry (e.g., a syntax-highlighted minimap, a custom progress ring, a diagram/chart embedded in docs). `canvas()` is fine for pure decoration with no hit-testing.
- **ADOPT**: `PathBuilder::fill()`/`PathBuilder::stroke(width)` + `window.paint_path` for any vector drawing (charts, progress arcs, diagrams) — hummingbird's `graph.rs` is a complete, working reference implementation to copy structurally.
- **ADOPT**: pixel-sampling assertions (`validate_blur`'s edge-energy/mean-luminance technique) for any blur/shadow/frost effect the docs reader ships, wired into its own screenshot-fixture harness (§10).
- **ADOPT**: `edge_fade.rs`'s "compute at paint time, not render time" fix and `frost.rs`'s "wrap blur+child in one `paint_layer`" fix — both are pre-paid tuition for exactly the two bugs a docs reader will hit the moment it tries a frosted sidebar with a scroll-fading content pane; copying the *documented reasons*, not just the code, avoids re-discovering them the hard way.
- **ADOPT**: manually-sampled arc-as-polyline via `PathBuilder` (both apps do this identically) for any progress ring/radial indicator — gpui has no arc primitive at either app's pinned rev.
- **IGNORE**: the specific EQ/spectrum visuals — domain-specific to audio, not reusable content, only the *technique* transfers.

---

## 6. Theming

### hummingbird: theme is a hand-parsed user file format, hot-reloaded from disk

`Theme` itself (`src/ui/theme.rs`) is a large **flat struct of hundreds of semantic component tokens** (`nav_button_hover`, `eq_grid_line_zero`, `toast_success_track`, `palette_item_border_active`, ...), each a plain `Rgba` — every component reads exactly the named token it needs, with no runtime shade derivation; all shading decisions are baked into the theme file. Spacing/radius tokens live in a **separate** compile-time-`const` file (`src/ui/constants.rs`: `APP_ROUNDING`, `PANEL_GAP`, `PANEL_ROUNDING`, ...) rather than the same theme system — a deliberate split between "what a theme file can override" (color) and "what's fixed in layout code" (spacing/radius). Fonts are embedded via `rust_embed::RustEmbed` over the whole `assets/` folder (`fonts/*`, `icons/*`, `images/*`, `src/ui/assets/bundled.rs:5-13`) and then discovered through the custom asset-source scheme shown below — so "drop a font file in the folder" is genuinely sufficient, no per-file Rust edit needed. One more detail directly relevant to an animation-heavy, numeric-display-heavy docs reader: hummingbird forces tabular (fixed-width) figures globally via a `FontFeatures` setting (`window_chrome.rs:162-164`, `[("tnum".to_string(), 1)]`) so numeric displays (track times, dB labels) never jitter width per digit — worth doing for any live-updating number (word count, reading progress %, elapsed time).

Themes are external files parsed with a custom `#[rgb/rgba hex]` deserializer:
```rust
// src/ui/theme.rs:44-62 (elided)
fn parse_hex_color(value: &str) -> Result<Rgba, String> {
    // accepts #rgb, #rgba, #rrggbb, #rrggbbaa
}
```
and the theme directory is watched live with `notify::recommended_watcher`, feeding a `cx.spawn` loop that reloads and re-emits the active theme on file change (`src/ui/theme.rs:11,962-1030`):
```rust
// src/ui/theme.rs:1012-1017 (elided)
let (tx, rx) = channel::<notify::Result<Event>>();
let watcher = notify::recommended_watcher(tx);
if let Ok(mut watcher) = watcher {
    watcher.watch(&data_dir, RecursiveMode::Recursive)?;
    cx.spawn(async move |cx| { loop { while let Ok(event) = rx.try_recv() { /* reload + re-emit */ } } })
```
Settings changes (theme selection) are picked up the ordinary way via `cx.observe(&settings_model, ...)` (`theme.rs:992`). Fonts are discovered dynamically rather than individually embedded:
```rust
// src/ui/app.rs:180-192
pub fn find_fonts(cx: &mut App) -> gpui::Result<()> {
    let paths = cx.asset_source().list("!bundled:fonts")?;
    let mut fonts = vec![];
    for path in paths { if path.ends_with(".ttf") || path.ends_with(".otf") { fonts.push(cx.asset_source().load(&path)?.unwrap()); } }
    cx.text_system().add_fonts(fonts)
}
```
— drop a font file in the bundled-assets folder and it's picked up, no per-file Rust edit needed.

### zeron: a source-neutral theme domain model with format importers kept out of the runtime

```rust
// crates/theme/src/lib.rs:1-9
//! Zeron's source-neutral theme domain model.
//! Runtime code consumes complete [`ThemeVariant`] values. Import formats
//! such as VS Code are deliberately isolated in [`vscode`], so a component
//! never needs to understand a workbench color id or TextMate scope.
mod builtins; mod library; pub mod vscode;
pub use builtins::builtin_registry;
pub use library::{CustomThemeEntry, CustomThemeLibrary, CustomThemeSource, CustomThemeStatus, InstallMode};
```
A custom-theme registry (user-installed themes) lives behind a `RwLock` swapped wholesale by the UI after load/edit (`crates/theme/src/lib.rs:22-35`, `replace_custom_families`). `SurfaceTreatment`/`SurfacePreference` (`lib.rs:56-70`) is a theme-independent axis — "Opaque" vs "Frosted" glass — layered on top of any theme/accent choice, not baked into individual theme files. The consumed `Theme` struct is a `Global` (`crates/ui/src/theme.rs:591,1519` `impl Global for Theme {}`) with an `is_glass()` helper (`theme.rs:899`) components query instead of checking the surface preference directly.

The theme domain model also bakes in **accessibility as a first-class, testable property**, not an afterthought: `Color::ensure_contrast(background, minimum)` iteratively mixes toward black or white (whichever contrasts more) until a WCAG-style contrast minimum is met:
```rust
// crates/theme/src/lib.rs:155-172 (elided)
pub fn ensure_contrast(self, background: Self, minimum: f32) -> Self {
    if self.contrast(background) >= minimum { return self; }
    let target = if Self::BLACK.contrast(background) >= Self::WHITE.contrast(background) { Self::BLACK } else { Self::WHITE };
    for step in 1..=20 { let candidate = self.mix(target, step as f32 / 20.0); if candidate.contrast(background) >= minimum { return candidate; } }
    target
}
```
and a `#[cfg(test)]` visual-QA matrix cross-multiplies every theme variant against a fixed set of test fixtures (300 cells) to catch contrast/structural regressions before they ship (`crates/theme/src/lib.rs:825-835`). The VS Code importer isolates failures **per variant** within a multi-theme package so one malformed file doesn't hide 11 valid ones (`vscode.rs:199-320`, `VariantFailure`), and the custom-theme library only swaps in a freshly-compiled theme "after a complete successful compile, preserving the last known good version on any source error" (`library.rs:230-241`) — i.e. a broken user theme file can never leave the app broken or blank.

One GPUI-specific mechanic worth knowing before it surprises you: because `Theme` is read imperatively at paint time (`cx.global::<Theme>()`) rather than through a reactive binding, **`cx.notify()` alone will not repaint stale colors** — zeron's own module doc explains why the fix is `App::refresh_windows()`, not `.notify()`: it disables gpui's per-view prepaint cache for exactly one frame, forcing every view to actually re-read the new theme instead of replaying its cached paint output (`crates/ui/src/theme.rs:1-33`). Any docs reader storing theme as a `Global` (rather than routing it through the snapshot entity) needs the same escape hatch.

Appearance (dark/light) is a three-state persisted mode resolved against the OS, and — a detail worth copying on macOS — the app also pushes its resolved appearance into AppKit so native chrome (menu, window shadow) matches:
```rust
// crates/ui/src/appearance.rs:78-82, 311-320 (elided)
pub fn resolve(mode: AppearanceMode, system: Appearance) -> Appearance {
    match mode { AppearanceMode::System => system, AppearanceMode::Light => Appearance::Light, AppearanceMode::Dark => Appearance::Dark }
}
fn sync_ns_appearance(mode: AppearanceMode) {
    // AppearanceMode::Light => Some(c"NSAppearanceNameAqua"), ::Dark => Some(c"NSAppearanceNameDarkAqua")
}
```
A second macOS-specific gotcha documented right next to it: `apply()` **unconditionally** re-applies the window background appearance even when the color palette itself didn't change, because "gpui's macOS backend removes the `NSVisualEffectView` from the window the moment the background appearance is anything but `Blurred`, and nothing puts it back on its own — so a single missed re-apply leaves the sidebar and tab strip permanently opaque" (`appearance.rs:248-276`). Any docs reader shipping a translucent/frosted macOS window should treat "re-assert the vibrancy view" as part of *every* appearance-affecting state change, not just palette swaps.
Fonts are individually embedded (Geist + GeistMono, regular/italic/medium/semibold/bold × italic variants) via `include_bytes!` and registered in one call:
```rust
// crates/ui/src/typography.rs:254-282 (elided)
include_bytes!("../assets/fonts/Geist.ttf"), /* ...8 Geist + 8 GeistMono files... */
match cx.text_system().add_fonts(fonts) { ... }
```
with a `FontAvailability`/`register_fonts` return value the settings UI can use to show whether a *user-chosen* custom font family actually resolved (`typography.rs:355`).

### Verdict
- **ADOPT**: zeron's separation of "theme domain model" (colors/tokens) from "import format" (VS Code parser lives in its own module, never touches runtime code) — do the same if the docs reader supports importing external theme/CSS-variable files.
- **ADOPT**: `SurfaceTreatment`/`SurfacePreference` as an orthogonal axis over the theme (glass vs opaque) rather than duplicating every theme in a "-frosted" variant.
- **ADOPT**: `sync_ns_appearance` — push resolved light/dark into `NSAppearanceNameAqua`/`DarkAqua` on macOS so native window chrome doesn't visually clash with the themed content — and unconditionally re-assert the vibrancy/blur window background on every appearance-affecting change, not just palette swaps, since gpui's macOS backend silently drops the `NSVisualEffectView` otherwise.
- **ADOPT**: `Color::ensure_contrast` + a visual-QA test matrix cross-multiplying every theme variant against fixture content — bake accessibility contrast checking into the theme *type*, testable in CI, rather than trusting each theme author to get it right by eye.
- **ADOPT**: isolate theme-import failures per-variant (VS Code importer) and only swap in a newly-compiled custom theme after it fully succeeds, keeping the last-known-good otherwise — essential the moment the docs reader lets users load external theme/CSS files.
- **ADOPT (gotcha)**: if theme colors are read imperatively from a `Global` at paint time, remember `cx.notify()` won't repaint them — you need `App::refresh_windows()` (which disables the per-view prepaint cache for one frame) to force a re-read.
- **ADOPT (dev-quality-of-life)**: hummingbird's `notify`-watched theme directory + hot reload — valuable for a docs reader if it will ever support user-editable themes/CSS, since designers get instant feedback without restarting the app.
- **ADAPT**: font loading — hummingbird's directory-scan approach (`asset_source().list(...)`) is less boilerplate for many weights/families; zeron's explicit `include_bytes!` list is safer when you need to guarantee *exactly* which font files ship. Pick directory-scan for a large/extensible font set, explicit list for a small pinned type system.

---

## 7. Window chrome

Both apps implement the same core primitive — an app-owned titlebar (`window_control_area(WindowControlArea::Drag)` + `window.start_window_move()`) — but zeron's version is measurably more careful about not breaking macOS's native double-click-to-zoom detection.

### hummingbird: direct move-on-mousedown

```rust
// src/ui/components/window_header.rs:78-90
.id("titlebar")
.window_control_area(WindowControlArea::Drag)
.when(cfg!(not(target_os = "windows")), |this| {
    this.on_mouse_down(MouseButton::Left, move |ev, window, _| {
        if ev.click_count != 2 { window.start_window_move(); }
    })
    .on_click(|ev, window, _| { if ev.click_count() == 2 { window.zoom_window(); } })
})
```
Simple and works, but starting the native drag session directly from `on_mouse_down` (guarded only by `click_count != 2`) is exactly the kind of thing zeron's comment (below) warns can misfire around double-clicks.

### zeron: arm-on-down, trigger-on-move, with an explicit note about why

```rust
// crates/ui/src/shell.rs:5183-5215 (elided)
/// Make a titlebar strip drag the window — zed's platform-titlebar pattern:
/// mark it a WindowControlArea::Drag, hand the drag to the compositor once
/// the pointer moves with the button down, and double-click zooms.
el.id(id).window_control_area(WindowControlArea::Drag)
    .on_mouse_down_out(cx.listener(|this, _, _, _| this.titlebar_should_move = false))
    .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, _| this.titlebar_should_move = false))
    .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, _| this.titlebar_should_move = true))
    .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, window, _| {
        if this.titlebar_should_move && event.pressed_button == Some(MouseButton::Left) {
            this.titlebar_should_move = false;
            window.start_window_move(); // AppKit's performWindowDragWithEvent: — native drag session
        }
    }))
```
The comment at `shell.rs:5203-5211` explains *why*: `start_window_move()` on macOS runs AppKit's native drag session, and AppKit resolves a quick second click **inside that session** as a titlebar double-click (system zoom) — a `pressed_button` guard on `on_mouse_move` is required so a stale armed flag (from a mousedown whose bubble got stopped) can't start a drag session from a mere hover-move between the two clicks of a double-click. This is a real, previously-hit bug class, documented in the code.

Sizing/position constants are centralized rather than scattered — `crates/ui/src/surface_chrome.rs` defines shared metrics (`HEADER_HEIGHT`, `CONTROL_SIZE = 24.0`, `CONTROL_RADIUS = 6.0`, `EDGE_INSET = 8.0`) consumed by every sidebar-surface toolbar. Fullscreen-aware titlebar layout is handled with named helpers, not inline math: `titlebar_cluster_start(fullscreen: bool)`, `titlebar_spacer_width(...)` (`shell.rs:227-238`), animated across fullscreen toggles with a `WidthTween` (`shell.rs:1672-1673`, a hand-rolled width/position tween distinct from the `motion.rs` catalogue — same "manual tween, not `with_animation`" philosophy as hover fades).

### Verdict
- **ADOPT**: zeron's arm-on-mousedown / trigger-on-mousemove-with-pressed-button-guard drag pattern over hummingbird's simpler direct-start — the extra state machine exists specifically to not break native double-click-to-zoom, which the docs reader's own titlebar will hit on macOS the same way.
- **ADOPT**: centralize titlebar/toolbar metrics as named constants (`surface_chrome.rs`-style) instead of inlining `px(24.0)` etc. across files.
- **ADAPT**: `WidthTween`/fullscreen-aware titlebar spacer animation — only relevant if the docs reader's chrome visibly reflows on fullscreen toggle or sidebar collapse; otherwise skip.

**Sidebar collapse and resizable panes, in both apps:**

zeron's sidebar collapse lives in `shell.rs`, not a separate component — `toggle_sidebar` sources its `WidthTween` from the *currently-painted* width (not the resting target), so reversing direction mid-animation never jumps:
```rust
// crates/ui/src/shell.rs:2459-2469 (elided)
fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
    let from = self.sidebar_now();
    self.settings.sidebar_collapsed = !self.settings.sidebar_collapsed;
    self.sidebar_tween = Some(WidthTween::new(from, self.sidebar_target()));
    cx.notify();
}
```
Live drag-resize is clamped to `SIDEBAR_MIN=224.0`/`SIDEBAR_MAX=400.0`. Resizable *panes* (as opposed to a binary collapse) get a purpose-built component, `composer_dock.rs` (676 lines) — the composer's hero-canvas↔docked-at-bottom transition, driven by the critically-damped `Glide` spring from §4, whose `prepaint` applies the offset via `window.with_element_offset` against real measured bounds, never a guessed endpoint.

Hummingbird's equivalent, `src/ui/components/resizable.rs`, is a smaller, more generic hand-implemented `Element`: a child plus a thin drag handle on one edge, backed by a caller-owned `Entity<Pixels>` (e.g. `Models.sidebar_width`, `Models.queue_width`, `Models.lyrics_height`), supporting both fixed-pixel and percent sizing with min/max/default clamping (`resizable.rs:23-49`), setting `CursorStyle::ResizeLeftRight`/`ResizeUpDown` on the handle's hitbox and applying drag deltas straight to the entity via raw `cx.on_mouse_event` handlers inside `paint` (the same low-level pattern as its `Slider`, §5). Sidebar collapse itself is a single `Entity<bool>` (`Models.sidebar_collapsed`) read at the top of `render()` to switch between full-width and icon-only rail layouts; the right-side queue/lyrics panel uses its own independent `show_queue`/`show_lyrics` entities rather than sharing the left sidebar's state. Window resize itself (client-side-decorated windows) is a `canvas()`-based hit-test overlay (§5) plus an explicit `window.start_window_resize(edge)` call on mousedown near an edge (`window_chrome.rs:103-110`, 8-way edge/corner detection via `resize_edge`), and the window drop shadow is one explicit `gpui::BoxShadow` rather than a style helper (`window_chrome.rs:139-145`).

More verdict additions:
- **ADOPT**: hummingbird's `resizable.rs` shape — a generic `Element` + caller-owned `Entity<Pixels>` + min/max/default clamping — as the default resizable-pane primitive for a docs reader's sidebar/TOC width; it's simpler than zeron's purpose-built spring-driven dock and sufficient for a plain drag-to-resize handle.
- **ADOPT**: source any collapse/resize tween's starting value from the *currently-painted* value, not the resting state, so reversing mid-animation doesn't jump (zeron's `toggle_sidebar`).
- **ADOPT**: `window.start_window_resize(edge)` + an invisible hit-test `canvas()` overlay for client-side-decorated window edge-resizing on Linux/Windows (hummingbird's `window_chrome.rs`) — directly reusable if the docs reader ships its own window chrome on non-macOS platforms.

---

## 8. Keyboard

### hummingbird: data-driven JSON keymap, parsed into GPUI `KeyBinding`s at startup

```rust
// src/ui/keymap.rs:1-19, 54-80 (elided)
const DEFAULT_KEYBINDS: &str = include_str!("../../assets/keybinds.json");
#[derive(Deserialize)] struct KeymapEntry { key: String, action: String, context: Option<String>, platform: Option<Platform> }
pub fn load_default_keymap(cx: &mut App) {
    let bindings: Vec<KeyBinding> = file.bindings.into_iter()
        .filter(|e| e.platform.is_none_or(Platform::matches))
        .map(|e| {
            let action = cx.build_action(&e.action, None).unwrap_or_else(|err| panic!("unknown action {}: {err}", e.action));
            let context_predicate = e.context.as_deref().map(|ctx| Rc::new(KeyBindingContextPredicate::parse(ctx).unwrap()));
            KeyBinding::load(&e.key, action, context_predicate, false, None, &gpui::DummyKeyboardMapper).unwrap()
        }).collect();
    cx.bind_keys(bindings);
}
```
`Platform` (`macos`/`linux`/`windows`/`!macos`/...) lets one JSON file describe cross-platform bindings declaratively, and there are unit tests asserting the JSON parses and that platform filtering never empties the binding set (`keymap.rs:82-98`). Command palette registration is the ordinary GPUI `App::on_action` (`src/ui/command_palette.rs:369`).

### zeron: user-customizable keymap rebuilt wholesale, plus live conflict detection while recording a new shortcut

5 `actions!(...)` sites (`shell.rs`, `app_menus.rs`, `composer.rs`, `terminal/panel.rs`, `browser/mod.rs`). Unlike hummingbird's static JSON keymap, zeron's bindings are **user-customizable and rebuilt from scratch** every time settings change — `apply_keymap` clears every binding and reconstructs them from a `KeymapConfig`, falling back to a hardcoded default whenever a persisted combo fails to parse:
```rust
// crates/ui/src/shell.rs:302-400 (elided)
pub fn apply_keymap(cx: &mut App, keymap: &KeymapConfig, composer_send_behavior: ComposerSendBehavior) {
    fn valid_or_default(combo: &str, fallback: &str) -> String {
        let candidate = platform_combo(combo);
        if Keystroke::parse(&candidate).is_ok() { candidate } else { platform_combo(fallback) }
    }
    cx.clear_key_bindings();
    // clear_key_bindings ALSO removes gpui-base's own contextual editing
    // actions — they must be reinitialized before rebuilding zeron's own.
    gpui_base::init(cx);
    crate::composer::init(cx, composer_send_behavior);
    cx.bind_keys([ KeyBinding::new(&valid_or_default(&keymap.save_file, "mod-s"), SaveFile, None), /* ... */ ]);
}
```
The `cx.clear_key_bindings()` side effect wiping a third-party crate's own default bindings — requiring an explicit re-init call — is a real gotcha worth designing around from the start rather than discovering after ship.

**Live conflict detection while recording a new shortcut** uses `cx.intercept_keystrokes`, which runs *before* any bound action fires, so a chord that would collide with an existing binding is captured and refused at record time rather than silently overriding or double-firing later:
```rust
// crates/ui/src/settings/shortcuts.rs:134-152 (elided)
fn start_recording(&mut self, id: ShortcutId, window: &mut Window, cx: &mut Context<Self>) {
    self.recording = Some(id);
    self.recording_interceptor = Some(cx.intercept_keystrokes(move |event, window, cx| {
        page.update(cx, |page, cx| { if page.focus.is_focused(window) { page.record_keystroke(&event.keystroke, cx); } });
    }));
    self.recording_blur = Some(cx.on_blur(&self.focus, window, |this, _, cx| { this.stop_recording(); cx.notify(); }));
    window.focus(&self.focus, cx);
}
```

### Overlays' own keyboard handling (shared with §9)
`crates/ui/src/popover.rs` centralizes menu keyboard navigation as pure, tested functions rather than inline key-event matching:
```rust
// crates/ui/src/popover.rs:215, 236, 277-288 (elided)
pub fn menu_step(active: Option<usize>, count: usize, delta: isize) -> Option<usize> { ... } // wraps
pub fn match_rank(query: &str, label: &str) -> Option<usize> { ... }
pub fn classify_key(key: &str, cmd: bool, ctrl: bool) -> MenuKey {
    match key { "escape" => MenuKey::Escape, /* ... */ }
}
```

**Focus management and modal focus restoration.** Opening the command palette explicitly captures whatever had focus, and closing it restores exactly that (not just "focus something reasonable"):
```rust
// crates/ui/src/shell/command_palette.rs:113-131 (elided)
let previous_focus = window.focused(cx);
self.command_palette = Some(CommandPalette { search, focus: cx.focus_handle(), previous_focus, ... });
// on close:
if let Some(focus) = palette.previous_focus { window.focus(&focus, cx); }
```
A window-level safety net catches the general case (any element losing focus without a specific "restore to X" caller) by checking the *completed dispatch tree*, not just whether a handle still exists — because a hidden editor can remain alive after its element unmounts:
```rust
// crates/ui/src/shell.rs:91-121 (elided)
pub(crate) fn restore_mounted_focus(root: &FocusHandle, preferred: &FocusHandle, unfocused: &FocusHandle, window: &mut Window, cx: &mut App) {
    let preferred_mounted = root.contains(preferred, window);
    if !root.contains_focused(window, cx) || (root.is_focused(window) && preferred_mounted) {
        let target = if window.focused(cx).is_none() { unfocused } else if preferred_mounted { preferred } else { root };
        window.focus(target, cx);
    }
}
```
wired from `cx.on_focus_lost`, with a one-frame-deferred variant (`restore_focus_if_empty_on_next_frame`) so an in-flight focus handoff isn't stolen from by a synchronous fallback running too early in the same tick.

### Verdict
- **ADOPT**: hummingbird's JSON-keymap + `Platform` predicate + `KeyBindingContextPredicate` shape — a docs reader's navigation-heavy keybindings (jump to heading, next/prev page, focus search) benefit from being data, testable, and diffable, exactly as hummingbird's own two unit tests demonstrate (`default_keybinds_json_parses`, `platform_filter_covers_all_entries`).
- **ADOPT**: `menu_step`/`classify_key` as **pure, unit-tested** free functions for list/menu keyboard navigation (up/down-wraps, enter, escape) — decoupling "what key does what" from "how the menu renders" is what makes zeron's 100+ `#[gpui::test]`s possible (§10).
- **ADAPT**: zeron's user-editable-shortcuts settings page — worth it only if the docs reader commits to end-user keybinding customization; otherwise the static JSON keymap is simpler and sufficient. If you do build one, copy `cx.intercept_keystrokes` for conflict detection at record time and the "clear + reinit third-party bindings" discipline (`apply_keymap`'s `gpui_base::init(cx)` re-call after `cx.clear_key_bindings()`).
- **ADOPT**: `restore_mounted_focus`'s "check the completed dispatch tree, not just handle liveness" focus-restoration logic, plus explicit capture/restore of the pre-open focus handle on any modal/palette/overlay — a docs reader with a command palette and multiple focusable surfaces (search, TOC, document body) will hit exactly this class of "focus went nowhere after closing an overlay" bug otherwise.

---

## 9. Overlays

### hummingbird: a complete, reusable `anchored()`/`deferred()` + dismissal-triad recipe, used consistently across four overlay kinds

Toasts are a capped, animated stack drained from a channel into an entity (see §2/§4 excerpts), max 4 visible (`src/ui/toasts.rs:20` `MAX_VISIBLE: usize = 4`), positioned top-right below the titlebar via the same `anchored()`/`deferred()` idiom zeron uses:
```rust
// src/ui/toasts.rs:149-154
anchored().position(point(viewport.width - px(16.0), px(54.0))).anchor(Anchor::TopRight).child(deferred(column))
```
each dismissible by click, an optional action button, or its own auto-dismiss timer — deliberately **no** click-outside/Escape dismissal, since toasts are non-modal ambient notifications.

Context menus, modals, popovers, and the dropdown all converge on the same three-way dismissal recipe (click-a-row, click-outside, Escape-via-action), each independently implemented but structurally identical — worth copying as a unit:
```rust
// src/ui/components/context.rs:81-119 (elided) — right-click context menu
Some(anchored().position(pos).child(deferred(
    menu.occlude() // hitboxes are paint-order only in gpui; without .occlude() clicks would fall through to whatever's underneath
        .id("menu").track_focus(&focus_handle)
        .on_click(move |_, window, cx| { /* clear position entity */ })
        .on_mouse_down_out(move |_, window, cx| { /* clear position entity */ })
        .on_action(move |_: &CloseContextMenu, window, cx| { /* clear position entity */ }),
)))
```
Modals reuse the *same* three paths but implement click-outside differently — the backdrop is a full-viewport `occlude()`d layer, and the **inner dialog stops its own mousedown from bubbling**, so only a click that truly reaches the backdrop (i.e. genuinely "outside" the dialog) triggers dismissal:
```rust
// src/ui/components/modal.rs:109-133 (elided)
this.on_any_mouse_down(move |_, window, cx| { on_exit_clone(window, cx); }) // backdrop: click anywhere closes
    .on_action(move |_: &CloseModal, window, cx| { on_exit(window, cx); })  // Escape closes
    .child(self.div.occlude().on_any_mouse_down(|_, _, cx| { cx.stop_propagation(); })) // dialog content: eats its own clicks
```
All app dialogs build on one shared `ActionDialog` component that itself wraps `modal::modal()`, so overlay chrome (backdrop, shadow-inset math for client-side-decorated windows, `ModalActive` global tracking "is any modal open") is implemented exactly once. The `Dropdown` popup (§8) is the same `anchored()`+`deferred()`+`on_mouse_down_out` shape again, plus a full keyboard-nav action set scoped to a `"Dropdown"` key context.

### zeron: two deliberately different overlay idioms — anchored dropdown menus vs. in-flow error chips vs. OS-native banners

**Anchored/deferred dropdown menus** (context menus, the command palette, pickers) share one helper:
```rust
// crates/ui/src/popover.rs:2-6, 446-458 (elided)
//! animation, outside-click dismissal, and pure keyboard-navigation + search
//! ...a `deferred(anchored().child(content))`: deferred so the menu paints
//! above everything already laid out, anchored so it tracks its trigger.
pub fn anchored_menu(...) -> AnyElement {
    gpui::deferred(gpui::anchored()....child(...)).with_priority(...) // occludes; `.on_mouse_down_out` dismisses
}
```
Dismissal is `.on_mouse_down_out(...)` on the anchored content plus `Escape` via `classify_key` (§8); a `PopupState<T>`/`Popup<T>` (`popover.rs:69-146,88-175`) tracks open/closing (`begin_close`/`finish_close`) so a close can itself be **animated** rather than an instant unmount — necessary because gpui unmounts an element the instant its backing state drops, which would otherwise cut an exit animation off mid-frame. The exit progress is computed from **wall-clock elapsed at render time**, not the animation element's own internal clock, specifically so it can never replay from 0 mid-exit the way a remounted `with_animation` element would (§4):
```rust
// crates/ui/src/popover.rs:380-401 (elided)
let exit = closing.map(exit_progress); // wall-clock elapsed since begin_close, monotonic by construction
// the frosted card's blur radius rides the same exit progress down to 0, because
// BackdropBlur ignores element_opacity — fading blur has to be driven manually too
```
`note_trigger_press`/`take_press_was_open` exists specifically to distinguish "this press just dismissed the menu, stay closed" from "open fresh" — a subtle click-outside/re-trigger race that's easy to get wrong. Menus don't have to be trigger-anchored — a right-click context menu instead captures the exact click point and positions there, reusing the identical `Popup`/`deferred(anchored())` machinery:
```rust
// crates/ui/src/shell.rs:6229-6238 (elided)
.on_mouse_down(MouseButton::Right, cx.listener(move |this, event: &MouseDownEvent, _, cx| {
    this.chat_menu.open(ChatMenuState { chat_id: menu_id.clone(), position: event.position, page: ChatMenuPage::Root });
}))
```

**In-flow error notices** are a different, deliberate choice — not a floating toast, a tinted card embedded directly in the composer/transcript flow, because failure payloads (stderr, exit codes) need to be read and copied, not glanced at and dismissed:
```rust
// crates/ui/src/notice.rs:1-3, 20-29 (elided)
//! Shared notice chip — the tinted failure card the composer strip and the
//! transcript converged on... a tiny copy button pinned to its top-right
//! corner — failure payloads are meant to be pasted, not screenshotted...
//! The message WRAPS instead of truncating: a one-line ellipsis was exactly
//! what made zeronsh/comet#95 undiagnosable from the screenshot.
```

**Out-of-focus notifications** go through the OS, not an in-app toast: `crates/ui/src/notify.rs` posts native `NSUserNotification` (macOS) / `notify-send` (Linux) desktop banners, with an env kill-switch (`ZERON_DISABLE_NOTIFICATIONS`) and silent failure ("a missing notifier must never bother the session flow", `notify.rs:1-25`).

### Verdict
- **ADOPT**: `deferred(anchored().child(...))` + `.on_mouse_down_out` for any dropdown/context menu/popover — this is the standard GPUI idiom both apps rely on (hummingbird uses the same `anchored`/`deferred` building blocks for its own about-dialog/missing-folder-dialog, per its imports in `src/ui/toasts.rs:1-4`).
- **ADOPT**: `PopupState<T>`'s `begin_close`/`finish_close` + trigger-press-tracking — directly solves the "menu-open click and outside-dismiss-click on the same trigger race" bug class that's easy to hit and easy to ship broken.
- **ADOPT**: drive exit animations off wall-clock-elapsed-since-close (`exit_progress`) rather than an element-keyed animation clock, so a popover's close animation can never replay from 0 if something upstream remounts it mid-exit — and remember that a backdrop-blur radius must be driven manually to zero the same way, since `BackdropBlur` ignores element opacity.
- **ADOPT**: hummingbird's "one shared component implements the dismissal triad once" discipline (`ActionDialog` wrapping `modal::modal()` for every dialog) — write the click-outside/Escape/click-row logic exactly once and have every context-menu/modal/dropdown instance in the docs reader consume it, rather than re-deriving it per overlay.
- **ADOPT (content-appropriate choice)**: reserve floating toasts for transient, glanceable status (hummingbird's model) and use an in-flow, copyable notice card (zeron's `notice_chip`) for anything the user needs to read carefully or copy — e.g. a docs reader's "broken link" or "render error" should probably be a notice chip, not a toast that vanishes.
- **IGNORE**: OS-native desktop banners — not relevant unless the docs reader does background work (e.g. long doc builds) the user might miss while the window isn't focused, in which case it's a cheap, well-isolated pattern to borrow (`notify.rs`'s "logged and swallowed, never blocks the app" discipline is the important part to copy).

---

## 10. Testing / screenshot harnesses (zeron) — corrected scope

As flagged up top, ignore `crates/harness`/`crates/preview` for this topic. zeron actually has **two distinct, complementary** testing layers, both worth adopting:

### Layer A — deterministic, headless logic tests: `#[gpui::test]` + `TestAppContext` + virtual time

Zeron has **100+** `#[gpui::test]` functions inside its production UI files (`composer.rs`, `shell.rs`, `transcript.rs`, `pickers.rs`, `queue.rs`, `lib.rs`, ...) — e.g. `grep -c '#\[gpui::test\]' crates/ui/src/*.rs` finds 104 in a partial scan. These run headless (no real window/display needed) against `gpui::TestAppContext`, and drive time deterministically instead of sleeping:
```rust
// crates/ui/src/pickers.rs:4913-4916, 5401-5403 (elided, representative)
cx.executor().advance_clock(Duration::from_secs(1));
cx.run_until_parked();
// ... later, testing a debounced search:
cx.run_until_parked();
cx.executor().advance_clock(Duration::from_millis(200));
cx.run_until_parked();
```
`cx.run_until_parked()` drains all pending futures/timers to quiescence; `cx.executor().advance_clock(dur)` fast-forwards virtual time so debounce/animation-duration logic can be asserted without real wall-clock waits or flakiness. This is what makes zeron's `PulseClock` background timer loop (§2/§4) safe to unit-test: it checks `scheduler().as_test().is_none()` before touching a platform-precise timer, deferring entirely to the virtual clock under test. Hummingbird has **zero** occurrences of `gpui::test`/`TestAppContext` anywhere in `src/` — its tests (`src/playback/tests/*`, `src/library/scan/*/tests.rs`) exercise backend/audio/library logic only; the GPUI view layer itself is untested.

### Layer B — real-window, real-input, real-OS-capture visual/integration fixtures

`crates/ui/examples/*-fixture.rs` are ordinary Rust binaries (`cargo run -p zeron-ui --example <name>`) that boot the **actual production `Shell`** in a **real GPUI window** with synthetic, offline `AppState` (temp dir, no engine/network) — not headless, not mocked rendering:
```rust
// crates/ui/examples/command-palette-fixture.rs:1,11-17,52-57 (elided)
//! Isolated, offline command palette screenshot fixture. Run on a dedicated display.
gpui_platform::application().with_assets(icons::Assets).run(move |cx| {
    gpui_tokio::init(cx); gpui_base::init(cx);
    settings::init(settings.clone(), data.clone(), cx);
    // ...builds a fake AppState with synthetic chats/spaces/devices...
    let _window = cx.open_window(WindowOptions { window_bounds: ..., titlebar: ..., app_owns_titlebar_drag: true, .. },
        |_, cx| cx.new(|cx| shell::Shell::new(state.clone(), boot, cx))).unwrap();
});
```
Screenshots are taken by **shelling out to the real OS capture tool**, not any in-process render-to-buffer:
```rust
// crates/ui/examples/browser-fixture.rs:22-62 (elided)
#[cfg(target_os = "macos")]
std::process::Command::new("/usr/sbin/screencapture").args(["-x","-o","-l", &window.windowNumber().to_string()]).arg(&path).status()?;
#[cfg(not(target_os = "macos"))]
std::process::Command::new("import").args(["-window", &id]).arg(&path).status()?; // ImageMagick, via xdotool-found window id
```
The more advanced fixtures (`browser-fixture.rs`, 512 lines) go further: they dispatch **real synthetic input** through the actual GPUI event pipeline (`window.dispatch_event(PlatformInput::MouseMove{...})`), assert on internal view state exposed only to the fixture (`b.fixture_native_visible()`, `b.fixture_overlay_visible()`, `b.fixture_visibility_changes()`), and even drive a **continuous screen recording** (`screencapture -v -V 24 ...browser-hover.mov`) while doing 12 rounds of tab/toolbar hover to regression-test tooltip flicker — with a real `requestAnimationFrame` timer overlaid on the live webview specifically to *prove* the capture is a live recording, not a still-frame montage (`browser-fixture.rs:273`, `"LIVE " + elapsed.toFixed(2) + "s"`). This ships as a documented, reproducible CI artifact (`docs/screenshots/browser/README.md`, referencing a specific GitHub Actions run and PR video) rather than an ad hoc manual screenshot.

A `scripts/run-macos-browser-fixture.sh` wraps the raw binary in a minimal `.app` bundle before running it, specifically because "bare executables do not enforce the app's ATS policy" (network security policy needed for the embedded live webview) — a packaging detail worth remembering if a docs-reader fixture also embeds a webview.

### What to copy into a new screenshot harness
1. **Two-layer split**: keep fast, deterministic `#[gpui::test]`+`TestAppContext`+virtual-clock tests in the same file as the code they test (Layer A) for logic/state; keep a **separate, opt-in** example binary that boots a real window with synthetic data for visual regression (Layer B). Don't try to make one mechanism do both.
2. For Layer B, **boot the real production root view** (not a stripped-down test harness view) against fabricated/offline state — this is what makes the fixture screenshots trustworthy as "what the user actually sees."
3. Capture via the **OS's own screenshot tool** (`screencapture`/`import`), not an in-process pixel dump — this is the only way to get an OS-composited real screenshot in the exact GPU/OS path a user would see, correctly through window shadows, corner rounding, etc.
4. If validating a purely-visual property (blur, contrast, color), **compute it from pixels** (§5's `validate_blur`) rather than relying on a human to eyeball the PNG — this is what makes Layer B usable in unattended CI.
5. Drive real input through `window.dispatch_event(PlatformInput::...)` when testing interaction-dependent visuals (hover/tooltip/menu-dismiss timing) rather than only capturing static states.
6. Gate any platform-precise background timer (§2's `PulseClock`) on `scheduler().as_test().is_none()` from day one — retrofitting deterministic-clock support after 100 tests exist is much more expensive than building it in from the first animation.

### Verdict
- **ADOPT, both layers, directly** — this is the strongest, most directly transferable material in either codebase for the stated goal ("screenshot harness" was explicitly asked for). Layer A gives cheap, fast, CI-friendly logic coverage of animation/debounce/list code; Layer B gives trustworthy, automatable visual regression coverage of the real rendered app.
- **IGNORE**: the actual `crates/harness`/`crates/preview` crates (per the naming correction) — not applicable.

---

## 11. Performance practices

### hummingbird
- **Field-granular reactive `Entity`s** (§1) is itself the main over-render defense; no separate "performance mode" exists because the data model makes over-rendering structurally hard to write by accident.
- **Viewer-gated background polling** (§2): the spectrum analyzer's `PARKED_FRAME_MS` (250ms) vs `FRAME_MS` (33ms) based on an atomic viewer count, plus skipping the FFT hop entirely when `analyzer.rings_have_data()` is false (`src/ui/equalizer/spectrum.rs:90-103`: "with the rings empty the tick is trivial, spare the executor hop").
- **A real, GPU-memory-aware image LRU** (`src/ui/caching.rs`, `HummingbirdImageCache`), implementing gpui's own `ImageCache` trait: decode is dispatched to `cx.background_executor()` as a `.shared()` future so concurrent requests for the *same* image dedupe onto one decode, and eviction explicitly calls `cx.drop_image(...)` to release GPU-side texture memory, not just the Rust struct:
  ```rust
  // src/ui/caching.rs:94-111 (elided)
  let task = cx.background_executor().spawn(load_future).shared();
  if self.usage_list.len() >= self.max_items {
      let oldest = self.usage_list.pop_back().unwrap();
      let mut image = self.cache.remove(&oldest).unwrap();
      if let Some(Ok(image)) = image.0.get() { cx.drop_image(image, Some(window)); }
  }
  ```
  Each view attaches its own bounded instance sized to its needs (`50` for the palette finder, `200` for the main track table) so caches don't compete across unrelated views — plus the `Arc<OnceLock<T>>` "bridge" idiom (§2) that lets a decoded image appear a frame earlier than the full `cx.spawn`→`entity.update` round trip would allow.
- **Equality-gated `cx.notify()` is a house style, not a one-off**: the palette/finder only regenerates its list state when the match *set* actually changed (`if matches != this.last_match { ...; cx.notify(); }`, `finder.rs:157-162`); the equalizer's config-push observer has an explicit equality guard "to keep this notify from looping back" (`equalizer/view.rs:245-251`); `subscribe_liked_updates` only notifies when the recomputed liked-state actually flips (`models.rs:847-868`). Combined with a house style of **draining a whole burst of notifications before recomputing once** (palette/finder's `while receiver.try_recv().is_ok() {}`, the theme watcher's identical `while let Ok(event) = rx.try_recv() {}`), this is the same discipline zeron's dirty-flag gating (§1) independently converges on.
- A custom allocation-counting harness for regression-testing decode/convert-path allocations in tests (`src/test_support.rs:34-92`, a thread-local `CountingAllocator` wrapping the global allocator, used to assert hot audio paths don't allocate) — not GPUI-specific, but a good instinct: *measure* allocation counts in tests rather than assuming.
- A **noted gap**, not a strength: no dedicated text-shaping/measurement cache anywhere — `ShapedLine`s (e.g. the EQ graph's axis labels) are reshaped every `prepaint` call rather than cached keyed on text+style, unlike the discipline applied everywhere else (image cache, curve cache, row cache).

### zeron
- **`PulseClock`** (§4) is the headline finding: a documented, measured fix (36% CPU → shared 30fps clock) for the single most likely performance trap in "heavy animation" GPUI apps — many independently-animating `with_animation` elements.
- **`syntax_cache.rs`**: a bounded, hit/miss-instrumented, content-addressed cache for syntax-highlighted documents — exact numbers: `MAX_DOCUMENTS = 96`, `MAX_RETAINED_BYTES = 24MiB` (`syntax_cache.rs:14-16`), keyed by a SHA-256 of the source text (not by file path, so identical content anywhere hits the same cache slot) plus a query-generation number, with colors/GPUI concerns explicitly kept *out* of the key:
  ```rust
  // crates/ui/src/syntax_cache.rs:1-4 (doc) + key shape
  //! Colors and GPUI runs deliberately stay outside this cache so appearance
  //! changes recolor existing spans without parsing again.
  pub struct DocumentHighlightKey { pub language: LanguageId, pub content_hash: [u8; 32], pub query_generation: u32 }
  ```
  classic LRU eviction (`recency: VecDeque`, touch-on-access, evict-oldest-while-over-byte-budget) with `hits`/`misses` counters asserted directly in unit tests — directly relevant to a docs reader doing code-block syntax highlighting.
- **Image handling bounds decode size analytically, not by downscaling after the fact**: raster target dimensions are computed from viewport size × DPI × a fixed pixel budget *before* decode, not decoded at native resolution and shrunk afterward:
  ```rust
  // crates/ui/src/image_media.rs (elided)
  const PREVIEW_PIXELS: usize = 1024 * 1024;
  const MAX_RASTER_SIDE: f64 = 4096.0;
  const GPUI_SVG_SCALE: f64 = 2.0; // gpui's SvgRenderer rasterizes SVG images at 2x their declared size
  ```
  with explicit atlas eviction on release (`release_media`, calling `gpui::ImageSource::Image(image).evict(None, cx)` via `cx.defer`) — the same "release GPU-side memory explicitly, don't wait for Drop" discipline as hummingbird's `cx.drop_image`.
- **A regression test that pins gpui's own `Entity::cached()` behavior**: a Linux lightbox test explicitly asserts a sibling entity wrapped in `.cached(StyleRefinement::default())` does *not* re-render while an unrelated lightbox zooms (`image_viewer.rs:456,480,529`) — i.e. zeron doesn't just use gpui's render-caching wrapper, it regression-tests that the wrapper is actually preventing the re-render it's supposed to prevent.
- **Hover-state cost isolation**: the `HoverFades` thread-local (§4) explicitly avoids being a `Global` "so the many free-function element builders... can blend colors without threading `cx` through every signature" — a deliberate API-ergonomics-vs-architecture-purity tradeoff, justified because it's UI-thread-only.
- Appshot capture bounds memory explicitly rather than trusting OS capture size: `MAX_CAPTURE_DIMENSION = 8_192`, `MAX_CAPTURE_PIXELS = 32M`, `MAX_CAPTURE_RGBA_BYTES = 128MB` (`crates/ui/src/appshots.rs:50-55`) — a general lesson (bound any user-triggered capture/import path) more than a GPUI-specific one.
- **No frame-budget/profiling instrumentation exists anywhere** in `crates/ui/src` (no `tracing::instrument`, no frame-budget hooks) — performance is managed entirely structurally (throttled clocks, bounded/byte-budgeted caches, `Task`-drop cancellation), not through measurement infrastructure. Text shaping likewise has **no** bespoke cache — the team explicitly decided to rely on gpui's native `list()` measurement/memoization and revisit only "if cold-open of huge transcripts measures slow" (their own `ARCHITECTURE.md` open question), instead of pre-emptively building a shaping cache.

### Verdict
- **ADOPT**: `PulseClock`-style shared/leased animation clocks — this is the single highest-leverage performance pattern for a heavy-animation docs reader and directly prevents the exact failure mode zeron measured.
- **ADOPT**: `syntax_cache.rs`'s design (bounded LRU by document count *and* byte budget, content-hash key with colors excluded, hit/miss stats asserted in tests) — a docs reader will very likely syntax-highlight code blocks and hit the identical "don't re-lex on every theme toggle" problem; the exact numbers (96 docs / 24MiB) are a reasonable starting budget to copy.
- **ADOPT**: compute raster/decode target size analytically from viewport×DPI×pixel-budget *before* decoding (zeron's `image_media.rs`), and explicitly release GPU-side image memory on cache eviction (`cx.drop_image`/`ImageSource::evict`) rather than relying on `Drop` — both apps independently converged on this, so it's cheap, proven insurance for a docs reader rendering many inline images.
- **ADOPT**: equality-gate every `cx.notify()` whose upstream data can change without the *rendered* output changing (hummingbird's palette/finder, EQ config-push guard, liked-state flip; zeron's dirty-flag `apply_sessions_at`) — independently discovered by both apps, which is a strong signal it's load-bearing, not incidental style.
- **ADOPT**: bound any capture/import size explicitly (appshots' `MAX_CAPTURE_*` constants) if the docs reader ever imports external images/screenshots/pasted content.
- **ADOPT**: write at least one regression test that pins a caching/memoization wrapper's actual effect (zeron's `Entity::cached()` no-rerender assertion) rather than only testing the feature it protects — cheap to write, catches silent cache-defeating regressions.
- **ADAPT**: hummingbird's allocation-counting test harness (`CountingAllocator`) — worth adapting only for a specific hot path the docs reader identifies as allocation-sensitive (e.g. incremental markdown re-parse on keystroke); not needed as general infrastructure.
- **Deliberate non-pattern worth copying**: neither app pre-built a text-shaping cache; both rely on gpui's native shaping and only invalidate/remeasure on real changes (content-width, typography generation). Don't build a bespoke shaping cache pre-emptively — start with gpui's own measurement/memoization and add a cache only if profiling actually shows it's needed, exactly as zeron's own architecture doc argues.

---

## Summary table

| # | Topic | ADOPT from | Headline pattern |
|---|---|---|---|
| 1 | State architecture | zeron (structure) + hummingbird (router + granularity lesson) | One immutable `Entity<AppState>` + pure `view` derivation module + typed-event narrowcasting + dirty-flag gating; `cx.subscribe`-as-router for navigation |
| 2 | Async | both | `cx.spawn` + `cx.background_executor().spawn` + `entity.update(...,cx.notify())`; viewer-gated poll rate; test-scheduler-aware timers; dropped-`Task`/generation-counter cancellation; `OnceLock` background→paint bridge |
| 3 | Lists | zeron (structure) + hummingbird (row cache) | `ListState` for variable-height content, `splice` vs `remeasure_items`; content-addressed `ViewportAnchor` scroll restore; `MessageRail` as TOC minimap; per-index child-`Entity` row cache; real tree keyboard nav |
| 4 | Animation | zeron + hummingbird (independently convergent) | `PulseClock` leased shared clock (many small animations) + `cx.on_next_frame` self-terminating RAF loop (one view's continuous motion) + one boolean OR-gate for `request_animation_frame`; named `MotionSpec` catalogue; manual `HoverFades`/`SmoothScrollFollow` tweens (not `with_animation`); real critically-damped spring for measured-geometry transitions |
| 5 | Custom drawing | hummingbird (technique) + zeron (verification + blur/fade) | Full `impl Element` + `PathBuilder`/`paint_path`; memoized curve cache inside the element; `edge_fade`/`frost` paint-order bug fixes; pixel-sampling assertions for blur/shadow effects |
| 6 | Theming | zeron (structure + a11y) + hummingbird (dev UX) | Source-neutral theme model + `ensure_contrast` + orthogonal glass/opaque axis + `NSAppearance`/vibrancy sync + `refresh_windows` cache-bust; hot-reloadable theme files; flat semantic-token struct; tabular figures |
| 7 | Window chrome | zeron (drag safety) + hummingbird (resizable primitive) | Arm-on-mousedown/trigger-on-mousemove drag guard (native double-click-zoom safe); generic `Element`+`Entity<Pixels>` resizable pane; tween from currently-painted value |
| 8 | Keyboard | hummingbird (data) + zeron (customization + focus) | JSON keymap + platform predicates; pure `menu_step`/`classify_key` functions; live conflict detection via `cx.intercept_keystrokes`; dispatch-tree-aware focus restoration |
| 9 | Overlays | zeron (exit animation) + hummingbird (unified triad) | `deferred(anchored())` + `PopupState`/wall-clock exit-progress close-race handling; one shared dismissal-triad component (click-row/outside/Escape) reused by every overlay kind; notice-chip vs toast vs OS-banner triage by content type |
| 10 | Testing/screenshots | zeron | Two-layer: `#[gpui::test]`+virtual clock (logic) + real-window/real-OS-capture example fixtures (visual) |
| 11 | Performance | zeron + hummingbird (independently convergent) | `PulseClock`; content-hash-keyed syntax-highlight cache with byte budget; analytical raster-size budgeting + explicit GPU-texture eviction; equality-gated `cx.notify()` as house style in both apps |
