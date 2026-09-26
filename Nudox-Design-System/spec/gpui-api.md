# GPUI-CE API Cheat Sheet (for a highly-animated, performant desktop UI)

Sources (read-only research; nothing here was compiled/run):

- **`GPUI`** = `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/gpui-ce-0.2.2` (the version the app actually compiles against)
- **`MACOS`** = `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/gpui_ce_macos-0.1.0`
- **`PLATFORM`** = `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/gpui_ce_platform-0.1.0`
- **`ELEM`** = `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/gpui_ce_elements-0.1.0`
- **`SCHED`** = `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/gpui_ce_scheduler-0.2.2`
- **`CE3`** = `~/projects/gpui-ce3/crates/gpui` (a newer checkout of the same project — noted only where it *differs* from 0.2.2)

All paths below are relative to these roots unless given in full. All line numbers are from the files as they exist on disk right now.

---

## 1. Animation

### `Animation` / `AnimationExt::with_animation`

`GPUI/src/elements/animation.rs`:

```rust
// 14-22
pub struct Animation {
    pub duration: Duration,
    pub oneshot: bool,
    pub easing: Rc<dyn Fn(f32) -> f32>,
}
// 27-33  Animation::new(duration) -> oneshot=true, easing=linear
// 36-39  .repeat() -> oneshot=false
// 44-47  .with_easing(f: impl Fn(f32)->f32 + 'static)

// 57-93  trait AnimationExt (blanket-impl'd for every IntoElement + 'static, line 95)
fn with_animation(self, id: impl Into<ElementId>, animation: Animation,
                   animator: impl Fn(Self, f32) -> Self + 'static) -> AnimationElement<Self>;
fn with_animations(self, id: impl Into<ElementId>, animations: Vec<Animation>,
                    animator: impl Fn(Self, usize, f32) -> Self + 'static) -> AnimationElement<Self>;
```

Usage (from the crate's own test, lines 309-316):

```rust
div().size_full().child(div().with_animation(
    "repeating-animation",
    Animation::new(Duration::from_secs(1)).repeat(),
    move |this, delta| {
        // delta ∈ [0,1] after easing; mutate `this`'s style/children here
        this.opacity(delta)
    },
))
```

### Easing functions (`GPUI/src/elements/animation.rs:235-289`, `mod easing`, re-exported `pub use easing::*` at line 9)

- `linear(delta) -> delta` (239-241)
- `quadratic(delta) -> delta*delta` (244-246)
- `ease_in_out(delta)` — quadratic in/out (249-256)
- `ease_out_quint() -> impl Fn(f32)->f32` — note: returns a *closure factory*, not a plain fn (259-261)
- `bounce(easing: impl Fn(f32)->f32) -> impl Fn(f32)->f32` — plays `easing` forward then backward (264-272)
- `pulsating_between(min, max) -> impl Fn(f32)->f32` — sine/cubic "breathing" alpha (275-288)

That's the complete easing set in 0.2.2 — no built-in cubic-bezier/back/elastic. `CE3` adds real physical springs (see below), and its easing closures are explicitly allowed to overshoot 0..1 (see delta below), which is what lets a bounce/elastic feel be built.

### Repeat/looping

`Animation::repeat()` just clears `oneshot`. In `request_layout` (animation.rs:161-190), when `oneshot` is true and `delta > 1.0`, delta is clamped to `1.0` and the element is marked `done` (no more frames scheduled); when `oneshot` is false, `delta %= 1.0` forever and a frame is always re-requested. `reduce_motion()` (see `App::reduce_motion`) short-circuits to a single static frame (`delta = 1.0` for oneshot, `0.0` for repeat) — **`with_animation` already respects the OS "reduce motion" accessibility setting for you** (doc comment lines 52-56).

### How an element is re-rendered per frame / `window.request_animation_frame()`

`GPUI/src/window.rs:2340-2355`:

```rust
pub fn request_animation_frame(&self) {
    let entity = self.current_view();
    self.on_next_frame(move |_, cx| cx.notify(entity));
}
```

So "animation frame" is **not** a platform vsync callback you hook into directly from element code — it is: (1) schedule a closure via `on_next_frame` (`window.rs:2336-2338`, pushed into `next_frame_callbacks`), (2) that closure calls `cx.notify(current_view)`, which marks the *nearest enclosing `Render` entity* dirty. The platform's real display-link/vsync (see `MACOS/src/display_link.rs`) is what eventually causes GPUI to actually draw a new frame and run `next_frame_callbacks`; `AnimationElement::request_layout` calls `window.request_animation_frame()` every frame it isn't `done` (animation.rs:201-203).

`Window::on_next_frame` (window.rs:2336) and the test-only `Window::simulate_next_frame` (window.rs:2361-2369, `#[cfg(any(test, feature = "test-support"))]`) drain exactly that queue — this is what headless tests use instead of a real frame loop.

### Driving spring/retargetable animations manually

**0.2.2 has no spring primitive.** The only manual-animation building blocks are:
- `scheduler::Instant` (re-exported by `use scheduler::Instant;` at animation.rs:1) for measuring elapsed time yourself, stored per-element via `window.with_element_state` (see below), and
- `window.request_animation_frame()` to keep getting re-polled.

The idiom (this is literally what `AnimationElement::request_layout` does, animation.rs:149-207) is:

```rust
struct MyAnimState { start: Instant, from: f32, to: f32 }
window.with_element_state(global_id.unwrap(), |state, window| {
    let state = state.unwrap_or_else(|| MyAnimState { start: Instant::now(), from: 0.0, to: 1.0 });
    let t = state.start.elapsed().as_secs_f32() / duration_secs;
    if t < 1.0 { window.request_animation_frame(); }
    // ... build element using lerp(from, to, ease(t.min(1.0))) ...
});
```

`CE3` adds a full spring system instead — **not present in 0.2.2** (`grep -rl SpringConfig/SpringAnimation` against `GPUI/src` returns nothing):

`CE3/src/spring.rs`:

```rust
// 13-20  physical spring parameters
pub struct SpringConfig { pub stiffness: f32, pub damping: f32, pub mass: f32 }
// 24-30  SpringConfig::new(stiffness, damping, mass)
// 43-51  step(state, target, delta_time) -> SpringState   (analytic, frame-rate independent, preserves velocity — safe to retarget mid-flight)
// 57-...  step_ramp(...) for a target moving at constant velocity (e.g. a dragged handle)
// 146-... is_settled(state, target, epsilon) -> bool
// 252-262 pub struct SpringState { pub position: f32, pub velocity: f32 }
// 264-... pub trait SpringTarget  (impl'd for f32, Pixels, Rems, bool, AnimationPhase)
// 459-471 pub enum SpringPlayback { Running, Paused, Stopped, Completed, Cancelled }
// 475-534 pub struct SpringAnimation<T> — builder: SpringAnimation::new(config).to(target).with_epsilon(e).playback(p).from(initial)
```

And the element-level entry point, `CE3/src/elements/animation.rs`:

```rust
// AnimationExt (extended trait)
fn with_spring<T: SpringTarget>(self, id: impl Into<ElementId>, animation: SpringAnimation<T>,
                                 animator: impl FnOnce(Self, T::Output) -> Self + 'static) -> SpringAnimationElement<Self>;
```

Critically, `SpringAnimationElement::request_layout` reads time via **`cx.background_executor().now()`** (`CE3/src/elements/animation.rs`, inside the spring `Element` impl, comment: *"Use the executor clock so spring progression is deterministic in tests"*) — NOT `Instant::now()`. This is the one animation primitive in either checkout whose clock is virtualizable in tests (see §10 — this is the load-bearing finding for that question).

`CE3` also adds, on plain `Animation` (still wall-clock driven, unchanged):
- `Animation::repeat_synced()` — phase-locks to a shared app-wide clock instead of a per-element start time (so multiple independently-mounted animations with the same duration stay in phase).
- `Animation::with_max_fps(f32)` — throttles re-render frequency instead of re-rendering every platform frame.
- Easing output is **no longer clamped to 0..1** (doc: *"The result may exceed 0..1 for easing functions that overshoot"*), enabling true overshoot/elastic effects.
- `sampled_easing(config, epsilon) -> (Duration, impl Fn(f32) -> f32)` (`CE3/src/spring.rs:541`) adapts a zero-velocity spring into the classic duration+easing API for one-shot use, with the caveat that *retargeting it restarts the spring* (only `SpringConfig::step` preserves velocity across retargets).

### Cost model — does `with_animation` keep the whole view dirty?

**No — it dirties exactly one entity (the nearest enclosing `Render`/view), not the whole window, and not even the whole element subtree of unrelated siblings.** Evidence, `window.rs`:

- `request_animation_frame` → `on_next_frame(|_, cx| cx.notify(current_view))` (2352-2355) — `current_view()` (4834) is the `EntityId` of the nearest `View`-boundary ancestor (see `ViewElement` in §11), not the root.
- `App::notify(entity_id)` (`app.rs:2709-2728`) looks up only the `WindowInvalidator`s that currently track that entity (`window_invalidators_by_entity`, filtered by `tracked_entities`) and calls `WindowInvalidator::invalidate_view` on those.
- `WindowInvalidator::invalidate_view` (`window.rs:154-166`) inserts the entity into a `dirty_views: FxHashSet<EntityId>` and sets a single `dirty` bool — it does **not** walk the whole tree.
- `Window::mark_view_dirty` (`window.rs:1937-1949`) walks *up* from the notified view to the root via `dispatch_tree.view_path_reversed`, inserting each ancestor into `dirty_views`, **stopping as soon as it hits an already-dirty ancestor** — so the cost of marking is O(depth), not O(tree size).
- The payoff is in `ViewElement::prepaint` (`view.rs:380-401`): a view with `cached_style` (via `Entity::cached`/`AnyView::cached`, see §11) reuses its whole previous prepaint/paint range instead of re-rendering **as long as** `bounds`/`content_mask`/`text_style` are unchanged **and** `!window.dirty_views.contains(&entity_id)` **and** `!window.refreshing`. So: an animated element inside view A only forces view A to re-`render()`; a cached sibling view B elsewhere in the tree is untouched and its previous paint primitives are replayed verbatim (`window.reuse_prepaint` / `window.reuse_paint`).
- Caveat: `Window::refresh()` (`window.rs:2013-2019`) — used e.g. by `.request_animation_frame()`'s equivalents that call `cx.refresh()` rather than `cx.notify(entity)` — **does** force a full redraw and bypasses all view caching (`ViewElementState` cache check explicitly excludes `window.refreshing`). Prefer `with_animation`/`cx.notify(entity)` over `window.refresh()` for hot animation loops.

Practical implication for a highly-animated UI: **wrap each independently-animating region in its own `Entity`/`Render` view** (or use `.cached()` on siblings) so that `cx.notify()`/`with_animation` inside a hot loop only re-renders that one small subtree, not the whole window.

---

## 2. Transforms / opacity / compositing

### What can be transformed

Only two paint primitives carry a `TransformationMatrix` (rotate + non-uniform scale + translate, composed in that order): `MonochromeSprite` (glyphs and **monochrome SVGs**) and nothing else.

`GPUI/src/scene.rs:809-919`:

```rust
#[repr(C)]
pub struct TransformationMatrix { pub rotation_scale: [[f32; 2]; 2], pub translation: [f32; 2] }
impl TransformationMatrix {
    pub fn unit() -> Self;                                   // 823-829
    pub fn translate(self, point: Point<ScaledPixels>) -> Self;  // 840
    pub fn rotate(self, angle: Radians) -> Self;              // 848
    pub fn scale(self, size: Size<f32>) -> Self;              // 861
    pub fn compose(self, other: Self) -> Self;                // 866 — self ∘ other (other applied first)
}
```

`GPUI/src/elements/svg.rs:44-47, 185-257` — `Svg::with_transformation(Transformation)`:

```rust
pub struct Transformation { scale: Size<f32>, translate: Point<Pixels>, rotate: Radians }
// builders: Transformation::scale(Size<f32>) / ::translate(Point<Pixels>) / ::rotate(impl Into<Radians>)
//           .with_scaling(..) / .with_translation(..) / .with_rotation(..)
```

```rust
svg().path("icons/chevron.svg").size_6()
    .with_transformation(Transformation::rotate(radians(open_amount * PI)))
```

Doc comment (svg.rs:43): *"this won't effect the hitbox or layout of the element, only the rendering"* — rotation/scale is purely visual, hit-testing still uses the untransformed layout bounds (see §3).

`PolychromeSprite` (used by `window.paint_image`, `scene.rs:954-963`) has **no transformation field at all** — regular raster images (`img()`/`paint_image`) cannot be rotated or non-uniformly scaled via the paint API; you can only change their `bounds`/`image_bounds` (crop/stretch into a rect). To rotate a bitmap you'd have to pre-rotate the pixel data or render it into an svg-style monochrome mask, or (in `CE3`, see below) use its wgpu path.

There is **no generic per-`div()` transform** (no `.rotate()`/`.scale()` fluent style on `Styled`/arbitrary elements) in 0.2.2 or `CE3` — transforms are only available on the `svg()` element specifically.

### `opacity()`

`GPUI/src/styled.rs:812-815`:

```rust
fn opacity(mut self, opacity: f32) -> Self { self.style().opacity = Some(opacity); self }
```

Backed by `Style.opacity: Option<f32>` (`style.rs:303-304`). This is **not a compositing layer** — it's multiplied into each primitive's alpha at paint time. Every `window.paint_*` call reads `self.element_opacity()` and multiplies it into the color it emits, e.g. `paint_quad` (`window.rs:4104-4116`): `background: quad.background.opacity(opacity)`, and identically for `paint_drop_shadows`/`paint_inset_shadows` (3932/3968), `paint_path` (4196), `paint_underline`/`paint_strikethrough`, `paint_svg` (4494: `color.opacity(element_opacity)`), `paint_image` opacity field. **Consequence**: `.opacity(0.5)` on a `div()` with overlapping children (e.g. two overlapping colored rects) will show the seam where they overlap (each primitive is independently alpha-blended against the background), *not* a single group composited at 50% against the background — unless you specifically use a filter group (below).

### Layer compositing

There *is* real offscreen-layer compositing, but only for **filters** (blur), via `Window::with_filter_layer` (`window.rs:4037-4090`, doc: *"the renderer renders everything `f` paints into an offscreen target, blurs it as a single layer, and composites the result"*) and `Window::paint_backdrop_filter` (4000-4035, CSS `backdrop-filter`/frosted-glass — blurs whatever was already painted behind the rect). Fluent entry points: `Styled::blur(radius)` (`styled.rs:46`) and `Styled::backdrop_blur(radius)` (`styled.rs:68`). Note from the source comment (window.rs:4067-4071): the filter group is composited at `opacity: 1.0`, *not* `element_opacity()`, specifically so `.blur(r).opacity(0.5)` doesn't double-apply opacity — children already carry the element's opacity individually.

Also: `Window::paint_layer(bounds, f)` (`window.rs:3892-3915`) — a lighter-weight "layer" that's really just a scene batching/z-index boundary (`scene.push_layer`/`pop_layer`) for non-overlapping geometry batching, not a true offscreen render target — used internally by `div()` painting, not a compositing primitive per se.

### Translating an element

No generic `translate()` on arbitrary elements either. Options: (a) style-driven layout (margins, `inset()`/absolute positioning via `Position::Absolute`), used by every normal element; (b) `Window::with_element_offset(offset, f)` (used internally by `anchored()`, `elements/anchored.rs:210`) shifts everything painted by `f` by a pixel offset — this is what you'd reach for to move a custom/overlay element imperatively frame-to-frame (e.g. as the output of a spring); (c) for SVGs, `Transformation::translate(Point<Pixels>)` above.

---

## 3. Custom drawing

### `canvas()`

`GPUI/src/elements/canvas.rs` (full file, 96 lines):

```rust
pub fn canvas<T>(
    prepaint: impl 'static + FnOnce(Bounds<Pixels>, &mut Window, &mut App) -> T,
    paint: impl 'static + FnOnce(Bounds<Pixels>, T, &mut Window, &mut App),
) -> Canvas<T>
```

`prepaint` runs during the prepaint phase (bounds are known; good place to call `window.insert_hitbox`/measure text) and returns arbitrary state `T`; `paint` runs during paint and receives that state. `Canvas` implements `Styled` (canvas.rs:91-95) so normal box styling (size, background, etc.) still applies via `style.paint(...)` (canvas.rs:85).

```rust
canvas(
    |bounds, window, _cx| window.insert_hitbox(bounds, HitboxBehavior::Normal),
    |bounds, hitbox, window, cx| {
        window.paint_quad(fill(bounds, red()));
        if hitbox.is_hovered(window) { /* draw hover ring */ }
    },
).size_full()
```

### `Element` trait (`GPUI/src/element.rs:51-142`)

```rust
pub trait Element: 'static + IntoElement {
    type RequestLayoutState: 'static;
    type PrepaintState: 'static;
    fn id(&self) -> Option<ElementId>;
    fn source_location(&self) -> Option<&'static panic::Location<'static>>;
    fn request_layout(&mut self, id: Option<&GlobalElementId>, inspector_id: Option<&InspectorElementId>,
                       window: &mut Window, cx: &mut App) -> (LayoutId, Self::RequestLayoutState);
    fn prepaint(&mut self, id: Option<&GlobalElementId>, inspector_id: Option<&InspectorElementId>,
                bounds: Bounds<Pixels>, request_layout: &mut Self::RequestLayoutState,
                window: &mut Window, cx: &mut App) -> Self::PrepaintState;
    fn paint(&mut self, id: Option<&GlobalElementId>, inspector_id: Option<&InspectorElementId>,
             bounds: Bounds<Pixels>, request_layout: &mut Self::RequestLayoutState,
             prepaint: &mut Self::PrepaintState, window: &mut Window, cx: &mut App);
    // + a11y_role/write_a11y_info/a11y_synthetic_children with default impls (106-136)
}
```

Three-phase contract enforced by `Drawable<E>` (element.rs:281-577): `request_layout` (ask Taffy for a `LayoutId`, once) → `prepaint` (bounds now known; commit hitboxes, measure) → `paint` (emit scene primitives). Calling them out of order panics (element.rs:367/480/522, `"must call request_layout only once"` etc). `id()` returning `Some` gives the element a stable `GlobalElementId` used to key persistent per-element state across frames (see `with_element_state` below).

### `window.paint_quad`

`GPUI/src/window.rs:4092-4101` + helpers `quad()`/`fill()`/`outline()` at 6881-6920:

```rust
pub fn paint_quad(&mut self, quad: PaintQuad); // rounded rect w/ background+border, opacity-aware, snapped to device pixels
pub fn quad(bounds, corner_radii: impl Into<Corners<Pixels>>, background: impl Into<Background>,
            border_widths: impl Into<Edges<Pixels>>, border_color: impl IntoColor<Hsla>, border_style: BorderStyle) -> PaintQuad;
pub fn fill(bounds: impl Into<Bounds<Pixels>>, background: impl Into<Background>) -> PaintQuad;
pub fn outline(bounds: impl Into<Bounds<Pixels>>, border_color: impl IntoColor<Hsla>, border_style: BorderStyle) -> PaintQuad;
```

Note (window.rs:4098-4100): `corner_radii` are allowed to exceed bounds (sharp "witch's hat" corners); use `Corners::clamp_radii_for_quad_size` to avoid it. Quads are SDF-rendered on the GPU (rounded-rect signed-distance-field, analytically antialiased — see `MACOS/src/shaders.metal:128-212` "antialias_threshold" fast paths) — **this is the cheapest way to draw a rounded/chamfer-like shape** if a single fixed corner radius per corner is enough; for a true chamfered octagon you need `paint_path` instead (see below), since a quad only supports 4 independent corner radii, not chamfers on an arbitrary edge count.

### `paint_path` + `PathBuilder`

`GPUI/src/path_builder.rs` (full file, 348 lines) wraps `lyon` tessellation:

```rust
pub struct PathBuilder { /* lyon SvgPathBuilder + optional Transform + style + dash_array */ }
pub fn stroke(width: Pixels) -> PathBuilder;   // 88
pub fn fill() -> PathBuilder;                  // 96  (== PathBuilder::default())
pub fn with_style(self, style: PathStyle) -> Self;   // PathStyle::Stroke(StrokeOptions) | Fill(FillOptions)
pub fn dash_array(self, dashes: &[Pixels]) -> Self;  // 108
pub fn move_to/line_to/curve_to/cubic_bezier_to/arc_to/relative_arc_to/add_polygon/close(&mut self, ...); // 125-201
pub fn transform/translate/scale/rotate(&mut self, ...);  // 205-240, lyon::math::Transform
pub fn build(self) -> Result<Path<Pixels>, Error>;  // 244-255, tessellates via lyon Fill/StrokeTessellator
```

```rust
window.paint_path(bounds, path.color) // actually: window.paint_path(path: Path<Pixels>, color: impl Into<Background>)
```
(`window.rs:4187-4202`).

**Important limitation**: one `PathBuilder`/`Path` is *either* a fill *or* a stroke (the `PathStyle` enum is single-valued, `path_builder.rs:17-22`, and `build()` dispatches to exactly one of `tessellate_fill`/`tessellate_stroke`, 251-254). **To draw a filled-and-stroked chamfered/octagonal polygon you build two `PathBuilder`s (one `::fill()`, one `::stroke(width)`) tracing the same points, and call `window.paint_path` twice** (fill first, then stroke on top) — there is no single call that fills+strokes one path.

```rust
let mut fill_path = PathBuilder::fill();
fill_path.move_to(p0); for p in &octagon_pts[1..] { fill_path.line_to(*p); } fill_path.close();
window.paint_path(fill_path.build()?, fill_color);

let mut stroke_path = PathBuilder::stroke(px(1.5));
stroke_path.move_to(p0); for p in &octagon_pts[1..] { stroke_path.line_to(*p); } stroke_path.close();
window.paint_path(stroke_path.build()?, stroke_color);
```

**Antialiasing of tessellated paths**: `Path::push_triangle` (scene.rs:1083-1119) stores per-vertex `st_position` UV coordinates. For straight-edge fan triangles (`line_to`, scene.rs:1057-1064) all three vertices get the *same* `st` (`(0,1),(0,1),(0,1)`); for curve segments (`curve_to`) the three vertices get distinct `st` implementing the classic Loop–Blinn implicit-quadratic AA technique. In the Metal fragment shader (`MACOS/src/shaders.metal:772-812`, `path_rasterization_fragment`), when the `st` derivative is ~0 (straight edges) `alpha = 1.0` unconditionally (no analytic AA on the flat edges of a polygon — that path is filled by hard triangle rasterization only); when the derivative is non-zero (curve edges) it computes the implicit function `f = s² − t`, `alpha = saturate(0.5 − distance)` — a genuine 1px analytically-antialiased edge. **So: bezier/arc edges you build with `curve_to`/`cubic_bezier_to`/`arc_to` are antialiased by the shader; dead-straight polygon edges (as an octagon would mostly be) rely on the path being rasterized into an intermediate atlas tile and then sampled via `path_sprite_fragment` (shaders.metal:842+) — I did not fully trace whether that composite sample uses bilinear filtering that would soften straight edges; treat straight-edge AA quality as unverified and prefer building rounded corners via multiple small `arc_to`s (which do get analytic AA) if the chamfers need to look crisp at low pixel radii.**

### `paint_shadow` / `BoxShadow`

`GPUI/src/style.rs:349-362`:
```rust
pub struct BoxShadow { pub color: Hsla, pub offset: Point<Pixels>, pub blur_radius: Pixels, pub spread_radius: Pixels, pub inset: bool }
impl BoxShadow { pub fn new(offset_x, offset_y, color) -> Self; .blur_radius(px) .spread_radius(px) }
```
Fluent style methods come from `gpui_macros::box_shadow_style_methods!()` (invoked at `styled.rs:35`, e.g. `.shadow_sm()`/`.shadow_md()`/`.shadow(Vec<BoxShadow>)` — exact generated names live in the `gpui_macros` crate, not inspected here). Direct paint API: `Window::paint_drop_shadows` (non-inset, window.rs:3922-3953) and `Window::paint_inset_shadows` (3958-3998) — call drop-shadows *before* your background quad, inset-shadows *after*, per their doc comments.

### Gradients / `Background`

`GPUI/src/color.rs`:
```rust
pub fn solid_background(color: impl IntoColor<Hsla>) -> Background;      // 353
pub fn linear_gradient(angle: f32, from: impl Into<LinearColorStop>, to: impl Into<LinearColorStop>) -> Background; // 367-379, interpolates in Oklab by default
pub struct LinearColorStop { pub color: SceneHsla, pub percentage: f32 } // 386-391
```
`Background.color_space(ColorSpace)` (color.rs:483) lets you switch interpolation between `Srgb`/`Oklab` (261-266). `Background` is accepted anywhere a fill color is (`fill()`, `paint_quad`, `div().bg(...)`), so gradients work on ordinary divs, not just paths.

### `paint_svg` / `paint_image`

```rust
// window.rs:4444-4458 (signature)
pub fn paint_svg(&mut self, bounds: Bounds<Pixels>, path: SharedString, data: Option<&[u8]>,
                  transformation: TransformationMatrix, color: Hsla, cx: &App) -> Result<()>;
// window.rs:4514+ (signature)
pub fn paint_image(&mut self, bounds: Bounds<Pixels>, image_bounds: Bounds<Pixels>,
                    corner_radii: Corners<Pixels>, data: Arc<RenderImage>, frame_index: usize,
                    grayscale: bool) -> Result<()>;
```
Both go through a `sprite_atlas.get_or_insert_with(params, ...)` content-addressed cache keyed by path/size (svg) or image id+frame (image) so repeated paints of the same asset don't re-rasterize/re-upload (see §11).

### Hit-testing custom shapes

`Window::insert_hitbox(bounds: Bounds<Pixels>, behavior: HitboxBehavior) -> Hitbox` (window.rs:4778-4792) must be called during **prepaint**; it only takes a rectangle. `Hitbox::is_hovered(&self, window) -> bool` (window.rs:792-794) / `HitboxId::is_hovered` (707-745) checks membership in `window.mouse_hit_test`, and returns `false` while the last input modality was keyboard (so hover styles don't light up during keyboard nav) — use `should_handle_scroll` instead specifically for `ScrollWheelEvent` handling (documented distinction, 796-804). `HitboxBehavior` (809-...) has `Normal` (default) and occlusion variants (`BlockMouse`/`BlockMouseExceptScroll`, wired to `.occlude()`/`.block_mouse_except_scroll()`, `elements/div.rs:1243/1259`). **For a non-rectangular shape (e.g. your octagon), GPUI gives you only the bounding-box hitbox** — for pixel-accurate hit-testing you register a rectangular hitbox for the bounds and then, inside your `on_mouse_down`/`on_mouse_move` handler (or in `paint`, comparing `window.mouse_position()` against `hitbox.bounds`), do your own point-in-polygon test against the mouse position before treating it as a hit.

---

## 4. Text

### Font registration from bytes

`GPUI/src/text_system.rs:102-105`:
```rust
pub fn add_fonts(&self, fonts: Vec<Cow<'static, [u8]>>) -> Result<()>
```
(`TextSystem::add_fonts`, thin wrapper over `self.platform_text_system.add_fonts(fonts)`). Reached via `cx.text_system().add_fonts(vec![Cow::Borrowed(include_bytes!("../assets/MyFont.ttf"))])`.

### Variable-font weight

`Font.weight: FontWeight` where `FontWeight(pub f32)` (text_system.rs:916-920) — **a continuous scalar**, not an enum; named consts exist for convenience (`FontWeight::THIN..BLACK`, 100..900 in steps of 100, text_system.rs:949-980) but any `f32` (e.g. `FontWeight(450.0)`) is valid. On macOS this flows straight through: `MACOS/src/text_system.rs:722-724`, `fn fontkit_weight(value: FontWeight) -> FontkitWeight { FontkitWeight(value.0) }` — the raw float is handed to `font-kit`/CoreText's descriptor matching, so a variable font's `wght` axis *can* be driven continuously, though whether CoreText interpolates the axis vs. snapping to the nearest named instance is a CoreText/font-kit matching detail I did not verify further (flagged as unconfirmed).

### Font features

`GPUI/src/text_system/font_features.rs:1-42`:
```rust
pub struct FontFeatures(pub Arc<Vec<(String, u32)>>);
impl FontFeatures {
    pub fn disable_ligatures() -> Self;      // sets "calt" -> 0
    pub fn tag_value_list(&self) -> &[(String, u32)];
    pub fn is_calt_enabled(&self) -> Option<bool>;
}
```
Set via `Font { features: FontFeatures(...), .. }`; deserializable from JSON as a `{ "tag": true/false/number }` map (font_features.rs:44-100+).

### `StyledText` / `TextRun`s / highlights

`GPUI/src/elements/text.rs:392-...`:
```rust
pub struct StyledText { /* text: SharedString, runs, delayed_highlights */ }
impl StyledText {
    pub fn new(text: impl Into<SharedString>) -> Self;                     // 402
    pub fn with_default_highlights(self, default_style: &TextStyle,
        highlights: impl IntoIterator<Item=(Range<usize>, HighlightStyle)>) -> Self; // 419
    pub fn with_highlights(self, highlights: impl IntoIterator<Item=(Range<usize>, HighlightStyle)>) -> Self; // 434
}
```
`TextRun` (text_system.rs:1018-1035): `{ len, font: Font, color: Hsla, background_color, underline, strikethrough, letter_spacing }` — the low-level unit `shape_line` consumes.

### `InteractiveText`

`GPUI/src/elements/text.rs:1201-1276`:
```rust
pub struct InteractiveText { /* .. */ }
impl InteractiveText {
    pub fn new(id: impl Into<ElementId>, text: StyledText) -> Self;               // 1227
    pub fn on_click(self, ranges: Vec<Range<usize>>,
                     listener: impl Fn(usize, &mut Window, &mut App) + 'static) -> Self; // 1242
    pub fn on_hover(self, listener: impl Fn(Option<usize>, MouseMoveEvent, &mut Window, &mut App) + 'static) -> Self; // 1261
    pub fn tooltip(self, builder: impl Fn(usize, &mut Window, &mut App) -> Option<AnyView> + 'static) -> Self; // 1270
}
```
`on_click` ranges are matched against click-down/up character indices (1250-1252: both down and up index must fall in the same range, so a drag-select doesn't fire a click). `on_hover`/`tooltip` are indexed **per character**, driven by hit-testing the text's own hitbox each mouse-move (text.rs:1412-1476) — this is how you'd build "hover a word, see a tooltip for just that word".

### Text measurement

`GPUI/src/text_system.rs:407-446` (`WindowTextSystem::shape_line`, single line only — panics on `\n`):
```rust
pub fn shape_line(&self, text: SharedString, font_size: Pixels, runs: &[TextRun], force_width: Option<Pixels>) -> ShapedLine
```
Plus a hash-keyed variant for cache-friendly repeated shaping without materializing a `SharedString` (`shape_line_by_hash`, 458+). `layout_line`/`layout_line_by_hash` (667, 816) are the lower-level cached-glyph-run producers `shape_line` calls into.

### Line clamping / truncation

`GPUI/src/styled.rs`:
```rust
fn text_ellipsis(self) -> Self;         // 137, end
fn text_ellipsis_start(self) -> Self;   // 145
fn text_ellipsis_middle(self) -> Self;  // 150
fn truncate(self) -> Self;              // 197-200 == overflow_hidden().whitespace_nowrap().text_ellipsis()
fn line_clamp(mut self, lines: usize) -> Self; // 205-208, sets text_style().line_clamp = Some(lines)
```

### Rem size / font-scaling accessibility

`GPUI/src/window.rs`:
```rust
pub fn rem_size(&self) -> Pixels;                                  // 2596-2600 (honors an override stack, see with_rem_size)
pub fn set_rem_size(&mut self, rem_size: impl Into<Pixels>);        // 2605
pub fn with_rem_size<F, R>(&mut self, rem_size: Option<impl Into<Pixels>>, f: F) -> R; // 2642, scoped override (used by deferred/anchored redraws to preserve the rem size that was active when they were queued)
```
Default rem size is `px(16.)` (window.rs:1836). Driving a global "text size" accessibility slider is: `window.set_rem_size(px(16.0 * user_scale))` — every `rems(n)` unit used throughout your styles (and `FontMetrics`-derived line-height/ascent calcs) scales from that single call.

---

## 5. Lists & virtualization

### `uniform_list` (fixed row height — the fast path)

`GPUI/src/elements/uniform_list.rs:22-29`:
```rust
pub fn uniform_list<R: IntoElement>(
    id: impl Into<ElementId>,
    item_count: usize,
    f: impl 'static + Fn(Range<usize>, &mut Window, &mut App) -> Vec<R>,
) -> UniformList
```
Only the visible index range is ever rendered (module doc, line 1-5: *"only renders the visible subset of items"*; it measures just the first item and lays the rest out on a line, skipping full Taffy layout per row). Builder methods: `.with_width_from_item(Option<usize>)` (622), `.with_sizing_behavior(ListSizingBehavior)` (628), `.with_horizontal_sizing_behavior(...)` (636), `.with_decoration(impl UniformListDecoration)` (653, e.g. striping/dividers), `.track_scroll(&UniformListScrollHandle)` (683), `.y_flipped(bool)` (690, chat-style bottom-anchored lists).

`UniformListScrollHandle` (80-...): `::new()`, `.scroll_to_item(ix, ScrollStrategy)` (150), `.scroll_to_item_strict(...)` (163), `..._with_offset` variants (182/201), `.logical_scroll_top_index()` (222), `.is_scrolled_to_end()` (241), `.scroll_to_bottom()` (252).

### `list()` / `ListState` (variable row height)

`GPUI/src/elements/list.rs:24-34` (module doc, 1-8: *"use `uniform_list` if all rows are the same height"*):
```rust
pub fn list(state: ListState, render_item: impl FnMut(usize, &mut Window, &mut App) -> AnyElement + 'static) -> List
```
`ListState` (54, `Rc<RefCell<StateInner>>`, so cheap to clone and stash on your view):
```rust
pub fn new(item_count: usize, alignment: ListAlignment, overdraw: Pixels) -> Self;   // 314 — overdraw = extra px measured above/below viewport to avoid pop-in
pub fn measure_all(self) -> Self;                       // 336 — measure every row up front (correct scrollbar size, costs an up-front pass)
pub fn with_uniform_item_height(self, height: Pixels) -> Self; // 347 — cheap scrollbar-size hint that converges to real heights as rows render
pub fn reset(&self, element_count: usize);              // 355
pub fn splice(&self, old_range: Range<usize>, count: usize); // 503 — insert/remove/replace a range of items
pub fn splice_focusable(&self, old_range, count, focus_handles: impl IntoIterator<Item=Option<FocusHandle>>); // 511
pub fn set_scroll_handler(&self, handler: impl FnMut(&ListScrollEvent, &mut Window, &mut App) + 'static); // 552
pub fn scroll_to(&self, scroll_top: ListOffset);         // 647
pub fn scroll_to_reveal_item(&self, ix: usize);          // 664
pub fn bounds_for_item(&self, ix: usize) -> Option<Bounds<Pixels>>; // 698
```
Rows are stored in a `sum_tree::SumTree<ListItem>` (module uses `gpui_ce_sum_tree`) keyed by cumulative height/count, so `splice`/`scroll_to_reveal_item`/`bounds_for_item` are O(log n) rather than O(n). Doc note (list.rs:1-6): row heights outside the scrolled area must not silently change — call `splice`/`reset` if they do.

### Animating row insertion

There is no dedicated "animate insertion" API for either `list()` or `uniform_list()`. The idiomatic approach (following the same per-element persistent-state mechanism `AnimationElement` itself uses, §1) is to wrap the content each `render_item`/`f` callback returns in `.with_animation(ElementId::from(row_id), Animation::new(dur), |el, t| ...)`, keyed by a stable id derived from the row's own identity (not its index, since indices shift on insertion). Because `with_element_state`/`with_animation`'s per-element state is created fresh the first time a given `GlobalElementId` is seen, a freshly-inserted row naturally starts its enter animation on the frame it first appears; a row that was already visible and only shifted position keeps its animation state (no id collision) as long as you key by stable identity rather than index.

---

## 6. Overlays

### `deferred()`

`GPUI/src/elements/deferred.rs` (full file, 96 lines of impl + tests):
```rust
pub fn deferred(child: impl IntoElement) -> Deferred;
impl Deferred { pub fn with_priority(self, priority: usize) -> Self; }  // higher = drawn on top; also `.priority(...)` alias (89-96)
```
Layout of the child happens inline (so it still participates in flex sizing where it's declared), but painting is pushed to `window.defer_draw(child, absolute_offset, priority, content_mask)` (window.rs:3868-3890+) during `prepaint`, and actually painted **after all ordinarily-drawn ancestors**, ordered by `priority` (deferred.rs:54-66). This is the primitive both popovers and tooltips are built from.

### `anchored()`

`GPUI/src/elements/anchored.rs` (full file, 240 lines of impl):
```rust
pub fn anchored() -> Anchored;
impl Anchored {
    pub fn anchor(self, anchor: Anchor) -> Self;                 // 40, which corner is pinned (Anchor: geometry.rs:2180)
    pub fn position(self, anchor: Point<Pixels>) -> Self;        // 47, in window (or parent-local, see position_mode) coords
    pub fn offset(self, offset: Point<Pixels>) -> Self;          // 54
    pub fn position_mode(self, mode: AnchoredPositionMode) -> Self; // 62, Window | Local
    pub fn snap_to_window(self) -> Self;                         // 68
    pub fn snap_to_window_with_margin(self, edges: impl Into<Edges<Pixels>>) -> Self; // 74
}
pub enum AnchoredFitMode { SnapToWindow, SnapToWindowWithMargin(Edges<Pixels>), SwitchAnchor /* default */ }
```
During `prepaint` (anchored.rs:122-215) it measures its children, then if `fit_mode == SwitchAnchor` (the default) it flips the anchor to the opposite horizontal/vertical edge when the desired bounds would overflow the *window* viewport (98-180); afterward it always clamps/snaps into the viewport regardless of mode (189-205) — so a popover placed near a window edge automatically flips or slides to stay fully on-screen. **`anchored()` is almost always wrapped in `deferred()`** (every test and the doc comment pattern does `deferred(anchored().position(...).child(...)).with_priority(n)`) so it paints above sibling content instead of being clipped/occluded by normal z-order.

### Z-order / priority / occlusion

- `Deferred::with_priority(usize)` — higher draws later/on top among deferred draws (deferred.rs:22-28); nested deferred popovers (a menu-inside-a-menu) each get their own priority (test at deferred.rs:105-132 does exactly this, priorities 1 and 2).
- `InteractiveElement::occlude()` (`elements/div.rs:1243-1246`) → `HitboxBehavior::BlockMouse`: makes every hitbox *behind* this element's hitbox report `is_hovered() == false`, for building modal scrims.
- `InteractiveElement::block_mouse_except_scroll()` (div.rs:1259-1262) → `HitboxBehavior::BlockMouseExceptScroll`: blocks clicks/hover but still lets scroll wheel reach what's behind (e.g. a non-blocking overlay/watermark).

### Popover positioning near window edges

Handled automatically by `Anchored`'s `SwitchAnchor`/`SnapToWindow(WithMargin)` fit modes described above — see the crate's own regression tests `test_anchored_position_without_scroll` / `test_anchored_snaps_to_window` (anchored.rs:330-397) for exact expected behavior at a window edge.

---

## 7. Input & focus

### `FocusHandle` / focus

`GPUI/src/window.rs:468-...`:
```rust
pub struct FocusHandle { /* .. */ }
impl FocusHandle {
    pub fn focus(&self, window: &mut Window, cx: &mut App);   // 542
}
impl Window {
    pub fn focus_handle(&self) -> FocusHandle;                 // app.rs:2701-2706, allocate a fresh handle
    pub fn focus(&mut self, handle: &FocusHandle, cx: &mut App); // window.rs:2033
}
```
`InteractiveElement::track_focus(self, focus_handle: &FocusHandle) -> Self` (`elements/div.rs:777-781`) attaches a `FocusHandle` to a specific element so it becomes focusable/keyboard-navigable to that node.

### `key_context` / actions / `KeyBinding`

`InteractiveElement::key_context<C: TryInto<KeyContext>>(self, key_context: C) -> Self` (div.rs:819-...) — sets the keymap context string(s) used to resolve which action a keystroke maps to (`Window::set_key_context`, window.rs:4802-4809, used during paint). `Interactivity::on_action<A: Action>(&mut self, listener: impl Fn(&A, &mut Window, &mut App) + 'static)` (div.rs:457) is the imperative form; `Window::on_action` (window.rs:6005) / `on_action_when` (6025) are the window-global forms.

```rust
actions!(my_app, [ToggleSidebar, Confirm]);   // action.rs:24-37 macro — generates unit structs `my_app::ToggleSidebar` etc.
```

`KeyBinding::new` (`GPUI/src/keymap/binding.rs:33-45`):
```rust
pub fn new<A: Action>(keystrokes: &str, action: A, context: Option<&str>) -> Self
```
```rust
KeyBinding::new("cmd-b", ToggleSidebar, Some("Workspace"))
```
registered into a `KeyMap` (`GPUI/src/keymap.rs:53`, `KeyMap::new(bindings: Vec<KeyBinding>)`).

### `track_focus` / tab order

`GPUI/src/elements/div.rs`:
```rust
fn track_focus(self, focus_handle: &FocusHandle) -> Self;   // 777
fn tab_stop(self, tab_stop: bool) -> Self;                  // 789 — stays in tab order bookkeeping but unreachable via Tab when false
fn tab_index(self, index: isize) -> Self;                   // 798 — also sets focusable=true, tab_stop=true
fn tab_group(self) -> Self;                                 // 809 — resets tab-index numbering to 0 for children of this subtree
```
Backing data structure: `TabStopMap` (`GPUI/src/tab_stop.rs`, a `sum_tree::SumTree<TabStopNode>` ordered by a path of nested `TabIndex`es) — supports arbitrary nested groups with locally-renumbered tab order, not just a flat integer list.

### Hover

```rust
fn hover(self, f: impl FnOnce(StyleRefinement) -> StyleRefinement) -> Self;               // div.rs:831 — declarative hover style
pub fn on_hover(&mut self, listener: impl Fn(&bool, &mut Window, &mut App) + 'static);      // div.rs:676 (imperative; true=start, false=end)
```

### Mouse drag / move / scroll

```rust
pub fn on_drag<T: 'static, W: 'static + Render>(&mut self, value: T,
    constructor: impl Fn(&T, Point<Pixels>, &mut Window, &mut App) -> Entity<W> + 'static); // div.rs:621-641 — builds the drag-ghost view on drag start
pub fn on_drag_move<T: 'static>(&mut self, listener: ...);   // div.rs:360 (fluent) / 1043 (imperative)
pub fn external_drag_payload<T>(&mut self, resolver: impl Fn(&T, &mut Window, &mut App) -> Option<ExternalDragPayload> + 'static); // div.rs:647-669, offer a payload if the drag leaves the window
pub fn on_mouse_move(&mut self, listener: impl Fn(&MouseMoveEvent, &mut Window, &mut App) + 'static); // div.rs:325 (fluent) / 1016 (imperative)
pub fn on_scroll_wheel(&mut self, listener: impl Fn(&ScrollWheelEvent, &mut Window, &mut App) + 'static); // div.rs:390-401 (fluent) / 1055 (imperative)
```
`on_scroll_wheel`'s imperative form (div.rs:390-399) only fires the listener when `hitbox.should_handle_scroll(window)` is true during the bubble phase — this is the "outer scrollable container" distinction called out in `Hitbox::is_hovered`'s doc comment (window.rs:786-791; see §3).

---

## 8. Async

### Spawning

```rust
// App (main/foreground thread) — app.rs:1960-1973
pub fn spawn<AsyncFn, R>(&self, f: AsyncFn) -> Task<R>
    where AsyncFn: AsyncFnOnce(&mut AsyncApp) -> R + 'static, R: 'static;
// Window convenience — window.rs:2375-2385, wraps App::spawn with an AsyncWindowContext
pub fn spawn<AsyncFn, R>(&self, cx: &App, f: AsyncFn) -> Task<R>
    where AsyncFn: AsyncFnOnce(&mut AsyncWindowContext) -> R + 'static, R: 'static;
// Context<T> (entity-scoped) — app/context.rs:236-...
pub fn spawn<AsyncFn, R>(&self, f: AsyncFn) -> Task<R>;   // gives you a weak handle to `self`'s entity across await points
```
**`App::spawn`/`Window::spawn` run on the *foreground* executor (main thread)** — safe to touch window/entity state between `.await`s, but don't block it with CPU work.

```rust
// AppContext trait — gpui.rs:236 (impl'd by App/AsyncApp/Context<T>/TestAppContext/HeadlessAppContext/...)
fn background_spawn<R: Send + 'static>(&self, future: impl Future<Output = R> + Send + 'static) -> Task<R>;
// raw executor access — app.rs:1945 / executor.rs:89
pub fn background_executor(&self) -> &BackgroundExecutor;
pub fn spawn<R>(&self, future: impl Future<Output = R> + Send + 'static) -> Task<R>;  // BackgroundExecutor::spawn, SCHED/src/executor.rs:72/219
```
Use `cx.background_spawn(...)`/`background_executor().spawn(...)` for CPU-bound or blocking-adjacent work (parsing, layout of huge data, etc.) so the main thread stays free to paint.

### `Task` semantics

`SCHED/src/executor.rs:318-370`:
```rust
#[must_use]
pub struct Task<T>(TaskState<T>);   // Ready | Spawned(async_task::Task) | Downcast
```
Doc comment (317-323): *"It implements Future... If you drop a task it will be cancelled immediately. Calling `Task::detach` allows the task to continue running, but with no way to return a value."* — i.e. **cancel-on-drop is the default; you must explicitly `.detach()` a fire-and-forget task** or hold onto the `Task` (e.g. as a struct field) to keep it alive. `TaskExt::detach_and_log_err(self, cx: &App)` (`GPUI/src/executor.rs:30-53`) is the common pattern for detaching a `Task<Result<T,E>>` while still surfacing errors to the log.

### Timers

```rust
pub fn timer(&self, duration: Duration) -> Task<()>;   // GPUI/src/executor.rs:158-167 (BackgroundExecutor); zero-duration short-circuits to Task::ready(())
```

### `WeakEntity::update` from async

`GPUI/src/app/entity_map.rs:777-805`:
```rust
pub fn update<C: AppContext, R>(&self, cx: &mut C, update: impl FnOnce(&mut T, &mut Context<T>) -> R) -> Result<R>;      // errors if entity released
pub fn update_in<C: AppContext, R>(&self, cx: &mut C, update: impl FnOnce(&mut T, &mut Window, &mut Context<T>) -> R) -> Result<R>; // + errors if no current window
```
Standard async pattern: capture `entity.downgrade()` before `.await`ing, then `weak.update(&mut cx, |state, cx| { ...; cx.notify(); })` after resuming, handling the `Result` (entity may have been dropped while you were awaiting).

### `cx.notify()` / `observe` / `subscribe` / `EventEmitter`

```rust
// Context<T> — app/context.rs:228-231
pub fn notify(&mut self);              // -> App::notify(self.entity_state.entity_id)
// App — app.rs:2709 (see §1 cost model for exactly what this dirties)
pub fn notify(&mut self, entity_id: EntityId);
// App — observe another entity's notify() calls
pub fn observe<W: 'static>(&mut self, entity: &Entity<W>, on_notify: impl FnMut(Entity<W>, &mut App) + 'static) -> Subscription; // app.rs:1112-1124
// App — subscribe to an EventEmitter's emitted events
pub fn subscribe<T: 'static + EventEmitter<Event>, Event: 'static>(&mut self, entity: &Entity<T>,
    on_event: impl FnMut(Entity<T>, &Event, &mut App) + 'static) -> Subscription;  // app.rs:1201-1214
pub fn emit<EntityType: EventEmitter<EventType>, EventType: 'static>(&mut self, entity: &Entity<EntityType>, event: EventType); // app.rs:1095-1109
```
`observe`/`subscribe` return a `Subscription` you must hold (drop = auto-unsubscribe, standard GPUI RAII pattern). `EventEmitter<Event>` is a marker trait your entity implements per event type it can emit (imported at app.rs:50; not re-read in full here — standard `impl EventEmitter<MyEvent> for MyState {}`).

### Avoiding blocking the UI thread / re-render invalidation rules

- Never do blocking I/O or heavy CPU work inside `Render::render`, an action handler, or an `App::spawn`/`Window::spawn` future's synchronous portions — route it through `background_spawn`.
- What makes a view re-render: only `cx.notify()` on that view's own entity (directly, or transitively via `with_animation`/`request_animation_frame`, or via `Window::refresh()` which forces everyone) — see the full mechanism in §1's cost-model section (`WindowInvalidator::invalidate_view`, `mark_view_dirty`, `ViewElement`'s cache-key check).
- `AnyView::cached(style)` / `Entity<T>::cached(style)` (view.rs:39, 232) — see §11.

---

## 9. Window

### Custom titlebar on macOS

`GPUI/src/platform.rs:2064-2076`:
```rust
pub struct TitlebarOptions {
    pub title: Option<SharedString>,
    pub appears_transparent: bool,             // hide the system titlebar to draw your own
    pub traffic_light_position: Option<Point<Pixels>>, // macOS traffic-light button offset
}
```
Default `WindowOptions::titlebar` (platform.rs:2041-2045) is `Some(TitlebarOptions { title: None, appears_transparent: false, traffic_light_position: None })`. At runtime: `Window::set_traffic_light_position(&self, position: Point<Pixels>)` (window.rs:2541-2543, thin wrapper over `platform_window.set_traffic_light_position`, default no-op outside macOS/Windows — `platform.rs:914`).

### Window resize observation

```rust
// Context<T> — app/context.rs:426-441
pub fn observe_window_bounds(&self, window: &mut Window,
    callback: impl FnMut(&mut T, &mut Window, &mut Context<T>) + 'static) -> Subscription;
```
Fired from `Window::bounds_changed` (window.rs:2408-2424, doc: *"Normally called automatically by the platform's resize callback, but exposed publicly for test infrastructure"*), which also refreshes `scale_factor`/`viewport_size`/`display_id`/`mouse_position` and calls `self.refresh()` — so a resize is always a full-window redraw, not a targeted `notify`.

### `window.viewport_size()`

`window.rs:2470` (right after `appearance()`): `pub fn viewport_size(&self) -> Size<Pixels>` — the drawable area size, distinct from `Window::bounds()` (2427-2429, global/multi-display bounds from the platform window).

### Appearance (dark/light) observation

```rust
pub fn observe_window_appearance(&self, callback: impl FnMut(&mut Window, &mut App) + 'static) -> Subscription; // window.rs:1952-1965
pub fn appearance(&self) -> WindowAppearance; // window.rs:2465-2467
```
`WindowAppearance` (platform.rs:2109-2135): `Light | VibrantLight | Dark | VibrantDark`, doc'd as mapping directly to macOS `NSAppearance` names. Fired from `Window::appearance_changed` (window.rs:2450-2456).

### Background blur / transparency

```rust
pub fn set_background_appearance(&self, background_appearance: WindowBackgroundAppearance); // window.rs:2551
```
`WindowBackgroundAppearance` (platform.rs:2139-2160): `Opaque` (default) | `Transparent` | `Blurred` ("contents behind the window are blurred... not always supported") | `MicaBackdrop`/`MicaAltBackdrop` (Windows 11 only). Set at window-creation time via `WindowOptions.window_background` (default `Opaque`, platform.rs:2054), or later via the setter above.

---

## 10. Headless / testing — **and the animation-clock question**

### `TestAppContext` (`GPUI/src/app/test_context.rs:21-...`)

Key methods: `executor() -> BackgroundExecutor` (197), `open_window`/`add_window` (220-266), `update`/`read` (207-219), `run_until_parked()` (476-480), `simulate_keystrokes`/`simulate_input`/`dispatch_action` (481-523), `simulate_mouse_move/down/up/click` (via `VisualTestContext`, 805-867), `debug_bounds(selector) -> Option<Bounds<Pixels>>` (888), `draw<E>(...)` (893).

### `HeadlessAppContext` (`GPUI/src/app/headless_app_context.rs:38-...`)

```rust
pub fn new(platform_text_system: Arc<dyn PlatformTextSystem>) -> Self;         // 51
pub fn open_window<V: Render + 'static>(&mut self, ...) -> WindowHandle<V>;    // 104
pub fn run_until_parked(&self);                                                // 129
pub fn advance_clock(&self, duration: Duration);                               // 134
pub fn capture_screenshot(&mut self, window: AnyWindowHandle) -> Result<RgbaImage>; // 168
```

### `render_to_image` / screenshotting

`GPUI/src/window.rs:2431-2438`:
```rust
#[cfg(any(test, feature = "test-support"))]
pub fn render_to_image(&self) -> anyhow::Result<image::RgbaImage>
```
Doc: *"Renders the current frame's scene to a texture...does not present the frame to screen"*. `HeadlessAppContext::capture_screenshot` (above) is the higher-level entry point that presumably calls this per-window.

### `advance_clock` / `simulate_next_frame`

```rust
#[cfg(any(test, feature = "test-support"))]
pub fn advance_clock(&self, duration: Duration);          // GPUI/src/executor.rs:176-179, BackgroundExecutor -> TestDispatcher::advance_clock
#[cfg(any(test, feature = "test-support"))]
pub fn simulate_next_frame(&mut self, cx: &mut App) -> usize; // GPUI/src/window.rs:2361-2369 — drains Window::on_next_frame callbacks, returns how many ran
```

### **Which clock does the animation system read? Can the test dispatcher control it?**

This is the load-bearing finding, cross-checked against both source trees:

1. `SCHED/src/clock.rs:1-55` — `pub use web_time::Instant;` (the *only* `Instant` type in this ecosystem is a re-export of `web_time`'s, i.e. real OS time on native targets). `TestClock` (12-55) implements `trait Clock { fn now(&self) -> Instant; fn utc_now(&self) -> DateTime<Utc>; }` and its `now()`/`advance(duration)` (34-38) are entirely independent bookkeeping — advancing a `TestClock` does **not** change what `web_time::Instant::now()` returns.
2. `BackgroundExecutor::now()` (`GPUI/src/executor.rs:150-156`): `self.inner.scheduler().clock().now()` — doc comment: *"Calling this instead of `std::time::Instant::now` allows the use of fake timers in tests."* This method (and `.timer(duration)`, 162-167) **is** routed through the `Clock` trait, so in a `TestAppContext`/`HeadlessAppContext` it reads the fake `TestClock`, and `executor.advance_clock(duration)` (177-179, delegates to `TestDispatcher::advance_clock`, `SCHED/src/test_scheduler.rs:377-409`) moves it forward and resolves pending `timer()` futures (`advance_clock_to_next_timer`, test_scheduler.rs:368).
3. **`AnimationElement` (`with_animation`, easing-based) does *not* use `BackgroundExecutor::now()`.** `GPUI/src/elements/animation.rs:1,132-134,158`: `use scheduler::Instant;` ... `struct AnimationState { start: Instant, .. }` ... `start: Instant::now()` — this calls the raw `web_time::Instant::now()` directly, bypassing the `Clock` trait entirely. `state.start.elapsed()` (172-173) therefore measures **real wall-clock time**, in both 0.2.2 and `CE3` (the diff between the two versions leaves this line untouched).

**Consequence: `cx.executor().advance_clock(duration)` / `HeadlessAppContext::advance_clock` has *no effect whatsoever* on `with_animation`/`Animation`'s progress.** A test that wants to observe an easing-based animation's `delta` reach a specific value cannot fast-forward it — it can only call `window.simulate_next_frame(cx)` repeatedly and either tolerate near-zero deltas (since real time barely advances during a fast test — this is exactly what the crate's own test `test_repeating_animation_schedules_animation_frames`, animation.rs:364-374, does: it only asserts the *count* of rendered frames increases, never a specific `delta` value) or use `std::thread::sleep` in the test (undesirable/flaky), or refactor the animation to source time from the executor clock instead.

4. **`CE3`'s new `SpringAnimationElement` fixes exactly this** for the spring path only: its `request_layout` reads `let now = cx.background_executor().now();` (comment: *"Use the executor clock so spring progression is deterministic in tests and remains consistent with scheduled animation work"*) and computes `elapsed = now.duration_since(state.updated_at)`. So in `CE3`, `with_spring(...)` animations **are** fully controllable/deterministic under `advance_clock` in tests, while plain `with_animation(...)` easing animations remain wall-clock-real in both versions. If porting/vendoring spring support from `CE3`, this is the one piece of plumbing (`cx.background_executor().now()` instead of `scheduler::Instant::now()`) that makes it testable — worth deliberately carrying over even for a from-scratch spring implementation.

---

## 11. Performance notes

- **View caching**: `AnyView::cached(style: StyleRefinement) -> ViewElement<AnyView>` (`GPUI/src/view.rs:39-41`) and `Entity<T>::cached(style) -> ViewElement<Entity<T>>` (223-234) recycle a view's entire previous prepaint/paint output when `bounds`/`content_mask`/`text_style` are unchanged and the entity hasn't been `cx.notify()`'d and `!window.refreshing` (`ViewElement::prepaint`, view.rs:386-401, see §1). **Tradeoff**: a cached view is laid out purely from the `style: StyleRefinement` you pass (`root_style.refine(style)`, view.rs:327-329) — it is *not* measured from its actual contents, so you must give it a definite size (doc comment, view.rs:224-230: *"Caching requires a definite size... is not measured from its contents"*). Use this for expensive, rarely-changing subtrees sitting next to hot-animating ones (exactly the nested-deferred-popover regression test at `elements/deferred.rs:134-146` uses `.cached(StyleRefinement::default().size_full())` on a whole dock-panel view).
- **`with_animation`'s cost is local, not global** — see the full mechanism in §1 (`mark_view_dirty` walks only ancestors, stops at the first already-dirty one; unrelated cached siblings replay their previous frame's primitives via `window.reuse_prepaint`/`reuse_paint`, view.rs:394/475).
- **`Window::refresh()`** (window.rs:2013-2019) forces a *full* redraw and explicitly bypasses the `ViewElementState` cache check (`!window.refreshing` in the cache condition) — avoid calling it from a per-frame animation path; prefer `cx.notify(specific_entity)`.
- **Asset caches are content-addressed and automatic**, not something you manage: `window.paint_svg`/`paint_image` both go through `self.sprite_atlas.get_or_insert_with(params, ...)` (window.rs:4467-4474, 4527-4535) keyed by `RenderSvgParams{path,size}`/`RenderImageParams{image_id,frame_index}` — repaint of the same asset at the same size is a cache hit, no re-rasterization/re-upload. On macOS the atlas is `MetalAtlas` (`MACOS/src/metal_atlas.rs:13-40`, `get_or_insert_with`/`allocate` packing multiple `MetalAtlasTexture`s) — this is the "glyph atlas" for text too (glyphs are just another sprite kind through the same atlas machinery via `layout_line`/`shape_line`'s cache, `text_system.rs`).
- **`SharedString`** — a cheap-to-clone reference-counted string type (its own crate, `gpui_ce_shared_string`; used pervasively as `Font.family`, element ids, `StyledText`'s text, asset paths) so passing text/paths into style/element builders every frame is not a re-allocation on each render, unlike `String`.
- **`FontFeatures`** and `Font` are cheap to clone too (`FontFeatures(Arc<Vec<..>>)`, text_system/font_features.rs:8) — safe to construct fresh `Font { .. }` values per-render without worrying about deep copies.
- **Style refinement cost**: every element's final `Style` is produced by `Style::default().refine(&StyleRefinement)` (seen in `Canvas::request_layout`, canvas.rs:56-57, and `ViewElement`'s cached-style path, view.rs:327-328) via the `refineable` crate's derive — a field-by-field `Option`-overlay merge, O(number of style fields) per element per frame; not something to optimize away, but a reason `.cached()` (skipping render+refine entirely) is worth it for genuinely static-looking subtrees.
- **`Task<T>` is `#[must_use]` and cancel-on-drop** (`SCHED/src/executor.rs:318-323`) — a background computation you forget to `.detach()`/store is silently cancelled the moment the `Task` value is dropped; this is a correctness footgun more than a perf one, but a common source of "my background work never seems to finish."

---

## Notes on what I could not fully verify

- **Straight-edge antialiasing quality for tessellated `PathBuilder` polygons** (§3): confirmed analytic AA exists for curve segments (Loop–Blinn technique in `path_rasterization_fragment`, `MACOS/src/shaders.metal:772-812`), but did not trace whether the subsequent `path_sprite_fragment` composite/sampling step (shaders.metal:842+) softens dead-straight polygon edges (e.g. an octagon's flat sides) via bilinear sampling of the offscreen-rasterized tile, or whether those edges are hard/aliased. Recommend a direct pixel-level visual check (`render_to_image` + zoom) before relying on crisp AA for straight polygon edges at low pixel radii.
- **Variable-font axis interpolation on macOS** (§4): `FontWeight`'s raw `f32` reaches CoreText via `font-kit`'s descriptor matching (`MACOS/src/text_system.rs:722-724`), but whether CoreText actually interpolates a `wght` variation axis continuously for an arbitrary weight value (vs. snapping to the nearest static/named instance the font ships) is a CoreText/font-kit behavior I did not independently verify from source alone.
- **`observe_window_bounds`** exists exactly under that name (`GPUI/src/app/context.rs:426`) — no gap here, listed as a note only because the task's phrasing suggested it might not exist.
- **No generic per-element `.rotate()`/`.scale()`/`.translate()` style exists on `div()`/arbitrary elements** in either 0.2.2 or `CE3` — transforms are SVG-only (`Svg::with_transformation`). If the design needs to rotate/scale arbitrary composed content (not just an icon), you'd need to either (a) render that subtree into an image/texture and paint it as an `Svg`-like monochrome sprite (lossy, monochrome-only), or (b) wait on/contribute a generic transform primitive — this looks like a genuine capability gap in the vendor-target version, not something I simply failed to find (checked `styled.rs`, `style.rs`, `div.rs` fluent methods exhaustively; only `blur`/`backdrop_blur`/`opacity` exist as generic per-element visual-effect styles).
- **`gpui_ce_wgpu-0.1.0` / `gpui_ce_linux-0.1.0` / `gpui_ce_windows-0.1.0`** exist in the local registry cache (dated 2026-09-23, newer than the other pinned crates) and `CE3` has a parallel `gpui_wgpu`/`gpui_linux`/`gpui_windows` cross-platform renderer stack (including a wgpu headless render target, `gpui_wgpu/src/wgpu_renderer/headless.rs`) — this looks like an in-progress cross-platform renderer effort sitting alongside the Metal-only macOS path this document focuses on (per the task's named sibling crates). Not explored in depth; flagged in case it's relevant to future portability decisions.
