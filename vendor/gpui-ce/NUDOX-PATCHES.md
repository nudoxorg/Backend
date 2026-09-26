# Nudox patches to vendored gpui-ce 0.2.2 and gpui_ce_components_base 0.2.0

Both crates are copied verbatim from crates.io (`gpui-ce 0.2.2`,
`gpui_ce_components_base 0.2.0`) minus their `examples/` directories (and the
matching `[[example]]` tables in the normalized `Cargo.toml`). They are wired
in through `[patch.crates-io]` in the root `Cargo.toml`. Every patched line is
marked with a `NUDOX:` comment; `grep -rn NUDOX: vendor/gpui-ce vendor/gpui_ce_components_base`
lists them all.

## Why: one clock

GPUI's animations read the wall clock (`scheduler::Instant::now()`), so a
headless capture could not step them: `advance_clock` moved timers but not
animations. Every animation time source below now reads
`cx.background_executor().now()`, the scheduler's `Clock`: real time in the
app, the `TestClock` under `TestAppContext`/`HeadlessAppContext`. Any frame at
any virtual time is then reproducible byte for byte (see
`apps/facet` `motion::tests::with_animation_advances_only_with_the_virtual_clock`).

## gpui-ce

| Site | Change | Why |
|---|---|---|
| `src/elements/animation.rs` `AnimationElement::request_layout` | start time and elapsed read `cx.background_executor().now()`; chained animations restart at that `now` | `with_animation` / `with_animations` step with the virtual clock |
| `src/elements/animation.rs` | the `0..=1` debug assertion on the eased delta became `is_finite()` | overshooting cubic-beziers (`bounce`, `spring`) leave `0..=1` by design and panicked debug builds |
| `src/transition.rs` `Transition` | `goal_last_updated_at` and every elapsed reading use the executor clock (`raw_evaluate`, `evaluate_delta`, `update`, `jump_to`, `default_goal_updated_at(now)`); `Instant` is `scheduler::Instant` | `window.use_transition` animations step with the virtual clock; same overshoot relaxation |
| `src/elements/img.rs` animated frames | GIF/WebP frame timing reads the executor clock | animated images are animation |
| `src/elements/img.rs` loading placeholder | `started_loading` and its delay check read the executor clock | the placeholder's delay is awaited on an executor timer; comparing it to the wall clock never showed the placeholder headless |
| `src/window.rs` `Window::with_element_opacity` | `pub(crate)` → `pub` | layout-neutral wrapper elements (`facet::motion::Offset`) fade a subtree the way `Div`'s `opacity` does |
| `src/text_system.rs` (4 `FontRun` builders) | a `letter_spacing` of exactly zero becomes `None` | CoreText reads an explicit `kCTKernAttributeName` of 0.0 as "disable kerning". Every run with `letter-spacing: 0` lost its kerning (Geist measured 2.1 % wider than Chrome, the serif italic 6.6 %); CSS `letter-spacing: 0` keeps kerning |

Left on the wall clock on purpose (not animation): frame pacing and the input
rate tracker in `window.rs`, the profiler, input-latency histograms,
`gestures.rs` scroll-gesture filtering, `app.rs` tab activation stamps,
`platform_scheduler.rs`, `bench_dispatcher.rs`, `visual_test_context.rs`
timeouts.

## gpui_ce_components_base

| Site | Change | Why |
|---|---|---|
| `src/scrollbar.rs` | `ScrollbarState::new(now)` (Default keeps the wall clock for its own tests); prepaint, wheel, hover, thumb-hover, drag-rate limiting and drag-end stamps read `cx.background_executor().now()`; `with_hovered_on_thumb` and `is_scrollbar_visible` take `now` | scrollbar fade/slide/width motion and its idle hold (whose timer already slept on the executor) now share one clock |
| `src/input/base/blink_cursor.rs`, `src/input/base/state.rs` | one active cursor lease owns one cancellable task; generation guards reject stale blink/resume deadlines; repeated starts preserve phase; focus and window activation share the active-and-focused predicate | closed, destroyed, and inactive inputs cannot retain or resurrect caret redraw timers; deterministic virtual-clock and native window activation tests cover the lifecycle |

Already on the executor clock, untouched: `motion.rs` (`transition`,
`spring`), `tooltip.rs` show/grace delays, `hover_card.rs`, `auto_scroll.rs`, and toasts (callers pass
`cx.background_executor().now()`). `history.rs` undo grouping and
`measure.rs` profiling stay on the wall clock (not animation).

`vendor/gpui_ce_components` has no animation time source of its own (its
`Instant::now` uses are highlighter budgets and table measurements).

## Compositing (W-Flow, 2026-09-25)

A subtree can be painted under a 2D transform and at a group opacity
(`gpui::layer(child).opacity(..).scale(..).scale_xy(..).origin(..).translate(..)`),
and cut plates get exact blurred polygon shadows. Designs weighed with
measurements in `apps/facet/src/motion/compositing/headless.rs`:

- **Transform: per-primitive, at scene insertion** (not an offscreen texture).
  Every `Window::paint_*` maps its geometry through the current
  `LayerTransform` before snapping; glyphs are *rasterized* at the layer's
  scale (quantized to 1/32, the residual stretched about the pen), SVGs at
  their transformed size. At scale 1.5 a layer's peak edge strength matches
  native 1.5x layout (756.1 vs 755.0) where a bilinear texture upscale —
  what an offscreen layer composites — loses 20 % (606.5), and the layer is
  2.7x closer to native pixels (mean |diff| 1.17 vs 3.19). Identity takes the
  untouched code path (one comparison per paint call): a layer at rest is
  bit-identical to no layer (tested).
- **Group opacity: offscreen group**, the one thing per-primitive math cannot
  do. Reuses the content-filter group machinery (a `FilterBoundary` pair with
  no filters); the Metal renderer composites the group 1:1 at the opacity.
  Result equals `alpha * rest + (1 - alpha) * background` to 0.65/255; the
  per-primitive fallback misses by up to 55.6/255 (tested). Opacity 1 creates
  no group.

| Site | Change | Why |
|---|---|---|
| `src/elements/layer.rs` (new) | `LayerTransform` (axis-aligned scale + offset: compose, inverse, bounds, length/raster scale) and the `Layer` element (`layer(child)`) | the element API; layout-neutral; translate via element offset, scale via the transform, opacity via a group |
| `src/elements/mod.rs` | `mod layer; pub use layer::*;` | export |
| `src/window.rs` `Window` fields | `layer_transform`, `isolation_depth`; content masks are stored in window space | transform state |
| `src/window.rs` `with_layer_transform`, `layer_transform`, `compositing`, `with_group_opacity` | new | the API; group opacity falls back to per-primitive opacity when the renderer lacks it or past `MAX_ISOLATION_DEPTH` (2, the renderers' group-texture pool) |
| `src/window.rs` `paint_quad`, `paint_path`, `paint_drop_shadows`, `paint_inset_shadows`, `paint_backdrop_filter`, `with_filter_layer`, `paint_underline`, `paint_strikethrough`, `paint_glyph`, `paint_emoji`, `paint_svg`, `paint_image`, `paint_surface` (both), `paint_layer` | geometry through the layer transform; lengths (radii, borders, blur, strokes) by its smaller axis; glyphs rasterized at `raster_scale` | the transform reaches every primitive |
| `src/window.rs` `insert_hitbox` | hitbox bounds and mask in window space under the transform | hit-testing follows what is painted |
| `src/window.rs` `with_content_mask`, `content_mask`, `window_content_mask` | masks pushed in local space are stored transformed; `content_mask()` answers in local space (inverse) so element culling stays consistent | clipping under a transform |
| `src/window.rs` `DeferredDraw` | carries the layer transform it was deferred under; prepaint/paint re-apply it | anchored overlays follow a transformed trigger |
| `src/window.rs` `paint_chamfer_shadows` | new: `Shadow { inset: Shadow::CHAMFER, corner_radii: chamfers }`; rounded-box fallback without renderer support | cut-plate shadows |
| `src/window.rs` `stretch_about`, `glyph_placement` | helpers | glyph placement under scale |
| `src/scene.rs` `Scene::insert_primitive` | an opacity-only group raises the order floor at its start (it owns content that does not overlap its marker) and its end marker carries the union of its content (the composite region); groups nest | a row falling in from above its slot stays inside its group |
| `src/scene.rs` `Shadow::{DROP, INSET, CHAMFER}` | named `inset` values; `CHAMFER = 2` | chamfered shadow mode |
| `src/scene.rs` `Path::apply_layer_transform` | new | paths under a transform |
| `src/platform.rs` `Compositing`, `PlatformWindow::compositing`, `PlatformHeadlessRenderer::compositing` | new, default `{ false, false }` | capability, so wgpu/DirectX (unpatched) never get a group they would drop |
| `src/platform/test/window.rs` `TestWindow::compositing` | asks its headless renderer | headless captures report Metal's capabilities |
| `src/view.rs` `ViewElementCacheKey` | + layer transform + element opacity | a cached view replays primitives with the transform and opacity they were painted under |

## gpui_ce_macos 0.1.0 (vendored 2026-09-25, `vendor/gpui_ce_macos`)

Copied verbatim from crates.io, wired through `[patch.crates-io]`. Patched
lines carry `NUDOX:` comments.

| Site | Change | Why |
|---|---|---|
| `src/metal_renderer.rs` `build_pipeline_state`, `build_path_sprite_pipeline_state` | destination alpha factor `One` -> `OneMinusSourceAlpha` | alpha composites "over" (was additive, saturating). Identical on opaque targets; inside a group it keeps coverage exact |
| `src/metal_renderer.rs` `group_composite_pipeline_state` + the `FilterBoundary` end branch | a group with no filters is composited 1:1 at its opacity over the end marker's bounds (was: skipped, because `sigma == 0` returned early — the group's content vanished) | group opacity |
| `src/metal_renderer.rs` `MetalHeadlessRenderer::compositing`, `src/window.rs` `MacWindow::compositing` | `{ group_opacity: true, chamfer_shadows: true }` | capability |
| `src/shaders.metal` `shadow_vertex`, `shadow_fragment` | `inset == 2`: chamfered-rectangle drop shadow — each row's span blurred exactly along x (erf), integrated over y with 8 samples; exact SDF at zero blur | coverage error vs the CPU-convolved polygon: worst 0.0057, mean 0.00073 (the rounded box it replaces: 0.1496, 0.0066) |
| `src/shaders.metal` `group_composite_fragment` | new: texel-exact copy of the group times opacity | group opacity |

## gpui_ce_components 0.2.0 (highlighter)

| Site | Change | Why |
|---|---|---|
| `Cargo.toml` | `tree-sitter` 0.26 -> 0.27, `tree-sitter-go` 0.23 -> 0.25, `tree-sitter-python` 0.23 -> 0.25; JSON moved from the base `tree-sitter` feature to its own `tree-sitter-json` feature | use the grammar versions the engine's frontends already build (one copy in the desktop binary, no new downloads) |
| `src/highlighter/highlighter.rs` | `QueryMatch::captures()` accessor (0.27 made the field private); `SyntaxHighlighter::captures(range)` | 0.27; capture names for `facet::code` roles |
| `src/highlighter/languages.rs` | `Json` behind `tree-sitter-json`; C# uses the grammar's `HIGHLIGHTS_QUERY` (was `""`: no highlighting); C++ uses `languages/cpp/highlights.scm` | C# and C++ were unhighlighted |
| `src/highlighter/languages/cpp/highlights.scm` (new) | C's query plus C++'s, specific patterns first (upstream's C++ query is written for `; inherits: c`, which this highlighter does not resolve) | C++ highlighting |
| `src/highlighter/languages/rust/highlights.scm` | `(lifetime "'" @label)` | a lifetime reads as one token |
