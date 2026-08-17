# docs/GUI-PLAN.md — lindsey Rev 1: the GPUI flagship shell

> **Status:** Frozen plan, 2026-07-18.
> **Crate:** `lindsey` (`workspace/gui`), GPUI `0.2.2` @ zed rev `1d217ee`, gpui-component `0.5.2` @ rev `c112e7b`.
> **Feeds from:** `docs/research/librarification/03-gui-audit.md` (live-code audit), `15-gui-references.md` (view catalog + prior art), `18-desktop-toolchains.md` (packaging), `docs/LIBRARIFICATION-PLAN.md` §13–§15, §18, GD-18/GD-22/GD-34, Waves 4–5.
> **Supersedes:** the view-roadmap sketch in `15-gui-references.md` §2/§5 — that document remains the *reference study*; this document is the *implementation spec*.
>
> Every API named in this plan was verified against the pinned checkouts on disk
> (`gpui` at `crates/gpui/src/elements/animation.rs`, `path_builder.rs`, `elements/uniform_list.rs`, `window.rs`, `styled.rs`;
> `gpui-component` at `crates/ui/src/animation.rs`, `dock/`, `table/`, `list/`, `tree.rs`, `virtual_list.rs`, `skeleton.rs`, `highlighter/`).
> Nothing here assumes an API the pins do not have.

---

## Part 0 — Mission and doctrine

### §0.1 Mission

lindsey is the start and end of the nudox developer experience (LIBRARIFICATION-PLAN §18). This revision commits it to three non-negotiable qualities:

1. **Nothing ever blocks.** The GPUI thread does layout, paint, input, and state mutation — and *nothing else*. Every byte of I/O, every parse, every index query, every embedding happens off-thread and arrives as a streamed event. There is no code path in the GUI crate that can stall a frame on a lock, a socket, a file, or a large serde pass.
2. **Motion is a system, not a garnish.** Every state change in the app is choreographed — including the subtle ones: a count ticking up, a row appearing, a status dot breathing, a dock settling on a spring. Motion is defined once as a token vocabulary (§5) and consumed everywhere; it degrades gracefully under load and disappears entirely under reduced-motion.
3. **Documentation streams in.** A symbol page is never "loading" — it is *arriving*. The header paints within one frame of local data, sections materialize as they are parsed, syntax highlighting upgrades code blocks in place, and references/timeline fill their tabs while you read. The protocol (§9) is designed so partial content is always valid content.

### §0.2 Where this plan sits in the program

LIBRARIFICATION-PLAN Wave 4 builds `client` (traits, `Routed`, SyncEngine, IrohBlobsTransport) and the REGISTRY; Wave 5 builds the GUI over it. This plan is the Wave-5 GUI lane expanded to implementation depth, **plus** a front-loaded foundation (M0–M3, §27) that is deliberately independent of `client` so it can start today against the current HTTP backend and swap transports later. The seam that makes this legal is the *stream protocol* (§8–§9): views and stores speak `DocEvent`/`SearchEvent` streams from day one; whether those streams originate from the current axum server, from a client-side chunker, or from the Wave-4 `Routed` stack is invisible above the bridge.

### §0.3 Standing decisions (LD-1…LD-20)

These are frozen. Reopening one requires editing this document.

- **LD-1 Stores-over-client (GD-22).** `ProjectStore`, `SearchStore`, `SymbolStore`, `RegistryStore`, `JobStore`, `NavHistory` are GPUI entities; views subscribe to stores; Workspace is a layout shell with zero I/O. Views never construct HTTP clients, never touch channels directly, never spawn.
- **LD-2 Three-runtime topology (§2).** GPUI foreground (UI), GPUI background executor (CPU work), client Tokio runtime (I/O, indexes, iroh). Bridged by bounded channels only. `async-compat` and the throwaway Tokio bootstrap in `backend.rs:146` are deleted.
- **LD-3 Streams, not responses.** Every store→client call that can return more than a scalar returns a stream handle with a generation token and a cancellation token. Request/response is the degenerate one-event stream.
- **LD-4 Two-tier motion.** Declarative `Animation`/`with_animation` for one-shot entrances and loops; retained `Motion<T>` springs (our code, §4.2) for interruptible, physical animation. No third mechanism, no ad-hoc timers.
- **LD-5 Paint-only animation by default.** Animate opacity, color, and small leaf offsets. Layout-mutating animation (width/height) is allowed only for dock resize, collapse/expand, and progress bars — surfaces whose subtree relayout is the point.
- **LD-6 Everything long is virtualized.** Any list that can exceed 32 rows renders through `uniform_list`, gpui-component `Table`, or `virtual_list`. No exceptions, including logs, refs, deps, and search results.
- **LD-7 RenderModel, versioned.** Symbol pages render typed `render.v1` nodes (§9.2). `SymbolEntry.kind: serde_json::Value` and the `@type`/`_type` sniffing die. Unknown node kinds render as a visible fallback chip, never a panic and never silence.
- **LD-8 Trust is chrome.** Every symbol hit, tab, and package row carries a provenance badge (trusted-local / synced / remote / stale). The trust colors are theme tokens (§10.4), not inline hex.
- **LD-9 Fixtures are debug-only.** `#[cfg(debug_assertions)]` plus a `--fixtures` flag requirement; release builds start empty with designed empty states.
- **LD-10 Pins are exact.** `gpui-component` gains `rev = "c112e7b..."` in Cargo.toml (it is unpinned today — audit §11.11). Upgrades are deliberate events with a migration commit.
- **LD-11 Latency budgets are CI-gated.** Frame p95 ≤ 8.3 ms during search-typing load; first search rows < 10 ms after debounce; symbol header < 50 ms local; §25 defines the harness.
- **LD-12 The GUI process embeds no GPL.** snix never links (GD-18). License check in CI.
- **LD-13 Keyboard completes every loop.** Every mouse path has a keyboard path; `?` overlay documents all of them; bindings live in one keymap module (§13.6, Appendix B).
- **LD-14 One theme system.** gpui-component `Theme` + a `NudoxThemeExt` global for trust/lineage/motion tokens. No inline colors in views.
- **LD-15 Stale-while-revalidate.** A `StreamSlot` (§8.2) never discards a displayed value because a refresh started; old content dims to 70 % opacity behind a shimmer, new content crossfades in.
- **LD-16 Errors are states, not dialogs.** Every slot has a designed error state with a retry affordance; toasts are for *completions of background work*, never for input-loop errors.
- **LD-17 Reduced motion is first-class.** `MotionScale` (§6) multiplies every duration; `0.0` means instant-cut everywhere; honors OS setting when detectable and a settings override always.
- **LD-18 No unbounded channels, no detached fire-and-forget tasks in views.** Every spawned task is owned by a store (`Task<()>` fields or a `TaskTracker`), so dropping the store cancels its work. The audit's leak class (detached tab subscriptions, workspace.rs:301) is eliminated structurally.
- **LD-19 Element IDs are stable and namespaced.** Animation identity, scroll state, and list state depend on `ElementId`; the id scheme (`"search.results.row", stem_id`) is part of code review.
- **LD-20 The shell is one window (v1).** Multi-window (detached symbol pages) is P3; nothing in the store layer may assume a single window, but no view work is spent on it now.

### §0.4 Reading order for implementers

A beginner should read Part I (how GPUI frames actually work — everything else derives from it), then implement milestones in §27 order, referring back to Parts II–VI as each milestone names them. Parts II–V are *kernels* (motion, async, design system, stores): small, fully-specified, testable. Part VI is the screen catalog that composes them.

---

# Part I — The GPUI execution model, and what "pushing its limits" means

## §1 Frame anatomy and budgets

### §1.1 How a frame happens

GPUI is an immediate-mode-over-retained-entities framework. The retained side is the entity graph: `Entity<T>` handles owned by `App`, mutated via `entity.update(cx, ...)`, observed via `cx.observe`/`cx.subscribe`, and invalidated via `cx.notify()`. The immediate side is `Render::render`, which rebuilds an element tree for the invalidated views each frame; Taffy computes layout; paint produces GPU primitives — quads (with corner radii, borders, shadows, solid/gradient/pattern backgrounds — `color.rs` `BackgroundTag::{Solid, LinearGradient, PatternSlash, Checkerboard}`), lyon-tessellated paths (`path_builder.rs`: stroke with width + dash arrays, fills, béziers, arcs), glyph sprites from the shaped-text cache, and images/SVGs (with `TransformationMatrix` rotation/scale on SVG). Metal renders on macOS, blade/Vulkan on Linux.

Consequences that drive this whole plan:

1. **Nothing re-renders unless notified.** A static screen costs zero. Animation is therefore *self-scheduled invalidation*: something must call `cx.notify()` (or `window.request_animation_frame()`, which is exactly `on_next_frame(|_, cx| cx.notify(entity))` — `window.rs:2169`) each frame for as long as motion runs.
2. **Invalidation granularity is the entity.** If the root Workspace entity is notified at 120 Hz, the whole window re-renders at 120 Hz. If a 24×24 px spinner entity is notified, only it does. Rule: **animate at the leaf**. Every continuously-animating element in lindsey is its own small entity or an `AnimationElement` leaf, never a notify on Workspace.
3. **Layout is the expensive phase.** Taffy is fast but a full-window flex relayout with wrapped text is the dominant cost. Paint-only changes (opacity — `styled.rs:746` applies to the element and its children at paint; colors; existing quads with new params) skip none of GPUI's phases structurally, but keep the tree shape identical so layout results are cheap and text shaping hits cache. Changing element *structure* (adding/removing children) or *geometry* (sizes) is what hurts. LD-5 exists because of this.
4. **Text shaping is cached by (text, font, size).** `SharedString` reuse matters: building `format!` strings in `render` defeats the cache and allocates per frame. All hot-path strings in lindsey are cached in store state as `SharedString` at *update* time, not render time.
5. **`with_element_state`** (`window.rs:3444`) gives an element access to per-element retained state keyed by `GlobalElementId` — this is how `uniform_list` keeps scroll offsets and how our custom elements (graph canvas, timeline) keep pan/zoom and hit-test caches without polluting stores.

### §1.2 The frame budget

Target: **120 Hz on ProMotion displays, 60 Hz floor everywhere** — 8.33 ms budget, engineered as:

| Phase | Budget | Enforcement |
|---|---:|---|
| Input dispatch + store `update`s | 1.0 ms | Stores mutate state only; batching rule §7.4 caps notifies at one per store per wake |
| `render` (element tree build) | 2.0 ms | No allocation-heavy work, no `format!`, no sorting, no filtering in render — precomputed in stores |
| Layout (Taffy + text) | 2.5 ms | Virtualization (LD-6); stable tree shapes; fixed row heights in lists |
| Paint + submit | 1.8 ms | Primitive counts bounded (graph LOD §18.5) |
| Headroom | 1.0 ms | Compositor jitter, GC of dropped entities |

The budget is *observed* by the in-app frame HUD (§25.2) and *gated* by the perf harness (§25.3). When a frame overruns, the HUD attributes it (render vs layout counters), and the motion system sheds load (§6.2) before anything visibly hitches.

### §1.3 The verified capability inventory (what "the limits" are)

What the pinned GPUI **has**, and how far lindsey pushes each:

| Capability (verified location) | Pushed to |
|---|---|
| `Animation` + `AnimationExt::with_animation/with_animations`, easings `linear/quadratic/ease_in_out/ease_out_quint/bounce/pulsating_between` (`elements/animation.rs`) | Entire declarative tier (§4.1): entrances, shimmers, pulses, staggered chains via `with_animations` |
| `Styled::opacity` — subtree opacity at paint (`styled.rs:745`) | Crossfades, dimming for stale content, entrance fades — the workhorse of LD-5 |
| `PathBuilder` stroke/fill/dash/bézier/arc + transform/translate/scale/rotate (`path_builder.rs`) | Graph edges with animated dash-flow, timeline splines, sparkline charts, focus rings |
| `canvas()` element (`elements/canvas.rs`) | Graph view, timeline, frame HUD — custom paint with element-state caches |
| `uniform_list` (fixed-height virtualization, `elements/uniform_list.rs`) + `list` (variable-height) | Every long list (LD-6); 100 k-row search results scroll at full rate |
| `anchored()` + `deferred()` (`elements/anchored.rs`, `deferred.rs`) | Overlays: omni-search, palette, popovers, context menus, quick-peek — painted above all panes with correct hit-testing |
| `WindowBackgroundAppearance::{Transparent, Blurred}` (`platform.rs:1713`) | Vibrancy on overlay scrims and (optionally) the whole window on macOS; opaque fallback is automatic where unsupported |
| `on_next_frame` / `request_animation_frame` (`window.rs:2159/2169`) | The retained motion tier (§4.2): springs advance per frame only while unsettled |
| `with_element_state` (`window.rs:3444`) | Pan/zoom state, hit grids, glyph-run caches in custom elements |
| Box shadows, 2-stop linear gradients, `pattern_slash`, checkerboard fills (`color.rs`) | Elevation tokens (§10.5), progress textures, "delegated" trust hatching |
| SVG with `Transformation` (rotate/scale — `elements/svg.rs:44`) | Spinner rotation, chevron twirls, status glyph pivots — true transforms where GPUI allows them |
| Focus system, key contexts, `actions!`, keymaps | The complete keyboard loop (LD-13) |
| Inspector + profiler modules (`inspector.rs`, `profiler/`) | Dev-mode element inspection; profiler feeds the HUD |

What it **does not have**, and the honest workaround:

| Missing | Consequence | Our approach |
|---|---|---|
| Free 2-D transform (translate/scale/rotate) on `div` | No true scale/slide compositing for arbitrary elements | Slides = animated `top/left/margin` on *small leaf nodes* (bounded relayout); "scale" feel approximated with opacity + 2–4 px rise; real rotation only on `svg()` |
| Custom shaders | No blur-behind-element, no shader glow | Elevation via layered shadows + gradients; glow via `pulsating_between` alpha on a gradient quad; window-level blur via `Blurred` background |
| Built-in spring physics | Duration-easing only | We build the spring kernel (§4.2) — ~120 lines, fully specified below |
| Element-level screenshot/golden testing | No pixel CI | State-machine tests + deterministic `#[gpui::test]` executors + manual visual QA checklist (§28) |
| Shared-element transitions | No magic-move between screens | Choreographed handoff: outgoing fades 80 ms, incoming header rises 160 ms with matched typography so the eye reads continuity (§5.3) |

## §2 The Never-Block Contract

### §2.1 Three runtimes, one direction of flow

```
┌──────────────────────────── GPUI main thread ─────────────────────────────┐
│  input → actions → store.update(cx) → cx.notify → render → layout → paint │
│  MAY: mutate store state, read caches, schedule motion, emit events       │
│  MAY NOT: touch a socket/file/DB, hold a contended lock, parse > 64 KiB,  │
│           serialize/deserialize payloads, compute embeddings/diffs        │
└──────────────┬────────────────────────────────────▲───────────────────────┘
     commands  │ (bounded flume channels)           │ events (drained §7.3)
┌──────────────▼──────────────┐      ┌──────────────┴──────────────────────┐
│  GPUI background executor   │      │  client runtime (Tokio, 2–4 threads)│
│  (smol thread pool)         │      │  owned by the `client` crate        │
│  fuzzy scoring · markdown → │      │  HTTP/INDEX · SyncEngine · iroh     │
│  RenderNodes · tree-sitter  │      │  rusqlite thread · tantivy ·        │
│  highlight · graph layout · │      │  qdrant-edge · EmbeddedForge jobs   │
│  diff compute               │      │  (LIBRARIFICATION §15)              │
└─────────────────────────────┘      └─────────────────────────────────────┘
```

- The **foreground** is sacred (§2.2 rules).
- The **background executor** (`cx.background_executor()`) is for pure CPU work on GUI-owned data. It is smol; it needs no Tokio types. Work units are ≤ 10 ms or internally yielding, so shutdown stays prompt.
- The **client runtime** is a Tokio multi-thread runtime constructed inside the `client` crate (LIBRARIFICATION §15.3 already mandates this for iroh: *"iroh runs on a dedicated Tokio runtime inside client with a channel bridge — never on the GPUI thread"*). lindsey never sees Tokio types. The bridge is `flume` (MPMC, both sync and async ends, runtime-agnostic), bounded everywhere.

This kills the audit's runtime tangle (03-gui-audit §1.4/§11.1): no more `async-compat`, no more throwaway Tokio runtime for reqwest construction, no more `std::thread` + 200 ms `async_io::Timer` polls.

### §2.2 Foreground rules (enforced, not aspirational)

1. **No blocking calls.** `std::fs`, `std::net`, `reqwest::blocking`, `rusqlite`, `std::thread::sleep`, channel `.recv()`/`.send()` (blocking forms) are forbidden in `workspace/gui`. Enforcement: `scripts/lint-gui-no-block.sh` greps the crate for the deny-list and runs in CI (§25.4); clippy `disallowed_methods` config for the ones clippy can see.
2. **Bounded channels only.** Every `flume::bounded(n)` capacity is written down in the channel registry (Appendix C) with its overflow policy: *coalesce* (progress: keep-latest), *drop-oldest* (logs), or *backpressure* (sync commands — the client side awaits).
3. **One notify per store per wake.** The drain loop (§7.3) pulls every immediately-available event before a single `update` + `notify`. A burst of 500 log lines is one re-render, not 500.
4. **Tasks are owned.** Stores keep their spawned tasks (`Task<()>` fields, or a `Vec<Task<()>>` for pools). Dropping the store drops the tasks; GPUI cancels a dropped `Task`. Nothing is `.detach()`ed except app-lifetime observers created in `main.rs`.
5. **Big values move, small values copy.** Events crossing the bridge carry `Arc<[T]>`/`SharedString`/`Bytes` for payloads; the foreground clones handles, never buffers.

### §2.3 Cancellation and generations

Two mechanisms, used together:

- **`Gen(u64)`** — a monotonically increasing token per *logical query slot* (the search box has one, each symbol tab has one, each package browser pane has one). Every stream event is stamped with the `Gen` it answers; the store drops events whose gen ≠ current. This makes staleness impossible by construction, replacing the audit's "no request cancellation" hazard (03-gui-audit §1.4).
- **`CancellationToken`** (tokio-util, lives client-side) — carried in every `StreamHandle`; the store cancels it when superseding or dropping the slot, so the client stops burning I/O on answers no one wants. GUI-side this is opaque: `handle.cancel()`.

```rust
pub struct StreamHandle {
    pub generation: Gen,
    canceller: Box<dyn Fn() + Send>,   // wraps client-side CancellationToken
}
impl Drop for StreamHandle { fn drop(&mut self) { (self.canceller)(); } }
```

Dropping the handle cancels the work. Stores hold the current handle per slot; assigning a new one drops (cancels) the old. That single ownership rule *is* the cancellation policy.

## §3 Entity graph hygiene

The audit found detached subscriptions pinning closed tabs (03-gui-audit §11.9). The structural fix:

1. **Subscriptions live in the subscriber.** Every view/store keeps `_subs: Vec<Subscription>`. `subscribe(...).detach()` is forbidden in the crate (same lint script).
2. **Tabs are `WorkspaceItem`s** (§13.4): the pane owns `Vec<ItemSlot { item: AnyView, _subs: Vec<Subscription> }>`; closing a tab drops the slot, the subs, the view, and (via LD-18) its in-flight streams — one drop does everything.
3. **Weak by default across long edges.** Long-lived observers (e.g., LogStore → status bar) hold `WeakEntity` and no-op when upgrade fails.
4. **Entity count is observable.** The debug HUD shows live entity and subscription counts; the leak test (§28.3) opens/closes 200 tabs and asserts the count returns to baseline.

---

# Part II — The motion system

## §4 Architecture: two tiers, one vocabulary

### §4.1 Declarative tier — GPUI `Animation` (entrances, loops, staggers)

Used when the animation is a *function of time since mount* and needs no interruption semantics: row entrances, shimmer, pulse, spinner, toast slide-in. It is zero-maintenance: `AnimationElement` self-invalidates until done (or forever with `.repeat()`).

```rust
use gpui::{Animation, AnimationExt, ease_out_quint, pulsating_between};
use std::time::Duration;

// Entrance: fade + 6px rise. `t` is eased 0..1.
fn rise_in<E: IntoElement + Styled + 'static>(el: E, id: impl Into<ElementId>, motion: &MotionTokens) -> impl IntoElement {
    let dur = motion.scaled(Duration::from_millis(160)); // MotionScale-aware (§6)
    el.with_animation(id, Animation::new(dur).with_easing(ease_out_quint()), |el, t| {
        el.opacity(t).mt(px(6.0 * (1.0 - t)))            // leaf-local: bounded relayout, LD-5
    })
}

// Shimmer for skeletons — the *only* infinite loop besides spinners and status pulses.
fn shimmer<E: IntoElement + Styled + 'static>(el: E, id: impl Into<ElementId>) -> impl IntoElement {
    el.with_animation(id, Animation::new(Duration::from_millis(1200)).repeat()
        .with_easing(pulsating_between(0.35, 0.75)), |el, alpha| el.opacity(alpha))
}
```

gpui-component's `Transition` (verified: `animation.rs:97` — `fade/slide_x/slide_y/width/height` + `apply(el, id)`) is the same tier with a nicer builder; we use it where its five effects suffice and our helpers where we need composed easings or `with_animations` chains.

**Stagger** uses `with_animations` (chain of `Animation`s: a linear "delay" segment then the entrance) or, simpler and preferred, a per-index delay folded into a custom easing:

```rust
/// Delay the start of an eased animation by `delay_frac` of its duration.
pub fn delayed(delay_frac: f32, ease: impl Fn(f32) -> f32) -> impl Fn(f32) -> f32 {
    move |t| if t <= delay_frac { 0.0 } else { ease((t - delay_frac) / (1.0 - delay_frac)) }
}
// Row ix in a fresh result set: total dur = 160ms + cap; delay = 16ms * min(ix, 8)
```

**Entrance identity rule (critical with virtualization):** `uniform_list` mounts and unmounts rows on scroll; keying an entrance animation to the row would replay it on every scroll-back. Entrances are keyed to the *data generation*, not visibility: `ElementId` = `("search.row.enter", generation.0, ix)` and rows only wrap themselves in the animation while `gen_age() < 400 ms` (store records the `Instant` each gen's first page landed). After that window, rows render bare. Scrolling never animates; only *new results* do.

### §4.2 Retained tier — the `Motion<T>` spring kernel (interruptible, physical)

For anything whose target can change mid-flight — dock widths, collapse/expand, pan/zoom, scroll-to, progress values, count tickers, graph node positions — duration-easing is wrong (it restarts; velocity discontinuities read as cheap). We use critically-damped springs with velocity preservation. This is the one piece of motion machinery we own. Complete, drop-in implementation:

```rust
// workspace/gui/src/motion/spring.rs
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug)]
pub struct Spring { pub stiffness: f32, pub damping: f32, pub mass: f32 }

impl Spring {
    /// Default UI spring — near-critical damping, settles in ~350 ms.
    pub const DEFAULT: Spring = Spring { stiffness: 459.0, damping: 41.6, mass: 1.0 };
    /// Overlays, toasts — faster attack, settles ~220 ms.
    pub const SNAPPY:  Spring = Spring { stiffness: 1225.0, damping: 67.9, mass: 1.0 };
    /// Progress bars, graph settle — soft, settles ~600 ms.
    pub const GENTLE:  Spring = Spring { stiffness: 145.0, damping: 23.4, mass: 1.0 };
}

/// A spring-animated scalar. Views own these in their state.
#[derive(Clone, Debug)]
pub struct Motion {
    value: f32,
    velocity: f32,
    target: f32,
    spring: Spring,
    last_tick: Option<Instant>,
    epsilon: f32,
}

impl Motion {
    pub fn new(value: f32, spring: Spring) -> Self {
        Self { value, velocity: 0.0, target: value, spring, last_tick: None, travel: 0.0 }
    }
    /// Retarget without touching velocity — this is what makes interruption smooth.
    pub fn animate_to(&mut self, target: f32) { self.target = target; }
    /// Jump instantly (reduced motion, initialization).
    pub fn snap_to(&mut self, target: f32) {
        self.target = target; self.value = target; self.velocity = 0.0; self.last_tick = None;
    }
    pub fn value(&self) -> f32 { self.value }
    pub fn target(&self) -> f32 { self.target }
    pub fn is_settled(&self) -> bool {
        self.velocity.abs() < self.epsilon && (self.value - self.target).abs() < self.epsilon
    }
    /// Advance to now. Returns true while still moving (caller must request another frame).
    /// Fixed 240 Hz sub-steps => framerate-independent, stable at any dt.
    pub fn tick(&mut self, now: Instant) -> bool {
        let dt = match self.last_tick.replace(now) {
            Some(prev) => (now - prev).min(Duration::from_millis(32)), // clamp stalls: no teleporting
            None => Duration::from_millis(8),
        };
        let mut remaining = dt.as_secs_f32();
        const H: f32 = 1.0 / 240.0;
        while remaining > 0.0 {
            let h = remaining.min(H);
            let f_spring = -self.spring.stiffness * (self.value - self.target);
            let f_damp = -self.spring.damping * self.velocity;
            self.velocity += (f_spring + f_damp) / self.spring.mass * h; // semi-implicit Euler
            self.value += self.velocity * h;
            remaining -= h;
        }
        if self.is_settled() { self.snap_settle(); false } else { true }
    }
    fn snap_settle(&mut self) { self.value = self.target; self.velocity = 0.0; self.last_tick = None; }
}
```

> **Correction (2026-07-27, measured).** The constants above are the retuned
> ones. As originally specified — 380/38, 560/46, 170/24 with an *absolute*
> `epsilon: 0.05` — settle time grew with travel distance, because the time for
> a linear spring to cross a fixed threshold scales with the log of the distance
> it covers. `DEFAULT` took **758 ms** over a 320 px dock but 517 ms over a
> 1-unit chevron: two surfaces sharing one preset disagreed about what that
> preset felt like, and the dock case breached §5.4's 700 ms cap. The fix is a
> *relative* rest threshold (0.5 % of travel, with the rest velocity derived
> from it so the two agree), which makes settle time scale-invariant, plus
> stiffnesses re-solved so each preset hits its advertised time. Verified by
> `presets_settle_at_advertised_time_regardless_of_travel`, which asserts the
> same wall-clock across travels of 1, 320 and 4000 units.

**The render-loop contract** (this is the entire integration — no registry, no global ticker; motion state is view-local and dies with the view):

```rust
impl Render for SearchPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = Instant::now();
        let mut animating = false;
        animating |= self.width.tick(now);          // Motion for dock width
        animating |= self.count_ticker.tick(now);   // Motion for result-count number
        if animating && !cx.theme_ext().reduced_motion() {
            window.request_animation_frame();       // notify *this entity* next frame only
        }
        // ... build elements using self.width.value(), etc.
    }
}
```

Because `request_animation_frame` notifies only the rendering entity (`window.rs:2169`), a settling dock animates *alone*: the symbol page, log panel, and everything else render zero frames. This is the leaf-animation rule made mechanical.

`Motion2` (a pair of `Motion`s with a shared `tick`) covers pan offsets and node positions; `MotionColor` lerps `Hsla` through the same interface for smooth status-color changes (implemented as 4 scalar motions on h/s/l/a with hue wrap-around on shortest arc).

### §4.3 Choreography — who triggers what

Motion is *never* triggered from render (render observes; it may tick, not retarget). Triggers live in exactly three places:

1. **Actions/input handlers** — user intent: `ToggleSidebar` → `self.width.animate_to(0.0 | 320.0)`.
2. **Store event subscriptions** — data arrival: `SearchEvent::Page { generation, .. }` → record `gen_arrival` (drives entrance windows), `count_ticker.animate_to(total as f32)`.
3. **Focus/hover callbacks** — micro-feedback: hover tints and press-sink (§5.1) via GPUI's built-in `hover:`/`active:` style states where possible (zero-cost, no notify), `Motion` only where the built-ins can't express it (e.g., a hover that reveals a secondary row of actions with a slide).

## §5 The motion vocabulary (every animation in the app)

Single source of truth. Every entry is a named token in `motion/tokens.rs`; views refer to tokens, never to raw durations. **Durations are pre-`MotionScale` (§6).**

### §5.1 Micro-feedback (continuous polish — the "subtle ones")

| Token | Trigger | Mechanism | Spec |
|---|---|---|---|
| `hover.tint` | pointer enter/leave any interactive row/button | GPUI `hover:` style state (no notify) | background +4 % lightness, 0-cost instant; where a fade is wanted (list rows), `MotionColor`, 80 ms equiv. |
| `press.sink` | mouse-down on buttons/chips | `active:` style state | background −6 % lightness + 1 px inset shadow; release returns instantly |
| `focus.ring` | keyboard focus arrival | declarative, 140 ms `ease_out_quint` | ring opacity 0→1 + ring inset 3→1.5 px (painted via border, leaf-only) |
| `count.tick` | any displayed count changes (results, refs, jobs) | `Motion` GENTLE on f32, rendered `round()` | numbers *roll* to their new value; ≤ 600 ms; skips to target if Δ > 500 |
| `status.breathe` | backend online dot, active-job dot | declarative `.repeat()` `pulsating_between(0.45, 0.9)` 2000 ms | alpha of an 8 px glow gradient behind the dot; at most 3 breathing elements on screen (§6.2) |
| `spinner` | any indeterminate wait ≥ 300 ms | gpui-component `Spinner` (SVG rotation) | never shown before 300 ms — avoids flicker on fast ops |
| `chevron.twirl` | tree/collapse toggles | `svg().with_transformation(rotate)` driven by `Motion` DEFAULT | 0→90°, true rotation (SVG supports matrices) |
| `scroll.glide` | keyboard-driven scroll-to (search selection, refs jump) | `Motion` DEFAULT on scroll offset via `UniformListScrollHandle` | preserves velocity when user chains ↓↓↓ |
| `tooltip.in` | 450 ms hover dwell | declarative 100 ms fade | opacity only; no rise (tooltips must feel weightless) |

### §5.2 Content arrival (streaming made visible)

| Token | Trigger | Mechanism | Spec |
|---|---|---|---|
| `skeleton.shimmer` | any `StreamSlot` in `Loading` with no prior value | `shimmer()` §4.1 on gpui-component `Skeleton` blocks | layout mirrors the real content's shape (§9.4) so arrival causes zero shift |
| `content.crossfade` | slot `Loading→Ready` with a prior value (stale-while-revalidate, LD-15) | two stacked layers, outgoing 70 %→0 over 100 ms, incoming 0→100 % over 140 ms | absolute-positioned overlap; heights equal by skeleton rule |
| `row.cascade` | new search/refs/deps result generation | `rise_in` + `delayed(ix)` §4.1 | 160 ms each, 16 ms stagger, cap 8 staggers; only within 400 ms gen window (§4.1) |
| `section.arrive` | each streamed doc section (§9) | `rise_in` 160 ms | sections arrive top-down; stagger emerges naturally from stream order |
| `highlight.sweep` | tree-sitter spans upgrade a code block | `MotionColor` per-token-class fade from `fg.muted` to final colors, 180 ms | color-only: zero relayout (§9.4 reserves geometry) |
| `progress.fill` | job/sync progress events | `Motion` GENTLE on fraction; bar width | never jumps backward; indeterminate = `pattern_slash` texture scrolling via declarative repeat |
| `badge.pop` | trust/status badge value changes | declarative 180 ms `bounce(ease_in_out)` | opacity 0→1 with a single soft overshoot on the badge only |
| `toast.in/out` | job completion, sync ready | `Motion` SNAPPY on x-offset + fade | in from right 16 px; auto-dismiss 5 s with a 100 % → 0 width hairline as the timer |
| `graph.settle` | force layout iterations arriving from background | `Motion2` GENTLE per node (≤ 200 animated; beyond = direct set) | layout thread streams positions; springs make discrete iterations look continuous |
| `edge.flow` | hovered graph edge / active lineage edge | `PathBuilder` dash array + animated dash offset (declarative repeat, 900 ms linear) | subtle directional flow communicates edge direction |

### §5.3 Navigation & shell

| Token | Trigger | Mechanism | Spec |
|---|---|---|---|
| `overlay.in` | omni-search, palette, modals open | `Motion` SNAPPY: opacity 0→1, rise 4→0 px; scrim fade 0→32 % | scrim uses `Blurred`-window vibrancy when available, plain alpha otherwise |
| `overlay.out` | close/escape | 120 ms `ease_in_cubic` fade (declarative) | outgoing overlays never rise — exit is always simpler than entry |
| `dock.slide` | sidebar/log/jobs toggle, drag-resize release | `Motion` DEFAULT on width/height px | the *one* sanctioned layout animation (LD-5); drag itself is direct 1:1, spring takes over on release/toggle |
| `tab.switch` | tab activation | `content.crossfade` 100 ms on pane body; active-tab underline slides via `Motion` SNAPPY on x/width | underline is a 2 px absolute-positioned quad — pure leaf motion |
| `tab.reorder` | drag tab | dragged tab follows pointer 1:1; siblings `Motion` DEFAULT to their new slots | the standard "tabs make room" choreography |
| `page.handoff` | opening a symbol from search/graph/refs | outgoing context fades 80 ms; incoming header `rise_in` 160 ms; body sections stream per §5.2 | our honest substitute for shared-element transitions (§1.3) |
| `nav.flash` | back/forward lands on an already-open tab | 240 ms tab-title background pulse (declarative, `bounce`) | orients the eye without a full transition |
| `empty.state` | any empty state mounts | `rise_in` 200 ms on illustration + copy | empty states are designed screens (LD-16), they deserve an entrance |
| `banner.drop` | offline/error banner | `Motion` SNAPPY on height 0→32 px + fade | height animation sanctioned: it must push content (that's the message) |

### §5.4 Budget rules for motion

- At most **one** layout-animating surface at a time (dock OR banner OR tab-reorder); triggers queue behind the active one (they're user-serial anyway).
- At most **3** infinite loops on screen (status dots); shimmer counts, spinners count. The `MotionTokens` global keeps a loop census; excess loops render static at their midpoint. (Enforced, not hoped: `MotionTokens::acquire_loop_slot()` returns a RAII permit.)
- Every declarative animation ≤ 240 ms; every spring settles ≤ 700 ms (GENTLE worst case). Anything longer is a progress indicator, not motion.

## §6 Reduced motion & load shedding

### §6.1 `MotionScale`

`ThemeExt.motion_scale: f32` — `1.0` default, `0.5` "reduced", `0.0` "off". Every helper multiplies durations by it; `Motion::animate_to` becomes `snap_to` when scale is `0.0` (checked inside the helpers, so call sites don't branch). Sources: settings (always wins) → OS reduced-motion (macOS `NSWorkspace` accessibility flag when the platform exposes it; else default). Every animation in §5 must remain *correct* at scale 0 — animations communicate, so each token row above has an instant-cut equivalent (crossfade → swap; cascade → appear; progress → set).

### §6.2 Load shedding

The frame HUD's rolling p95 is visible to `MotionTokens`. When p95 > 8.3 ms for 30 consecutive frames: cascade staggers drop to 0, shimmer framerate halves (easing quantized to 30 Hz), breathing dots freeze. When load recovers for 120 frames, full fidelity returns. Users perceive "always smooth", never "sometimes fancy".

---

# Part III — The async & streaming kernel

## §7 The bridge

### §7.1 Client runtime handle

The `client` crate exposes exactly one constructor to the GUI:

```rust
// client crate (Wave 4; M2 ships a stub with the same signature over today's HTTP)
pub struct ClientHandle { /* opaque: command sender + runtime keepalive */ }

impl ClientHandle {
    /// Spawns the Tokio runtime on dedicated threads. Called ONCE in main(), before app.run.
    pub fn start(config: ClientConfig) -> ClientHandle;
    /// All capabilities below return immediately; results stream via flume receivers.
    pub fn search(&self, q: SearchQuery, generation: Gen) -> (StreamHandle, flume::Receiver<SearchEvent>);
    pub fn open_symbol(&self, key: SymbolKey, generation: Gen) -> (StreamHandle, flume::Receiver<DocEvent>);
    pub fn open_package(&self, id: PackageId, generation: Gen) -> (StreamHandle, flume::Receiver<PackageEvent>);
    pub fn resolve_project(&self, root: PathBuf) -> (StreamHandle, flume::Receiver<ProjectEvent>);
    pub fn sync(&self) -> flume::Receiver<SyncEvent>;          // long-lived, app lifetime
    pub fn jobs(&self) -> flume::Receiver<JobEvent>;           // long-lived, app lifetime
    pub fn command(&self, cmd: ClientCommand);                 // fire-and-forget intents (cancel job, pin, retry)
}
```

Requirements the stub and the real Wave-4 implementation both satisfy:

- Never blocks the caller; `command` uses `try_send` on a bounded(64) channel and surfaces overflow as a `JobEvent::CommandRejected` (has never legitimately triggered = capacity review).
- All receivers are `flume` bounded (Appendix C capacities) and close when the source completes — closure IS the completion signal for finite streams.
- Every event enum carries `generation: Gen` where it answers a slotted query.

### §7.2 `Gen`

```rust
// workspace/gui/src/bridge/gen.rs
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Gen(pub u64);
pub struct GenSource(u64);
impl GenSource {
    pub fn new() -> Self { Self(0) }
    pub fn next(&mut self) -> Gen { self.0 += 1; Gen(self.0) }
}
```

One `GenSource` per query slot, owned by the store. No global counter (slots are independent), no timestamps (tests stay deterministic).

### §7.3 The drain loop (the only way events enter GPUI)

```rust
// workspace/gui/src/bridge/drain.rs
use gpui::{AppContext as _, Context, Entity, Task};

/// Spawn a foreground task that drains `rx` into `store`, batching bursts into
/// a single update+notify per wake (§2.2 rule 3). Returns the owning Task (LD-18).
pub fn drain<S: 'static, T: Send + 'static>(
    cx: &mut Context<S>,
    rx: flume::Receiver<T>,
    mut apply: impl FnMut(&mut S, T, &mut Context<S>) + 'static,
) -> Task<()> {
    cx.spawn(async move |store, cx| {
        loop {
            // Await one event (yields to the executor; costs nothing while idle).
            let Ok(first) = rx.recv_async().await else { break };  // channel closed → stream done
            let batch_result = store.update(cx, |store, cx| {
                apply(store, first, cx);
                // Opportunistically drain everything already queued — burst = one render.
                while let Ok(more) = rx.try_recv() { apply(store, more, cx); }
                cx.notify();
            });
            if batch_result.is_err() { break }                    // store dropped → stop
        }
    })
}
```

This ~25-line function replaces every ad-hoc `cx.spawn(Compat::new(...)).detach()` in the current code. Stores call it once per stream and keep the `Task`.

### §7.4 Store method shape (the template every data operation follows)

```rust
impl SearchStore {
    pub fn query(&mut self, text: SharedString, cx: &mut Context<Self>) {
        self.slot.generation = self.gens.next();                    // supersede
        self.slot.begin_loading();                           // keeps old value (LD-15)
        let (handle, rx) = self.client.search(self.build_query(&text), self.slot.generation);
        self.slot.handle = Some(handle);                     // drops+cancels predecessor (§2.3)
        self.drain_task = drain(cx, rx, |store, event, cx| {
            if event.generation() != store.slot.generation { return }       // stale guard
            store.apply_search_event(event, cx);              // pure state mutation
        });
        cx.notify();
    }
}
```

Four lines of policy (gen, slot, handle, drain) — memorize the shape once, apply it everywhere. There is no other pattern in the codebase.

## §8 Streaming primitives

### §8.1 `Phase` and `StreamSlot`

```rust
// workspace/gui/src/bridge/slot.rs
use std::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Phase {
    Idle,
    Loading  { since: Instant },                  // request out, nothing yet
    Streaming { first_at: Instant },              // partial content arriving
    Ready    { at: Instant },                     // stream completed
    Failed   { at: Instant },                     // terminal error (value may still be stale-visible)
}

pub struct StreamSlot<T> {
    pub phase: Phase,
    pub value: Option<T>,          // survives reloads: stale-while-revalidate (LD-15)
    pub error: Option<SlotError>,
    pub generation: Gen,
    pub handle: Option<StreamHandle>,
}

impl<T> StreamSlot<T> {
    pub fn begin_loading(&mut self) { self.phase = Phase::Loading { since: Instant::now() }; self.error = None; }
    pub fn first_event(&mut self)   { if matches!(self.phase, Phase::Loading{..}) { self.phase = Phase::Streaming { first_at: Instant::now() } } }
    pub fn complete(&mut self)      { self.phase = Phase::Ready { at: Instant::now() }; self.handle = None; }
    pub fn fail(&mut self, e: SlotError) { self.phase = Phase::Failed { at: Instant::now() }; self.error = Some(e); self.handle = None; }
    /// What should the view show? One function, one truth table.
    pub fn display(&self) -> Display<'_, T> {
        match (&self.value, self.phase) {
            (None, Phase::Idle)            => Display::Empty,
            (None, Phase::Loading{since})  => Display::Skeleton { show_after: since },   // 120ms grace before skeleton
            (None, Phase::Failed{..})      => Display::Error(self.error.as_ref().unwrap()),
            (Some(v), Phase::Loading{..})  => Display::Stale(v),                          // dim 70% + shimmer strip
            (Some(v), Phase::Streaming{..})=> Display::Partial(v),                        // content + arriving affordances
            (Some(v), Phase::Ready{..})    => Display::Fresh(v),
            (Some(v), Phase::Failed{..})   => Display::StaleWithError(v, self.error.as_ref().unwrap()),
            (None, Phase::Streaming{..})   => Display::Skeleton { show_after: Instant::now() }, // first event pending apply
        }
    }
}
```

`Display` is the contract between data and design: §5.2's tokens map 1:1 onto its variants (`Skeleton` → `skeleton.shimmer`, `Stale` → dim, `Fresh` after `Stale` → `content.crossfade`, …). A view renders a slot by matching `display()` — it *cannot* forget a state, because the compiler enumerates them.

**Grace periods (anti-flicker):** skeletons appear only if `Loading` persists 120 ms (locally-served content usually beats this — no skeleton flash); spinners only after 300 ms (§5.1). Both graces are checked in render against `since` with a scheduled re-notify at the boundary (a 1-shot `cx.spawn` timer owned by the slot's store).

### §8.2 `Progressive<T>` — append-only streamed documents

For streams that build a document (symbol page, refs list, timeline), the slot's value is a `Progressive<T>`:

```rust
pub struct Progressive<T> {
    pub parts: Vec<T>,                 // append-only during a gen; SharedStrings inside
    pub arrived: Vec<Instant>,         // drives §4.1 entrance windows per part
    pub complete: bool,
}
```

Append-only is a *protocol invariant* (§9.3): the GUI never reflows earlier parts because a later part arrived, which is what makes streamed reading calm instead of jumpy.

## §9 Streamed documentation

### §9.1 Goals

- **First paint < 50 ms** after open for locally-Ready packages (header from the SQLite catalog row / archive name index — no full doc parse on the critical path).
- **Reading begins immediately:** doc prose arrives section-by-section, top-down; code blocks appear as monochrome text and *upgrade* to highlighted in place.
- **Ancillary tabs fill in the background:** Refs/Impls/Timeline populate while the user reads Docs; their tab labels tick counts up (`count.tick`) as pages land.
- **Partial is valid:** killing the stream at any point leaves a correct, readable page plus an unobtrusive "…" affordance.

### §9.2 The RenderModel (`render.v1`)

Typed nodes, DocC-style (15-gui-references §1.6), replacing `SymbolEntry.kind: Value` (LD-7):

```rust
// shared wire crate (compiler-wire or client-core), versioned:
pub struct SymbolHead {                       // ALWAYS the first event
    pub key: SymbolKey,                       // stable id + generation stamp
    pub breadcrumb: Vec<CrumbRef>,            // each clickable (package/module refs)
    pub signature: Vec<SigToken>,             // typed tokens, see below
    pub kind: SymbolKindV1,                   // closed enum + Unknown(String) fallback
    pub visibility: VisibilityV1,
    pub provenance: Provenance,               // TrustedLocal | SyncedLocal { generation } | Remote { generation } — LD-8 badge
    pub deprecation: Option<SharedStr>,
    pub section_plan: Vec<SectionPlan>,       // what will stream + size hints → skeleton geometry (§9.4)
}
pub enum SigToken { Kw(&'static str), Ident(SharedStr), Ty { text: SharedStr, target: Option<SymbolKey> },
                    Punct(&'static str), Ws, Generic(SharedStr) }   // Ty is clickable → rustdoc-style linked signatures
pub struct SectionPlan { pub id: SectionId, pub kind: SectionKind, pub size_hint: SizeHint }
pub enum SizeHint { Lines(u32), Rows(u32), Unknown }

pub enum RenderSection {                      // one per Section event
    Prose      { id: SectionId, blocks: Vec<ProseBlock> },          // parsed markdown, typed
    CodeBlock  { id: SectionId, lang: LangId, text: SharedStr, line_count: u32 },
    Members    { id: SectionId, entries: Vec<MemberRow> },
    Fields     { id: SectionId, entries: Vec<FieldRow> },
    Examples   { id: SectionId, blocks: Vec<ProseBlock> },
    Callout    { id: SectionId, level: CalloutLevel, blocks: Vec<ProseBlock> },
    Unknown    { id: SectionId, kind_tag: SharedStr },              // forward-compat chip (LD-7)
}
pub enum ProseBlock { Paragraph(Vec<InlineRun>), Heading { level: u8, runs: Vec<InlineRun> },
                      List { ordered: bool, items: Vec<Vec<InlineRun>> }, Rule,
                      Code { lang: LangId, text: SharedStr, line_count: u32 } }
pub enum InlineRun { Text(SharedStr), Code(SharedStr), Strong(SharedStr), Em(SharedStr),
                     Link { text: SharedStr, target: LinkTarget } } // LinkTarget::Symbol(SymbolKey) | Url(SharedStr)
```

### §9.3 The `DocEvent` stream protocol

```rust
pub enum DocEvent {
    Head(SymbolHead),                                          // exactly once, first
    Section(RenderSection),                                    // in section_plan order, append-only
    Highlight { section: SectionId, spans: Arc<[HighlightSpan]> },  // async upgrade, any time after its Section
    Refs      { page: RefsPage, done: bool },                  // paged; interleaves freely
    Impls     { page: ImplsPage, done: bool },
    Timeline  { events: Arc<[LineageEvent]>, done: bool },
    Done,                                                      // terminal; receiver then closes
    Failed(DocError),                                          // terminal
}
```

Protocol invariants (tested in §28.2 with a property test over event permutations):

1. `Head` precedes everything; `Done`/`Failed` terminate; `Section`s respect `section_plan` order; `Highlight` only references already-sent sections. A violating stream is a client bug — the GUI logs and renders what it can (never panics).
2. Everything is *incremental-safe*: applying a prefix of a valid stream yields a valid page.
3. All payloads are `Arc`/`SharedString` — the drain loop moves pointers.

**Where events come from (the transport seam, §0.2):**

| Era | Producer |
|---|---|
| M5 (now) | Client-side chunker: fetch today's full `SymbolEntry` JSON on the client runtime, then *emit the same events* — parse markdown to `ProseBlock`s (background executor, `pulldown-cmark`), split by heading into `Section`s, run tree-sitter highlight per block and emit `Highlight` as each completes. The GUI cannot tell this from real streaming — and gets progressive highlight for free. |
| Wave 4/5 | REGISTRY: sections read straight from the PackageArchive doc parts (mmap) — near-instant, still streamed for uniformity. INDEX: HTTP response streaming per section. Identical events. |

### §9.4 Progressive rendering rules (zero-jump guarantee)

1. **Skeleton from `section_plan`:** the page pre-lays every planned section as a skeleton sized by `SizeHint` (`Lines(n)` → `n × line_height`; `Rows(n)` → `n × row_height`; `Unknown` → 3-line block). Arrival replaces skeleton with content of the *same height class* → the scroll position never teleports while reading.
2. **Code reserves geometry:** `CodeBlock.line_count` fixes the block height before text arrives fully styled; `Highlight` recolors runs (`highlight.sweep`, §5.2) with *no* geometry change — mono font metrics are constant across colors.
3. **Sections mount with `section.arrive`** (§5.2), keyed `("doc.section", generation.0, section_id)` — first arrival animates, tab-switch re-renders don't (arrival windows, §4.1).
4. **The body is a `list()`** (variable-height virtualized) whose items are sections — a 4 000-line mega-doc costs only its viewport. `ListState` measurement uses the same `SizeHint` before real measure.
5. **Below-the-fold priority:** the M5 chunker emits sections in plan order but the *highlighter* prioritizes viewport-visible sections first (the GUI sends visible `SectionId`s over `ClientCommand::HighlightPriority` on scroll — best-effort, coalesced at 4 Hz).

---

# Part IV — The design system

## §10 Tokens

All tokens live in `theme/tokens.rs` as a `NudoxThemeExt` GPUI global, layered over gpui-component `Theme` (LD-14). Views consume `cx.theme()` (gpui-component) + `cx.theme_ext()` (ours). No inline values.

### §10.1 Space & radius

4 px base grid: `space_1..space_8 = 4, 8, 12, 16, 20, 24, 32, 40`. Radii: `r_sm 4` (chips, badges), `r_md 6` (buttons, inputs, rows), `r_lg 10` (cards, panels, toasts), `r_xl 14` (overlays). Hairline `1 px` borders everywhere; never 2 px except focus rings.

### §10.2 Type scale

| Token | Size/Line | Use |
|---|---|---|
| `text.display` | 20/28 semibold | symbol page title |
| `text.title` | 15/22 semibold | panel headers, modal titles |
| `text.ui` | 13/20 regular | default chrome |
| `text.dense` | 12/16 regular | table rows, logs, refs |
| `text.caption` | 11/16 medium, +0.2 tracking, muted | overlines, section labels, shortcuts |
| `text.mono` | 12.5/19 | code, signatures, paths, log payloads |

Font stack: system UI face; mono defaults to the bundled asset font (gpui-component-assets) with a settings override. Line heights are locked into the size-hint math of §9.4 — changing them is a design-system PR, not a view PR.

### §10.3 Color roles

Roles, not palettes (both themes fill them): `bg.base`, `bg.raised` (+1 elevation surface), `bg.overlay`, `bg.hover`, `bg.active`, `fg.default`, `fg.muted`, `fg.faint`, `accent` (interactive), `accent.fg-on`, `border.default`, `border.strong`, `ring`. Semantic: `ok`, `warn`, `danger`, `info`. Kind colors for symbol kinds mirror rustdoc's family (fn/struct/trait/enum/mod/const distinct hues at matched luminance so badges scan preattentively).

### §10.4 Trust chrome (LD-8)

| Provenance | Color token | Badge | Extra |
|---|---|---|---|
| TrustedLocal | `trust.local` (green family) | `⬢ local` | none |
| SyncedLocal | `trust.synced` (blue family) | `⬢ synced` | generation stamp in tooltip |
| Remote | `trust.remote` (amber family) | `⬡ remote` | `pattern_slash` hatch on progress surfaces while syncing |
| Stale/Offline-served | `trust.stale` (grey) | `⬡ stale` | banner if whole view is stale |

### §10.5 Elevation

Three shadow tokens (GPUI box shadows): `elev.raised` (y1 b3 α.10), `elev.overlay` (y4 b16 α.18 + hairline border), `elev.toast` (y6 b24 α.22). Dark theme swaps shadow strength for border strength (shadows read poorly on dark).

### §10.6 Iconography

Lucide set via gpui-component `IconName` where it exists; custom SVGs (trust hexagons, kind glyphs, lineage markers) in `assets/icons/` at 16 px grid, `currentColor` only, so `svg().text_color(...)` themes them.

## §11 Component recipes

Reusable mid-level components (in `ui/` module), each a thin composition of gpui-component + tokens + motion — built once in M3–M4 and reused by every screen:

| Component | Composition | Used by |
|---|---|---|
| `SlotView<T>` | matches `StreamSlot::display()` → skeleton/stale/fresh/error frames with §5.2 motion baked in | every data surface |
| `Badge` | chip + kind/trust color + `badge.pop` | hits, tabs, headers, deps |
| `CountLabel` | `Motion`-ticked number + caption styling | tab counts, result totals, job counts |
| `SectionHeader` | caption overline + optional count + optional action | all panels |
| `KeyHint` | `Kbd` chips row | palette, overlays, empty states, `?` |
| `EmptyState` | icon + title + line + primary action + `empty.state` motion | every list/panel |
| `ErrorState` | `danger` accent + message + Retry + details disclosure | every slot |
| `ProgressRow` | label + `progress.fill` bar + count + cancel | jobs, sync, deps |
| `SignatureLine` | `SigToken` renderer: mono runs, `Ty` tokens are links with `hover.tint`+underline | symbol page, search rows, quick-peek |
| `ProvenanceDot` | 8 px dot + `status.breathe` (permit-gated) | status bar, package rows |
| `Toolbar` | h_flex + segmented filters (chips) + search input slot | search, logs, refs, package browser |

---

# Part V — Stores

## §12 Store catalog

Every store: GPUI `Entity`, constructed in `main.rs` order (settings → client handle → stores → workspace), holding **precomputed render-ready state** (sorted, filtered, `SharedString`-ified at update time — §1.1.4). Events are the only inter-store/view coupling. Full field/event specs:

### §12.1 `SettingsStore`

Replaces `NudoxSettings` (audit §4). XDG/`dirs`-based paths (fixes B8). Fields: `appearance { theme_id, motion_scale, font_size, mono_font }`, `connection { index_url, timeout, auth_token_ref }` (token in OS keychain, never in JSON — LIBRARIFICATION GD-26 "no store credentials on desktops"), `storage { registry_root, quota_gb }`, `search { debounce_ms, semantic_enabled, type_search_enabled }`, `languages { per-lang local|remote|off }` (18-desktop-toolchains §4.6), `keymap_overrides`. Persisted debounced-500 ms on change via background executor (atomic temp-file rename); file-watch hot-reload retained. Emits `SettingsChanged(SettingsDelta)`.

### §12.2 `ProjectStore`

State: `projects: Vec<ProjectRow>` (recent + open), `active: Option<ProjectId>`, per-project `resolve: StreamSlot<ResolvedProject>` where `ResolvedProject { jail_roots, trusted_members: Vec<MemberRow>, dep_set: Vec<DepRow>, diagnostics: Vec<TrustDiag> }` (frozen diagnostic codes from LIBRARIFICATION §14.2 rendered as designed rows, not toasts). Methods: `open(path)` → `client.resolve_project` stream; `promote_trust(path)` (confirm modal first); `set_active`. Events: `ProjectResolved`, `ActiveChanged`, `TrustDiagnostics`. Dep rows carry live `GenerationStatus` mirrored from `RegistryStore` by id (single source: RegistryStore owns status; ProjectStore stores ids only — no split brain, fixing the audit's `loaded_chunks` disease §2.4).

### §12.3 `SearchStore`

State: `input: SharedString`, `scope: SearchScope { dep_set_filter, kinds, langs, packages }`, `mode: Auto|Name|Type|Semantic`, `slot: StreamSlot<SearchResults>`, `gens: GenSource`, `selection: Option<usize>`, `gen_arrival: Instant`. `SearchResults { sections: [SectionBuf; 3] }` — Name/Type/Semantic sections, each `Vec<HitRow>` where `HitRow` is fully render-ready (`display_name`, `sig_preview: Vec<SigToken>`, `badge: Provenance`, `score`, `key`). Debounce: 24 ms foreground timer task (restart per keystroke); `SearchEvent::{Section, Merge, Latency, Done, Failed}` events append/replace per section — **local sections never wait for semantic** (15-gui-references §2.3 latency budget: local rows < 10 ms, semantic appends 20–200 ms later without clearing anything). Methods: `set_input`, `set_mode`, `toggle_kind`, `move_selection` (drives `scroll.glide`), `commit_selection` → emits `OpenSymbol(key, disposition)`.

### §12.4 `SymbolStore`

State: `docs: HashMap<TabId, SymbolDoc>` where `SymbolDoc { head: Option<SymbolHead>, sections: Progressive<RenderSection>, highlights: HashMap<SectionId, Arc<[HighlightSpan]>>, refs: Progressive<RefsPage>, impls: Progressive<ImplsPage>, timeline: Progressive<LineageEvent>, slot_meta: StreamSlot<()>, gens: GenSource }`. Methods: `open(key) -> TabId` (dedup: existing tab re-activates + `nav.flash`), `close(tab)` (drops doc + handle → cancels stream), `reload(tab)`, `set_version(tab, gen)` (new gen stream into same tab, stale-while-revalidate), `visible_sections(tab, ids)` (→ `HighlightPriority`). Events: `HeadReady(tab)`, `SectionArrived(tab, id)`, `TabCountsChanged(tab)`.

### §12.5 `RegistryStore`

Mirror of SyncEngine state: `generations: HashMap<GenerationId, GenRow { status: Pending|Planning|Fetching{done,total,bytes}|Verifying|Ready|Failed, provenance, bytes_by_path }` fed by the long-lived `sync()` receiver (coalesced progress — keep-latest per generation, Appendix C). Emits `GenerationChanged(id)` — consumed by ProjectStore dep rows, status bar, Jobs panel, and `Routed`-style provenance badges. Method: `retry(id)`, `pin(id, level)`.

### §12.6 `JobStore`

State: `active: Vec<JobRow>`, `recent: VecDeque<JobRow>` (cap 100), `pipeline: HashMap<ProjectId, PipelineState>` (parse→IR→symbols→tantivy→vectors→lineage stage dots, LIBRARIFICATION §6 stages). `JobRow { id, kind: Compile|Sync|Embed|Index, label, progress: Option<f32>, phase: SharedString, started, log_filter: LogFilterId }`. Fed by `jobs()` receiver. Methods: `cancel(id)` → `ClientCommand::CancelJob`; completion events also emit `ToastRequest` (consumed by shell §13.7). Replaces LocalIndexPanel's subprocess UX entirely (audit §7.2): local compile is a job like any other, run through `TrustedForgeContext` client-side.

### §12.7 `LogStore` (upgraded, kept)

Keeps the ring buffer + tracing layer (audit §3.6 — it's good). Changes: **seq-based reads** — `entries_since(Seq) -> (Seq, &[LogEntry])` over a `VecDeque` with a monotonically increasing base seq (fixes B11's full-clone-per-frame); `job_id` field threading for per-job drill-down; notify path becomes a standard `drain` on a bounded(1024, drop-oldest) channel.

### §12.8 `NavHistory`

`back: Vec<NavEntry>`, `forward: Vec<NavEntry>`, `current: Option<NavEntry>`; `NavEntry { kind: Symbol(SymbolKey)|Package(PackageId)|Screen(ScreenId), scroll_fraction: f32 }`. Push on every open/breadcrumb/graph navigation; back/forward re-activate existing tabs when present (else re-open); scroll restored via list handles. Buttons + `cmd-[`/`cmd-]`.

### §12.9 `ShellStore`

Pure UI state with persistence: dock sizes/visibility (`Motion` targets live in views; *committed* values live here and persist via `DockArea` state — gpui-component `DockAreaState` serde), active tab per pane, overlay stack (omni/palette/modal — one active overlay max, stack for nesting modals over search), banner queue, toast queue (cap 3 visible + overflow count chip).

## §12.10 Event flow map (who listens to whom)

```
SettingsStore ──SettingsChanged──────────▶ everything (theme, motion, keymap swaps)
ProjectStore ──ActiveChanged────────────▶ SearchStore (scope), ShellStore (title), JobStore (pipeline)
SearchStore ──OpenSymbol────────────────▶ SymbolStore.open → ShellStore (tab activate) + NavHistory.push
SymbolStore ──HeadReady/SectionArrived──▶ (views only)
RegistryStore ─GenerationChanged────────▶ ProjectStore dep rows · status bar · Jobs panel
JobStore ──ToastRequest─────────────────▶ ShellStore toasts
LogStore ──(seq polling via drain)──────▶ LogPanel only
NavHistory ──Navigated──────────────────▶ ShellStore (activate tab/screen)
```

Acyclic by construction; a cycle is a review-blocking error.

---

# Part VI — Screens (all scoped)

Every screen is specified as: purpose · layout · states · data · motion (tokens from §5) · keyboard · perf notes · acceptance. States are always the `Display` variants of the slots involved (§8.1) — no screen invents new states.

## §13 The shell

### §13.1 Chrome layout

```
┌ TitleBar  [☰] Nudox — my-app ⬢local     [⌘K Search…]      [Jobs ●2] [◐] ┐
├─ left dock ──┬── center: panes of tabs ─────────────┬── right dock ─────┤
│ Project      │ ◀ ▶  [Router ⬢][map_err ⬡][Pkg: axum]│  Outline          │
│ Search       │                                      │  Quick peek       │
│ Packages     │        active item content           │  (P1)             │
├──────────────┴──────────────────────────────────────┴───────────────────┤
│ bottom dock:  [Jobs] [Logs]                                             │
├──────────────────────────────────────────────────────────────────────────┤
│ status bar: ● index synced · 3 gens fetching (2.1 MB/s) · ⚠1 · 143 ms   │
└──────────────────────────────────────────────────────────────────────────┘
```

Built on gpui-component `DockArea` (`dock/mod.rs:44`) with `Panel`-implementing wrappers (`dock/panel.rs:54`) for each dock surface; sizes persist via `DockAreaState` in `ShellStore`. The monolithic flex in `workspace.rs` is deleted. Drag-resize is 1:1 during drag; on release, `dock.slide` spring settles to the snapped size. Toggles animate the same spring (LD-5's sanctioned layout animation).

### §13.2 States

Shell is always Ready; degraded modes are *banners*: offline (`trust.stale` background, "Serving local Ready subset — n packages unavailable"), INDEX unreachable (retry countdown with ticking seconds), update available (P3). One banner max; `banner.drop` in/out; queue in `ShellStore`.

### §13.3 Status bar

Left→right: backend `ProvenanceDot` + label · sync summary (`CountLabel` gens + `Motion`-smoothed MB/s from `bytes_by_path`) · diagnostics count chip (click → Logs filtered to warnings) · frame-time readout (debug builds — the HUD trigger §25.2). Every segment is a button; every segment has a tooltip with detail.

### §13.4 Panes, tabs, items

`WorkspaceItem` trait (imitating Zed's Item pattern, not its code — 15-gui-references §3.4): `tab_content(cx) -> AnyElement` (icon + title + provenance badge + optional `CountLabel`), `telemetry_id`, `nav_entry()`. Implementors: SymbolPage, PackageBrowser, GraphView, DiffView, SettingsPage. One pane in v1 (splits P2 — the trait is split-ready: items are `AnyView`s, panes own slots). Tab strip: active underline slides (`tab.switch`), reorder by drag (`tab.reorder`), close buttons appear on hover (`hover.tint` reveal), middle-click closes, overflow → chevron menu. `cmd-1..9` activates by index.

### §13.5 Overlay system

`ShellStore.overlays` renders via `deferred(anchored(...))` above everything: omni-search (§15), command palette (§23.1), modals (trust confirm, GPL pack consent — 18-desktop-toolchains §12.4), quick peek (§23.2), `?` shortcuts (§23.3). Scrim: window `Blurred` appearance when platform supports (macOS), else `bg.overlay` at 32 % alpha. `overlay.in`/`overlay.out`; Escape pops the stack; focus is trapped (gpui-component `focus_trap`) and restored on close.

### §13.6 Global keymap

Declared once in `app/keymaps.rs` (full table Appendix B). Core: `cmd-K` omni · `cmd-shift-P` palette · `cmd-[`/`]` history · `cmd-B` left dock · `cmd-J` bottom dock · `cmd-W`/`cmd-1..9` tabs · `cmd-,` settings · `?` shortcuts overlay (when no input focused).

### §13.7 Toasts

Bottom-right stack, cap 3 + "+n" chip; `toast.in/out`; hover pauses the dismiss timer (the hairline countdown pauses too — the detail users feel). Toast = completion of background work only (LD-16): job done/failed, generation Ready, pack installed. Every toast has one action max ("View", "Retry").

**Acceptance:** shell renders < 8 ms steady; dock toggle animates ≥ 110 Hz on ProMotion with symbol page open; tab close leaks zero entities (§28.3); every §13.6 binding works with docks in any visibility combination.

## §14 Project manager (first-run + project panel)

**Purpose.** Open/assign projects, visualize the trust boundary, watch deps sync — LIBRARIFICATION §14's jail/trust model made visible (P0 acceptance demo surface).

**Layout.** First-run: centered card (max-w 560) — wordmark, "Open a project…" primary button + recent list + drop-target (drag a folder anywhere on the window). After open, the left-dock Project panel:

```
┌ PROJECT ──────────────────────────────┐
│ my-app  ⬢ local · 3 members           │
│ ~/Projects/my-app · Cargo workspace   │
├ TRUSTED (compiled locally) ───────────┤
│ ⬢ my-app        ✓ indexed @ abc123f   │
│ ⬢ my-app-core   ⟳ compiling 64% ▓▓▓░  │
│ ⬢ my-app-api    ✓ indexed            │
├ DELEGATED (INDEX → local) ── 247 ─────┤
│ [All ▾] [Syncing] [Ready] [Error]     │
│ ⬢ serde 1.0.210     ✓ ready          │
│ ⬡ tokio 1.40.0      ▓▓▓░ 3.1/8.0 MB  │
│ ⬡ axum 0.7.5        queued           │
│ … uniform_list …                      │
├ ⚠ 2 trust diagnostics ────────────────┤
│ trust::symlink_escape  crates/vendored │
└ [Sync all]              [Trust rules] │
```

**States.** Resolve slot: Skeleton (member/dep rows shimmer with realistic widths) → Streaming (members land first, deps stream in with `row.cascade`) → Ready; Failed → `ErrorState` with the failing diagnostic. Dep rows individually live: `progress.fill` bars, `badge.pop` on Ready flips, `pattern_slash` hatch while remote-served (§10.4).

**Data.** `ProjectStore.resolve` + `RegistryStore.generations` (per-row status), `JobStore.pipeline` (member compile stages). Right-click dep → context menu: pin version, re-sync, open package, copy coordinate. "Promote to trusted" always behind a modal naming the exact path and consequence (compile executes this code locally).

**Motion.** `row.cascade` on dep list arrival; `count.tick` on the 247; `progress.fill`; member stage dots use `status.breathe` while active (permit-gated).

**Keyboard.** `↑↓` rows, `Enter` open package, `space` expand row detail, `s` sync-selected.

**Perf.** Deps = `uniform_list`, 10 k rows fine; progress coalescing means a 50-dep parallel sync notifies ≤ 30 Hz total (Appendix C).

**Acceptance.** Cold open of a 250-dep workspace: members visible < 300 ms, full dep list streaming < 1 s, zero frame > 8.3 ms during sync storm; the trust modal is the only way anything becomes trusted.

## §15 Omni-search (`cmd-K`)

**Purpose.** The homepage (15-gui-references §6.1). Name + type + semantic fused, local-first, < 10 ms to first rows.

**Layout.** Centered overlay 640 w, max-h 60 %: input row (mode chips Auto/Name/Type/Semantic + scope chips) → sectioned results (`uniform_list` per section under sticky caption headers with per-section latency readouts) → footer `KeyHint` row.

**States.** Input empty → recent symbols + recent searches (from NavHistory) with `EmptyState` beneath if none. Typing → §7.4 flow: name/type sections land together (< 10 ms budget: tantivy/SQLite on client runtime, but the *paint* is gated only by the drain — no skeleton, rows just cascade); semantic section shows a 3-row shimmer *only in its own section* until it lands (never blocks the others, LD-15 never clears them). Zero hits → designed empty with "search remote INDEX instead" action when scope was local. Offline → semantic section replaced by a one-line `trust.stale` notice.

**Data.** `SearchStore` (§12.3). Selection movement never wraps silently across sections — it announces section change by scrolling the sticky header into place (`scroll.glide`).

**Motion.** `overlay.in/out`; `row.cascade` per gen (§4.1 window rule — scrolling never re-animates); `count.tick` per-section counts; selection bar is a `Motion` SNAPPY y-offset (it glides between rows rather than teleporting — the single most felt polish detail in the app).

**Keyboard.** `↑↓` move · `tab` next section · `enter` open · `cmd-enter` open without closing overlay · `alt-enter` open in background tab · `esc` clear-then-close · `cmd-1/2/3` jump to section.

**Perf.** Fuzzy scoring for the recent-items mode runs on background executor over ≤ 512 entries; result rows are precomputed `HitRow`s (§12.3) — render does zero string work.

**Acceptance.** Keystroke→first-rows p95 < 10 ms local (harness §25.3); 10 k-hit result set scrolls ≥ 110 Hz; semantic arriving never moves the selection or existing rows.

## §16 Symbol page (the streamed centerpiece)

**Purpose.** §9 made visible. Docs/Source/Refs/Impls/Timeline/Graph tabs inside a `WorkspaceItem`.

**Layout.**

```
┌ axum › response › Result — breadcrumb (each crumb clickable)     ⬢ synced ┐
│ pub fn map_err<F, O>(self, op: O) -> Result<T, F>        [fn] [0.7.5 ▾]  │
├ [Docs] [Source] [Refs 128⟳] [Impls 4] [Timeline] [Graph] ────────────────┤
│                                                                          │
│  (Docs) virtualized section list — §9.4                                  │
│   Prose… CodeBlock(mono→highlight.sweep)… Members… Examples…             │
└──────────────────────────────────────────────────────────────────────────┘
```

**States.** The whole page is `SlotView` over `SymbolDoc`: Head lands → header paints (< 50 ms local, < 250 ms remote — §9.1) while the body shows the `section_plan` skeleton; sections stream (`section.arrive`); tab labels tick counts as `Refs`/`Impls`/`Timeline` pages land (`⟳` glyph while their stream is live). `Failed` after content = `StaleWithError` — page stays readable, a slim retry bar appears (LD-16). Version switch (`set_version`) = stale-while-revalidate: old doc dims to 70 %, new streams over it, `content.crossfade` per section as replaced.

**Tabs.**
- **Docs** — §9.4 rules verbatim. `SignatureLine` types are links (`page.handoff` on click, NavHistory push).
- **Source** — gpui-component editor (read-only) fed by CAS bytes via `PackageEvent::SourceChunk` (VirtualOnly materialization — LIBRARIFICATION §3.5); tree-sitter from the highlighter registry; sticky context header (GitHub pattern); `L` deep-links; loads only on first activation (lazy tabs).
- **Refs / Impls** — virtualized tables (`TableDelegate`), columns path/line/kind/precision-badge (Sourcegraph precise-vs-textual pattern); grouped-by-file collapse (`chevron.twirl`); pages append with `row.cascade`.
- **Timeline** — §22.1.
- **Graph** — §18 scoped to this symbol's neighborhood.

**Keyboard.** `cmd-shift-]`/`[` inner tabs · `y` copy stable symbol URI · `v` version picker · `g d/s/r/t` go-to-tab.

**Perf.** Body `list()` + reserved geometry = no jumps (§9.4 guarantee is a test §28.2); highlight priority follows viewport (§9.4.5); closed tabs cancel streams (LD-18).

**Acceptance.** Open from search on Ready package: header same-frame as tab creation, first prose < 120 ms, full 200-section doc streamed < 1.5 s, scrolling *during* streaming stays ≥ 110 Hz with zero scroll-position jumps; killing the network mid-stream leaves a readable partial page with retry.

## §17 Package browser

**Purpose.** Module tree + API surface + version bar (rustdoc/docs.rs patterns), diff entry point.

**Layout.** Two-column item: left gpui-component `tree` (module hierarchy, kind glyphs, per-node counts), right virtualized table of the selected module's items ([Exports]/[All]/[Changed since ▾] filter chips) with `SignatureLine` previews. Header: package name, version `Select`, provenance badge, "not latest" chip → jump-to-latest (docs.rs CTA pattern).

**States.** Tree streams by depth (roots first — expandable immediately; `PackageEvent::TreeLevel` events); expanding an unloaded node shows an inline 3-row shimmer. Syncing package = browsable partial: table rows carry per-symbol availability; missing rows show as faint hatched skeleton rows that fill as the generation completes (the *strongest* visualization of progressive sync in the app).

**Motion.** `chevron.twirl`; `row.cascade` on module switch; `content.crossfade` on version switch; filter chip changes animate the table via a 100 ms crossfade, not a reflow-flash.

**Keyboard.** `←→` collapse/expand · `↑↓` tree · `tab` tree↔table · `enter` open symbol · `d` diff against previous.

**Acceptance.** 4 000-item module renders < 5 ms/frame scroll; switching versions preserves tree expansion + scroll for shared module paths.

## §18 Graph view (Ladybug-powered canvas)

**Purpose.** Neighborhood exploration; the audit's dead prototype (B2/B3/B16) replaced by a real custom element.

**§18.1 Architecture.** A single `canvas()` custom element with `with_element_state` retained state: pan (`Motion2`), zoom (`Motion`, log-space), node positions, quadtree hit grid. Force layout (fruchterman-reingold with Barnes-Hut, ~150 LOC) runs on the **background executor**, streaming position iterations over a bounded(2, keep-latest) channel; the canvas springs nodes toward each iteration (`graph.settle`) — physics off-thread, silk on-thread.

**§18.2 Paint (all verified primitives).** Edges: `PathBuilder::stroke` béziers, dash arrays by `EdgeRelation` (member solid, semantic dashed, calls dotted), hovered/active edges run `edge.flow` dash-offset animation; arrowheads = small filled paths. Nodes: rounded quads + kind-colored 3 px left bar + name text; selected = `ring` + `elev.raised` shadow; dimming non-neighborhood on hover via opacity (whole-canvas repaint is one element — cheap).

**§18.3 LOD (the >100-node answer, 15-gui-references §8.4).** zoom < 0.4: nodes = 4 px dots, no labels, straight-line edges; 0.4–0.8: quads + labels for degree-top-K; > 0.8: full detail. Hard cap 600 rendered nodes — beyond, cluster hulls with counts ("+214") that expand on click (`ClientCommand::ExpandCluster` → new sub-layout).

**§18.4 Interaction.** Wheel/pinch zoom to cursor (zoom spring targets keep the anchor fixed — the standard maps math); drag pan 1:1 with spring fling on release (velocity handoff from pointer samples); click node → quick peek (§23.2); double-click → open symbol; `f` fit-to-view (springs both pan and zoom); right-click → expand neighbors / pin / open.

**§18.5 States.** Empty (no query yet) → `EmptyState` with "Explore from a symbol"; layout streaming → nodes fade in as the first iteration lands; Ladybug unavailable (projection rebuilding — GD-34 disposable store) → banner + remote cold-graph fallback via `Routed`.

**Acceptance.** 500 nodes/1 200 edges: pan/zoom ≥ 110 Hz (single canvas element, one repaint); layout thread produces zero foreground stalls; hover hit-test < 0.2 ms via grid.

## §19 Jobs & sync dashboard (bottom dock)

**Purpose.** Every background process observable (15-gui-references §6.8): compiles, syncs, embeds, index stages.

**Layout.** Three stacked groups: **Active** (`ProgressRow`s with phase text, elapsed `count.tick` seconds, Cancel), **Pipeline** (per trusted member: parse→IR→symbols→tantivy→vectors→lineage stage dots — done/active-breathing/pending/failed), **Recent** (virtualized, ✓/✗ + duration + summary + Retry on failures + "logs" jump wired by `job_id` → LogPanel filter).

**States.** All-idle → compact single line "All quiet — last activity 2 m ago" (dock auto-collapses to it if user hasn't pinned). Failure rows carry the error class inline; details expand in-place (never a dialog).

**Motion.** `progress.fill` (indeterminate = scrolling `pattern_slash`); rows *reorder* between groups (active→recent) via `tab.reorder`-style sibling springs — completions visibly migrate down; `toast.in` fires simultaneously via `ToastRequest`.

**Acceptance.** 20 parallel jobs update ≤ 30 notifies/s aggregate (coalescing per Appendix C); cancel reaches the client < 50 ms; a failed compile shows its last 5 log lines inline within the row expansion.

## §20 Log panel (bottom dock, second tab)

Kept from prototype, upgraded: seq-based incremental reads (§12.7 — no more full clones B11), `uniform_list` rows, auto-scroll via `UniformListScrollHandle` (fixes B4) with the standard contract (user scroll-up disengages, jump-to-latest chip with unseen `count.tick` appears), level/category chips retained, `job_id` filter chip (deep-linked from §19), text filter input (background-executor matching over the ring), copy-selection + export. Row entrance: none while auto-scrolling ≥ 10 rows/s (motion budget — flooding logs must not animate), `fade-in` 80 ms when trickling.

**Acceptance.** 5 000 lines/s synthetic flood: panel ≤ 30 notifies/s, frame p95 < 8.3 ms, ring stable at cap, zero allocations per frame in render (rows are cached `SharedString`s).

## §21 Settings screen

A `WorkspaceItem` (not a modal): left section nav, right forms (gpui-component `form`/`Switch`/`Select`/`Input`/`Slider`) bound to `SettingsStore` fields with 500 ms-debounced persist. Sections mirror §12.1: Appearance (theme cards with live mini-previews, motion scale slider whose *drag itself* demonstrates the scale by running a sample `rise_in` on release, font sizes), Connection (INDEX url, health check button with inline result, auth via keychain), Storage (registry root, usage bar per store — cas/tantivy/vectors/ladybug from `RegistryStore` stats, GC button with confirm), Languages (per-language local/remote/off with SDK-discovery status lines: "rustup 1.80 found ✓" / "JDK not found — remote only" — 18-desktop-toolchains §4.6), Search (debounce, semantic/type toggles), Keybindings (searchable table of Appendix B + rebind P2), About (versions, licenses, open-logs-dir). Every control change that has visible effect applies live (`SettingsChanged` fan-out) — the theme crossfades (`content.crossfade` on the window root, the one sanctioned full-window animation, 160 ms).

## §22 Timeline & diff (the lineage wedge)

### §22.1 Timeline tab

Custom canvas strip: horizontal version axis (evenly spaced releases, not time-scaled — API readers think in versions), event markers colored by class (`Added/SignatureChanged/DocsChanged/Moved/Deprecated/Removed` — LIBRARIFICATION §5 edge classes incl. T4/T5 soft edges rendered hollow), spline connectors (`PathBuilder` curves) across gaps, current version highlighted with `ring`. Hover marker → tooltip with event summary; click → select-for-compare (two selections arm the Compare button); `edge.flow` on the path between selected pair. Streams via `DocEvent::Timeline`; empty (single-version package) → one-line "No recorded history yet".

### §22.2 Diff view

`WorkspaceItem` comparing `(SymbolKey, GenA, GenB)`: header with both version chips; **Signature** block = token-level diff of `SigToken` runs (LCS over tokens, background executor; removed = `danger` strikethrough runs, added = `ok` underline runs — inline, not side-by-side, signatures are short); **Docs** = section-aligned prose diff (paragraph LCS, changed paragraphs expand word-level on click); **Members** = added/removed/changed lists with `Badge`s. Loads both docs through the same `DocEvent` streams (two slots), diff computes incrementally as matching sections arrive — the diff itself streams. Entry points: timeline compare, package browser "changed since", `CompareWithPreviousVersion` action.

**Acceptance.** Diff of two 300-section docs: first hunk < 300 ms, complete < 2 s, all off-thread.

## §23 Micro-screens

**§23.1 Command palette (`cmd-shift-P`).** Same overlay skeleton as omni (shared component), sections Actions/Recent symbols/Screens; actions carry their keybindings as `KeyHint`s (discoverability loop); fuzzy over ~100 actions on foreground (trivial n).

**§23.2 Quick peek.** `anchored` popover 440×280 near the invoking element (graph node click, refs row hover-dwell 600 ms, `space` on search selection): header + signature + first prose section only (its *own* gen slot with a lightweight `DocEvent` stream that the client truncates after the first Section — same protocol, tiny payload — JetBrains two-tier docs pattern). `overlay.in` scaled ×0.7; Escape or click-away closes; `Enter` promotes to full tab.

**§23.3 `?` shortcuts overlay.** Grouped cheat-sheet rendered *from the live keymap registry* (never hand-maintained); highlights bindings pressed while open (a delightful teach-mode: press a chord, its row flashes `nav.flash`).

**§23.4 Trust & consent modals.** Trust promotion (exact path, consequence sentence, typed-confirm for non-jail paths); GPL pack consent for `lang-nix` (license text link, source-offer URL — 18-desktop-toolchains §5.3); destructive GC confirm. All share one `Modal` recipe: title, body, danger/primary buttons, `overlay.in`, focus-trapped, Escape = cancel always.

---

# Part VII — Performance engineering

## §24 Standing techniques

1. **Virtualize (LD-6)** — `uniform_list` (fixed rows: search, logs, deps, refs), `list()` (variable: doc sections), gpui-component `Table` (column surfaces), `virtual_list` (grids if needed). Fixed row heights wherever design allows — uniform beats variable measurably.
2. **Precompute at update-time** — stores own sorted/filtered/`SharedString` state (§12 preamble); render is a pure projection. The lint for this is code review of `render` bodies: any `format!`, `.sort`, `.filter`, `to_string` is a finding (HUD-visible regressions back it up).
3. **Cache shaped text** by keeping `SharedString`s stable (§1.1.4); signature runs are pre-tokenized `SigToken`s (shape-cached per token run).
4. **Batch notifies** (drain loop §7.3) and **coalesce progress** (Appendix C policies).
5. **Animate leaves** (§1.1.2, §4.2 contract) — the dock spring test (§13 acceptance) is the canary.
6. **Reserve geometry** (§9.4) — streaming must never cause layout shift; this is also a perf property (no cascading relayouts).
7. **Element-state caches** for custom elements (§18.1 hit grids, timeline layouts) — recomputed only on data-version bump, not per frame.
8. **Payloads are `Arc`** across the bridge (§2.2.5); clone-cost on the foreground is pointer-sized.

## §25 Measurement

### §25.1 Instrumentation

`perf/frame_stats.rs`: a per-window ring of the last 600 frame durations (fed from a `on_next_frame` self-rescheduling sampler active only while the HUD is open or the harness runs), plus counters stores increment (`notifies`, `events_drained`, `rows_rendered`). GPUI's own `profiler/` module output is surfaced in dev builds.

### §25.2 The HUD

Debug overlay (status-bar frame-time click or `F12`): sparkline of frame times (canvas, 600 samples), p50/p95/p99, live entity + subscription counts (§3.4), active-motion count, loop-permit census (§5.4), per-store notify rates. The HUD itself budgets < 0.3 ms (one canvas, no text churn — numbers update at 4 Hz).

### §25.3 The perf harness (CI-gated, LD-11)

`tests/perf/` binaries run in CI on a pinned runner class, driving the real app headless (GPUI test window) through scripted scenarios, asserting budget tables:

| Scenario | Gate |
|---|---|
| `typing_storm` — 40 keystrokes into omni over 10 k-symbol fixture index | first-rows p95 < 10 ms; frame p95 < 8.3 ms |
| `doc_stream` — open 200-section fixture symbol, scroll during stream | header < 50 ms; zero scroll jumps (position assertions); frame p95 < 8.3 ms |
| `sync_storm` — 50 generations progressing at 20 Hz each | aggregate notifies ≤ 35/s; frame p95 < 8.3 ms |
| `log_flood` — 5 000 lines/s for 10 s | §20 acceptance numbers |
| `graph_500` — 500-node layout + scripted pan/zoom | frame p95 < 8.3 ms |
| `tab_churn` — open/close 200 symbol tabs | entity count returns to baseline; RSS delta < 10 MB |

Regressions fail CI with the HUD attribution attached to the report.

### §25.4 Static enforcement

`scripts/lint-gui-no-block.sh` (M0): deny-list grep (`std::fs::`, `reqwest::blocking`, `.detach()` outside `main.rs`, `block_on`, `std::thread::sleep`, `Mutex` in `workspace/gui` outside `log_store` migration shims, inline `Duration::from_` in views — durations come from tokens) + `cargo clippy` with `disallowed_methods`. Wired into CI next to the license check (LD-12).

---

# Part VIII — Implementation program

## §26 Module tree (target)

```
workspace/gui/src/
├── main.rs                    # boot: logging → settings → ClientHandle::start → stores → window
├── app/
│   ├── actions.rs             # all actions! declarations
│   ├── keymaps.rs             # Appendix B, one place
│   └── menus.rs
├── bridge/                    # §7–§8 kernel (no gpui-component deps)
│   ├── gen.rs  ├── drain.rs  ├── slot.rs  ├── handle.rs  └── progressive.rs
├── motion/                    # §4–§6 kernel (no store deps)
│   ├── spring.rs ├── motion2.rs ├── color.rs ├── tokens.rs ├── declarative.rs └── permits.rs
├── theme/
│   ├── tokens.rs  ├── ext.rs  └── themes/{light,dark}.rs
├── stores/                    # §12; one file per store + events.rs
├── client_stub/               # M2–M4 stand-in implementing §7.1 over today's HTTP; deleted at Wave-4 swap
├── ui/                        # §11 recipes
├── workspace/
│   ├── shell.rs  ├── item.rs  ├── pane.rs  ├── status_bar.rs  ├── overlays.rs  └── toasts.rs
├── views/
│   ├── project_panel.rs  ├── omni_search.rs  ├── palette.rs  ├── symbol_page/{mod,docs,source,refs,timeline}.rs
│   ├── package_browser.rs ├── graph/{view,layout,paint}.rs  ├── jobs_panel.rs  ├── log_panel.rs
│   ├── settings_page.rs   ├── diff_view.rs  ├── quick_peek.rs └── shortcuts_overlay.rs
├── render_model/              # §9.2 types + markdown chunker (moves to shared wire crate at Wave 4)
├── perf/                      # §25 frame_stats + hud
└── fixtures/                  # cfg(debug_assertions) + --fixtures (LD-9)
```

## §27 Milestones

Each milestone lists **goal → steps → acceptance**. Steps are ordered and beginner-explicit; "kernel code above" means copy the listed section's code verbatim as the starting point. Every milestone ends green: `cargo build && cargo test && scripts/lint-gui-no-block.sh`.

### M0 — Stabilize & re-scaffold (the honest floor)

*Goal: current app, minus its known lies; the new skeleton in place.*

1. Pin `gpui-component`/`-assets` to `rev = "c112e7b482b6b9c53a8c43deaf5847b94a29ad82"` (LD-10).
2. Fix audit bugs in place: **B1** `run_search` filters `self.results` not `fixture_symbols()` (search_panel.rs:143); **B2** parse `resp.graph` → `AppState.current_graph` (workspace.rs:249); **B3** hover key unify to `entry.id` (graph_view.rs:130/158); **B4** LogPanel `UniformListScrollHandle` + auto-scroll contract (§20).
3. Gate `fixtures.rs` behind `cfg(debug_assertions)` + `--fixtures` arg (LD-9); release empty states are temporary text (real ones arrive with §11 `EmptyState` in M3).
4. Create the §26 directory skeleton (empty `mod.rs`es); move existing files under `views/` unchanged; `main.rs` paths updated. No behavior change.
5. Add `scripts/lint-gui-no-block.sh` with the §25.4 deny-list, initially *warning* on the existing violations (async-compat, detach) — flipped to error in M2 when they're gone. Wire into CI.
6. Add deps: `flume`, `pulldown-cmark` (workspace-pinned versions).

*Acceptance:* app runs exactly as before minus the four bugs; CI runs the lint; fixtures absent from a release build's strings (`strings target/release/lindsey | grep -c axum::Router` = 0).

### M1 — Motion kernel

*Goal: `motion/` complete and proven, before any view uses it.*

1. `spring.rs` — §4.2 code verbatim. `motion2.rs`, `color.rs` per §4.2's closing paragraph.
2. `tokens.rs` — the §5 vocabulary as consts (`pub struct MotionTokens { scale: f32, loop_permits: Cell<u8>, … }`), `scaled(Duration)`, `acquire_loop_slot()` RAII permit (§5.4).
3. `declarative.rs` — `rise_in`, `shimmer`, `delayed`, `fade_in` helpers (§4.1 code), all `MotionTokens`-aware.
4. Unit tests (plain, no GPUI): spring settles from any (value, velocity) within 700 ms sim-time for all three presets; retarget mid-flight preserves velocity continuity (assert no sign-flip discontinuity in position derivative); `tick` is dt-clamped (32 ms stall doesn't teleport); scale-0 `animate_to` == `snap_to`.
5. Demo proof: wire the existing sidebar toggle to a `Motion` width (replacing the 0/320 jump) using the §4.2 render-loop contract — the first spring in the app, and the template for every later one.

*Acceptance:* tests green; sidebar toggle glides; with the sidebar mid-flight, adding a log line does not re-render the sidebar (verify with a temporary render counter — the leaf-animation rule §1.1.2 demonstrably holds).

### M2 — Async kernel + client stub

*Goal: three-runtime topology real; every legacy async path replaced.*

1. `bridge/` — §7.2 `Gen`, §7.3 `drain` (verbatim), §8.1 `slot.rs` (verbatim), §8.2 `progressive.rs`, `handle.rs` (§2.3).
2. `client_stub/` — implement §7.1's `ClientHandle` over the existing endpoints: `ClientHandle::start` builds a 2-thread Tokio runtime + reqwest client *inside the stub*; `search` maps `/run` + `/symbol-search` into `SearchEvent`s; `open_symbol` fetches `/terminus_search` and (for now) emits `Head` + one `Section` from the raw markdown; long-lived `sync/jobs` receivers exist but idle. Bounded channels per Appendix C.
3. Delete `async-compat`, the throwaway Tokio bootstrap (backend.rs:146), the indexer `std::thread` + 200 ms poll, and every `.detach()` outside `main.rs`. `backend.rs` contents migrate into `client_stub/http.rs`.
4. Slot state-machine tests: full `Display` truth table (§8.1), gen-supersession drops stale events, drop-handle cancels (observe stub-side token), drain batches a 100-event burst into one notify (counter assertion).
5. Flip the M0 lint from warn to error.

*Acceptance:* app functions over the stub; lint green with zero exceptions; `tab_churn`-style manual check shows no runaway tasks after closing views.

### M3 — Shell (docks, tabs, overlays, theme, palette)

*Goal: §13 complete; the app *feels* new from here.*

1. `theme/` — tokens §10 + `NudoxThemeExt` global + light/dark; migrate every inline color/px in existing views to tokens (mechanical sweep — acceptable to delegate).
2. `workspace/shell.rs` — `DockArea` with Panel wrappers for left (project+search placeholder), bottom (jobs placeholder + migrated LogPanel), center pane. Persist `DockAreaState` via `ShellStore`. Dock toggles/resize on `dock.slide` springs (M1 template).
3. `workspace/item.rs`+`pane.rs` — `WorkspaceItem` (§13.4), tab strip with sliding underline, drag-reorder, owned subscriptions (§3.2). Port SymbolView to the trait as-is.
4. `workspace/overlays.rs` — overlay stack + scrim + focus trap + `overlay.in/out`; `workspace/toasts.rs` (§13.7); `status_bar.rs` (§13.3 with stub data).
5. `app/keymaps.rs` — Appendix B in full; `views/palette.rs` (§23.1); `views/shortcuts_overlay.rs` (§23.3 reads the registry).
6. `NavHistory` store + back/forward buttons + bindings.
7. `perf/` — frame stats ring + HUD (§25.2) — from this point every subsequent milestone is developed with the HUD open.

*Acceptance:* §13 acceptance list; palette lists every action; `?` renders every binding from the registry; HUD shows steady-state 0 renders when idle.

### M4 — SearchStore + omni-search

*Goal: §15 shipped against the stub.*

1. `stores/search.rs` per §12.3; debounce task; §7.4 method shape.
2. `views/omni_search.rs` per §15: sectioned `uniform_list`s, sticky headers, selection-bar spring, gen-windowed cascades, footer hints. `ui/` recipes needed here get built now: `SlotView`, `Badge`, `CountLabel`, `KeyHint`, `EmptyState`, `ErrorState`.
3. Retire the old SearchPanel input path into the left-dock panel that *invokes* omni (`cmd-K` is primary; the docked panel shows pinned scope + recent results — 15-gui-references dual-surface note).
4. Perf harness bootstrap (§25.3): `typing_storm` scenario over a 10 k fixture index served by the stub *from a background thread with realistic 2–8 ms latencies*.

*Acceptance:* `typing_storm` gate green in CI; §15 acceptance list manually verified.

### M5 — RenderModel + streamed symbol page

*Goal: §9 + §16 over the client-side chunker.*

1. `render_model/` — §9.2 types + `chunker.rs`: markdown → `ProseBlock`s (pulldown-cmark, background executor), heading-split sections, `section_plan` synthesis with real `SizeHint`s, tree-sitter `Highlight` events per block (gpui-component highlighter registry), viewport-priority command handling (§9.4.5).
2. Stub `open_symbol` upgraded to emit the full protocol; property test for §9.3 invariants (proptest over event orderings/truncations → apply → assert valid page).
3. `stores/symbol.rs` (§12.4); `views/symbol_page/` — header, docs tab (`list()` + skeleton plan + `section.arrive` + `highlight.sweep`), refs/impls stub tabs with ticking counts, version picker shell.
4. `views/quick_peek.rs` (§23.2 — the truncated stream mode).
5. `doc_stream` perf scenario.

*Acceptance:* §16 acceptance list; `doc_stream` gate green; pulling the network cable mid-stream (stub latency injection) leaves the §9.1 partial-validity behavior.

### M6 — Jobs, sync, logs, toasts

1. `stores/{job,registry}.rs` (§12.5–6) fed by stub streams (synthetic generators behind `--fixtures` for development); `views/jobs_panel.rs` (§19) with row-migration choreography; LogStore seq upgrade + `views/log_panel.rs` finish (§20); toast pipeline end-to-end (`JobEvent` → `ToastRequest` → toast with action).
2. Status bar goes live (sync summary, MB/s smoothing, diagnostics chip).
3. `sync_storm` + `log_flood` scenarios.

*Acceptance:* §19/§20 acceptance lists; both new gates green.

### M7 — Project manager + trust

1. `stores/project.rs` (§12.2); stub `resolve_project` walks a real Cargo/npm workspace shallowly (manifest+lock parse — enough truth for real UI; full §14 typestates arrive with Wave-4 client).
2. First-run screen + `views/project_panel.rs` (§14) + trust modals (§23.4) + drag-drop-folder open.
3. Wire dep rows to `RegistryStore` (synthetic sync in stub; real at swap).

*Acceptance:* §14 acceptance list against a synthetic 250-dep resolve.

### M8 — Package browser

`stores` additions + `views/package_browser.rs` per §17 (tree + table + version select + partial-sync hatching).
*Acceptance:* §17 list.

### M9 — Graph

`views/graph/` per §18: layout worker (background executor), canvas element with element-state, LOD tiers, springs, quick-peek integration. Replace old `graph_view.rs`.
*Acceptance:* §18 list + `graph_500` gate.

### M10 — Timeline, diff, polish, Wave-4 swap readiness

1. §22 timeline canvas + diff view (token/paragraph LCS on background executor, streaming diff).
2. Settings screen (§21) complete; motion-scale + reduced-motion end-to-end (LD-17 audit of every §5 token's scale-0 behavior).
3. Polish pass: hover/press/focus micro-feedback sweep across every interactive element against §5.1; empty/error state sweep against §11.
4. **Swap seam rehearsal:** compile lindsey against a `client` crate mock implementing §7.1 verbatim (trait-object or feature-switch) proving `client_stub` deletion is a one-line change at Wave 4.
5. `tab_churn` gate; full §25.3 table green; license check (LD-12) in CI.

*Acceptance:* the §18.2 P0 demo script (LIBRARIFICATION §18.2) runs end-to-end on the stub: open project → trust gate → deps sync (synthetic) → offline local search → streamed symbol page — every transition animated per §5, HUD p95 < 8.3 ms throughout.

## §28 Testing strategy

1. **Kernel unit tests** (no GPUI): spring math (M1.4), slot truth table, gen supersession, drain batching, protocol property tests (M5.2), LCS diff.
2. **`#[gpui::test]` integration:** deterministic executors drive store+view pairs: search flow (type → events → selection → open), doc stream apply (scroll-position invariance assertions = the §9.4 zero-jump guarantee as a test), nav history, tab lifecycle.
3. **Leak tests:** `tab_churn` in-test variant asserting entity/subscription counts via HUD counters (§3.4).
4. **Perf gates:** §25.3 in CI.
5. **Manual visual QA checklist** (release-blocking, no pixel CI — §1.3): one pass per §5 token at scale 1.0 / 0.5 / 0.0; light+dark; 60 Hz + 120 Hz displays.

## §29 Risk register

| # | Risk | Sev | Mitigation |
|---|---|---|---|
| G1 | gpui/gpui-component pin drift breaks APIs on upgrade | M | LD-10 exact pins; upgrade = dedicated commit re-running §1.3 inventory |
| G2 | Spring-per-view render loops accidentally coupled to big entities (frame storms) | M | §4.2 contract review + HUD active-motion counter + M1.5 render-counter proof pattern reused in review |
| G3 | Layout animation creep (LD-5 erosion) | M | Token vocabulary is closed; new animation = PR to §5 tables |
| G4 | Client stub diverges from Wave-4 `client` shape | H | §7.1 signature is the contract; M10.4 swap rehearsal is release-blocking |
| G5 | Streaming protocol churn once REGISTRY serves real sections | M | `render.v1` versioned; `Unknown` fallbacks (LD-7); property tests pin invariants, not shapes |
| G6 | 120 Hz budget unreachable on low-end Linux/X11 | M | 60 Hz floor is the hard gate; ProMotion numbers are targets; load shedding §6.2 |
| G7 | Motion tastefulness drift (too much, too slow) | L | §5.4 budget rules + the ≤ 240 ms/≤ 700 ms caps are lintable constants in `tokens.rs` |
| G8 | `uniform_list` remount vs entrance-animation interaction regresses (replays) | M | Gen-window rule §4.1 is tested (scripted scroll in `typing_storm` asserts zero animations after window) |
| G9 | Blurred window appearance unsupported/ugly on some Linux compositors | L | Automatic opaque fallback; vibrancy is enhancement-only |
| G10 | Beginner implementers deviate from the §7.4 shape | M | The shape is a doc-tested macro-less template; code review checklist item #1 |

---

## Appendix A — Easing & spring reference

| Name | Source | Curve | Use |
|---|---|---|---|
| `linear` | gpui | t | crossfades, dash-flow |
| `ease_in_out` | gpui | quad in-out | generic two-sided |
| `ease_out_quint()` | gpui | 1−(1−t)⁵ | entrances (fast attack, long soft tail) |
| `ease_in_cubic` | gpui-component | t³ | exits only |
| `ease_out_cubic` | gpui-component | 1−(1−t)³ | small reveals |
| `pulsating_between(a,b)` | gpui | breathing sine | shimmer, status dots |
| `bounce(e)` | gpui | forward-then-back | badge pop, nav flash |
| `delayed(d,e)` | ours §4.1 | stagger shim | cascades |
| Spring DEFAULT 459/41.6/1 | ours | 350 ms settle (measured, travel-invariant) | docks, selection, chevrons, scroll |
| Spring SNAPPY 1225/67.9/1 | ours | 225 ms settle (measured, travel-invariant) | overlays, toasts, underline |
| Spring GENTLE 145/23.4/1 | ours | 600 ms settle (measured, travel-invariant) | progress, counts, graph |

## Appendix B — Keymap (macOS / Linux ctrl-equivalents)

| Binding | Action | Context |
|---|---|---|
| `cmd-K` | OpenOmniSearch | global |
| `cmd-shift-P` | OpenCommandPalette | global |
| `cmd-P` | OpenOmniSearch (files mode, P2) | global |
| `cmd-[` / `cmd-]` | NavigateBack / Forward | global |
| `cmd-B` | ToggleLeftDock | global |
| `cmd-J` | ToggleBottomDock | global |
| `cmd-W` / `cmd-1..9` | CloseTab / ActivateTab(n) | pane |
| `cmd-shift-[` / `]` | PrevInnerTab / NextInnerTab | symbol page |
| `cmd-,` | OpenSettings | global |
| `cmd-O` | OpenProject | global |
| `?` | ToggleShortcutsOverlay | no-input-focused |
| `F12` | ToggleHud | debug |
| `esc` | pop overlay / clear input | overlay/input |
| `↑↓ / tab / enter / cmd-enter / alt-enter / space` | §15, §23.2 semantics | overlays, lists |
| `y` / `v` / `g d,s,r,t` / `L` | §16 semantics | symbol page |
| `f` | FitGraphToView | graph |
| `s` / `d` | SyncSelected / DiffAgainstPrevious | project / package |

## Appendix C — Channel registry (capacities & overflow policy)

| Channel | Cap | Policy | Rationale |
|---|---:|---|---|
| `search` events | 256 | backpressure | pages are few and small |
| `open_symbol` DocEvents | 128 | backpressure | protocol-paced; client yields |
| `open_package` events | 128 | backpressure | tree levels + chunks |
| `resolve_project` events | 64 | backpressure | bounded by dep count pages |
| `sync` progress | 64 | **coalesce keep-latest per generation** | progress is idempotent state, not a log |
| `jobs` events | 128 | coalesce keep-latest per job for Progress; backpressure for lifecycle | completions must not drop |
| log lines | 1024 | drop-oldest | ring semantics end-to-end |
| `command` (GUI→client) | 64 | try_send + `CommandRejected` event | intents must never block UI |
| graph layout iterations | 2 | keep-latest | only the freshest layout matters |

Coalescing implementations live client-side (before the channel), so capacities are honest.

---

*End of GUI-PLAN Rev 1. Evidence: `docs/research/librarification/{03,15,18}` · LIBRARIFICATION-PLAN §13–§15/§18/GD-18/GD-22/GD-34 · pinned-source API verification per header. The plan is implementation-ready: kernels (Parts II–III) carry complete code; screens (Part VI) carry states, motion, keys, and acceptance; the program (§27) sequences it for a single implementer starting at M0 today.*
