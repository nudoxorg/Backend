# R2 — gpui_ce_components inventory for a custom-styled docs reader

Scope: `vendor/gpui_ce_components` (crate `gpui_ce_components` 0.2.0, lib name `gpui_component`),
patched in via root `Cargo.toml` `[patch.crates-io] gpui_ce_components = { path = "vendor/gpui_ce_components" }`
(/Users/mileswirht/Downloads/backend/Cargo.toml:108). Compared against the pristine registry copy at
`~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/gpui_ce_components-0.2.0/`.

Decision context: a desktop docs reader with heavy custom visuals — chamfered "cut" cards, two-tone
bevel borders as the focus/state channel, custom fonts, lots of animation. Verdicts below are judged
against that bar, not against generic UI-kit usefulness.

## 0. Load-bearing fact: most "component" behavior lives one layer down, in `gpui_base`

`gpui_ce_components`'s own `lib.rs` re-exports a large amount of core behavior straight from an
**unvendored, unpatched** crate whose lib name is `gpui_base` (package `gpui_ce_components_base`
0.2.0, pulled from crates.io — see Cargo.lock:3996-4030, source at
`~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/gpui_ce_components_base-0.2.0/`). Only
`gpui_ce_components` itself is in `[patch.crates-io]`; `gpui_base` is not, so it cannot be locally
edited the way the vendored crate can without also vendoring/patching it.

Things that live in `gpui_base`, not in the vendored tree:
- The whole text-editing engine: `InputState`, `EditorState`, `TextareaState`, `InputBaseState`,
  IME (`EntityInputHandler::replace_and_mark_text_in_range`, confirmed exercised by CJK composition
  tests at `gpui_ce_components_base-0.2.0/src/input/base/state.rs:2688-2871,3353,4187-4675`).
- `ResizablePanel`/`ResizablePanelGroup`/`ResizableState`/`h_resizable`/`v_resizable` — re-exported
  from `gpui_base` at vendor/gpui_ce_components/src/lib.rs:64-108 (the vendored `pub mod resizable`
  at lib.rs:64-69 is just a re-export shim; there is no resizable-panel *implementation* in the
  vendored tree at all).
- `TextSelection`/`TextSelectionLayer`/`TextSelectionScopeId` (cross-element text selection used by
  Root and by `text::` — vendor/gpui_ce_components/src/text/mod.rs:17-23, a local-diff addition, see
  §1).
- Focus trap (`gpui_base::active_focus_trap`, used by root.rs:488,523), window hit-test forwarder,
  scrollbar painting/motion, resize-handle painting, `TooltipOverlay`.
- A second `dock/` implementation lives under `gpui_ce_components_base-0.2.0/src/dock/` (with its own
  `layout/` and `fixtures/`) alongside the vendored `gpui_ce_components/src/dock/` — the vendored dock
  is the presentation/API layer over `gpui_base`'s dock layout engine.

Practical consequence: "wrap" and "restyle" verdicts below are cheap only for the parts that live in
the vendored crate. Anything whose actual painting/behavior lives in `gpui_base` (scrollbar, resize
handles, IME, focus trap, IME text-content-type bridge) can only be restyled through the knobs
`gpui_ce_components` exposes (mostly via `Theme`/`ThemeColor` projected through
`Theme::base_theme()`, vendor/gpui_ce_components/src/theme/mod.rs:258-293) — there is no local source
to hand-edit for those without vendoring `gpui_ce_components_base` too.

### 0.1. The shared `Styled`/override contract (`styled.rs`, `component_traits.rs`, `sizing.rs`)

`vendor/gpui_ce_components/src/component_traits.rs` (2 lines total) is just a re-export shim:
`pub use gpui_base::component_traits::Collapsible;` and `pub use gpui_base::{Disableable, Selectable};`
(component_traits.rs:1-2) — the actual `Disableable`/`Selectable`/`Collapsible` trait definitions live
in `gpui_base`, not in this crate. `sizing.rs` similarly defines the shared `Sizable`/`Size`/
`StyleSized` trait family every sized component (`Button`, `Input`, `Spinner`, `Command`, `Settings`,
etc.) implements for `.xsmall()/.small()/.large()/.with_size(Size)`-style builder methods.

`styled.rs` is where the crate's own visual vocabulary lives, layered on top of plain GPUI `Styled`:
- `ThemeStyled` (styled.rs:127-208, blanket-implemented for every `T: Styled`) adds three
  theme-aware helpers: `.focus_ring_style(window, cx)`, `.popover_style(cx)`, `.rounded_full_style(cx)`.
- **`focus_ring_style`** (styled.rs:170-208) is the mechanism behind every component's "focused" look
  (`Input`, `Button`, etc.) — it paints the element's own border in `cx.theme().ring`, then (unless
  `cx.theme().focus_ring == false`) adds a **second, absolutely-positioned child `div`** just outside
  the element's existing border, offset by `FOCUS_RING_WIDTH` (3px, styled.rs:10) plus whatever border
  width the element already has, drawn with `with_alpha(cx.theme().ring, FOCUS_RING_OPACITY)` (50%
  opacity, styled.rs:11) and radius = each corner's own radius + `FOCUS_RING_WIDTH`. This is a
  **single-tone** outside ring, not a two-tone bevel — reproducing a two-tone bevel focus/state channel
  means either replacing every call site that invokes `.focus_ring_style()` (or the `Input`/`Button`
  flags that trigger it, e.g. `Input::focus_bordered(false)`, see §6) or accepting this ring as one of
  the two tones and adding the second tone as the element's own border color.
- **`popover_style`** (styled.rs:203-208) is the single shared "floating surface" look — `bg(popover)`,
  `text_color(popover_foreground)`, `popover_shadow(...)` (a hairline ring-as-shadow plus two blurred
  layers modeled on shadcn/ui's popover, styled.rs:44-82, with unit-level commentary on why GPUI's blur
  radius must be halved vs. CSS's), `rounded(theme.radius)` — used by `Popover`, `PopupMenu`, `Select`,
  ComboBox, DatePicker, and the editor's hover popovers per the doc comment at styled.rs:151-153, so
  restyling this one function reskins every floating-panel surface in the library at once.
- `popover_shadow`/`toast_shadow`/`raised_shadow` (styled.rs:44-121) are the three canonical elevation
  presets (popover/toast/raised-inside-a-trough), each shadcn-derived and explained in detail in the
  source comments — worth reusing as reference shadow math even if the surfaces themselves are rebuilt.

### 0.2. GPUI's native primitive for chamfered/cut shapes: `PathBuilder` + `canvas()`

Cross-cutting finding relevant to every "restyle"/"wrap" verdict below: the underlying `gpui-ce` crate
(0.2.2, `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/gpui-ce-0.2.2/`) is **not** limited to
rounded-rectangle boxes for custom visuals. It ships a full Lyon-backed vector path builder,
`PathBuilder` (gpui-ce-0.2.2/src/path_builder.rs:25), with `.move_to()/.line_to()/.curve_to()/
.cubic_bezier_to()/.arc_to()`, and — directly on point for a chamfered "cut" card — `.add_polygon(&[Point<Pixels>], closed: bool)` (path_builder.rs:189) plus `PathBuilder::stroke(width)`/`::fill()` (path_builder.rs:88,96) and `.build() -> Path<Pixels>` (path_builder.rs:244). A built `Path` is painted with `Window::paint_path(path, color: impl Into<Background>)` (gpui-ce-0.2.2/src/window.rs:4191). The declarative escape hatch to reach this from an element tree is `gpui::canvas(prepaint_fn, paint_fn) -> Canvas<T>` (gpui-ce-0.2.2/src/elements/canvas.rs:10-18, doc comment: *"Useful for adding short term custom drawing to a view"* / *"accessing the low level paint API without defining a whole custom element"*), which implements `Element`/`IntoElement`/`Styled` like any other node, so it composes into a normal `.child(canvas(...))` call inside any of the components surveyed below.

**Practical implication**: an actual angle-cut chamfered card outline (not just a rounded corner) is a
first-class, supported thing to build — draw the cut-corner polygon with `PathBuilder::fill()` for the
card body and a second `PathBuilder::stroke()` pass (or two overlaid strokes with different colors/
offsets) for a genuine two-tone bevel border, wrapped in a `canvas()` sized to the card's bounds and
layered behind/around the real interactive content. This should be built once as a small shared
"cut card" primitive and reused as the background/border layer for restyled `Button`, `Input`,
`Dialog`/`Sheet` surfaces, `PopupMenu`/`Command` rows, etc., rather than fighting each component's own
`corner_radii`/`border_widths` styling (which only ever produce rectangles/rounded-rectangles).

## 1. Local modifications: vendored copy vs. pristine registry copy

`diff -rq` was not usable (the Nix-provided `uutils diffutils 0.5.0` on this machine does not
implement `-r`/`--recursive`); recursive comparison was done with `python3 filecmp.dircmp`, then each
differing file was diffed with `diff -u` (non-recursive works fine). Full recursive walk, not a
truncated listing.

Only in pristine (vendoring artifact, not a real diff): `.cargo-ok`.
No files exist only in the vendored copy. Six files differ:

1. **`Cargo.toml`** — adds a `headless = []` feature (vendor/gpui_ce_components/Cargo.toml:42, diffed
   against pristine at the same path). Wired up by apps/desktop/Cargo.toml:40
   (`visual-harness = [..., "gpui_component/headless"]`).
2. **`src/root.rs:103-104`** — guards `gpui_base::install_window_hit_test_forwarder(window)` with
   `not(feature = "headless")` in addition to the pre-existing `not(test)`, with a comment explaining
   headless GPUI windows have no native NSView/window handle. Pure infra change, zero visual impact.
3. **`src/input/content_type.rs:166,170`** — same pattern: gates the macOS native text-content-type
   bridge (`gpui_base::input::set_text_content_type`) behind `not(feature = "headless")` as well as
   `not(test)`. Zero visual impact.
4. **`src/button/button.rs`** — adds accessibility/observability instrumentation, not styling:
   - `accessibility_description` field + `.accessibility_description()` builder, wired to
     `aria_description` (button.rs:193-196, 354-357, 799-802).
   - Four observer hooks — `on_focus_observed`, `on_bounds_observed`, `on_hover_observed`,
     `on_press_observed` (button.rs:216-224, 270-274, 416-462) — that let a screenshot/accessibility
     test harness read the *native* GPUI focus handle, post-prepaint bounds (border-inclusive,
     corrected for the padding-box vs. border-box mismatch of `ElementExt::on_prepaint`, see the
     comment at button.rs:886-895), hover and press state for a concrete rendered button, without
     changing the button's own visual behavior or replacing the app's own `on_hover`/`on_click`.
   - Needs `use gpui_base::ElementExt as _` (button.rs:16) for `.on_prepaint`.
5. **`src/input/input.rs`** — the same observability pattern applied to `Input`: `tab_stop` field +
   `.tab_stop()` builder (input.rs:125,203,352-360 — lets a modal shell keep background controls out
   of Tab order), `aria_description` field/builder (input.rs:130,210,228-231), and
   `on_focus_observed`/`on_hover_observed`/`on_press_observed` (input.rs:132-134,213-215,234-260),
   mirrored through to the rendered `BaseInput` (input.rs:558-565, 621-637). Also a whitespace-only
   test cleanup (input.rs:973-976 in the diff, `assert_eq!` reformatted to one line).
6. **`src/text/mod.rs:17-24`** — re-exports `TextSelection`, `TextSelectionContentKey`,
   `TextSelectionCoverage`, `TextSelectionEndpoint`, `TextSelectionEvent`, `TextSelectionHandle`,
   `TextSelectionLayer`, `TextSelectionProjection`, `TextSelectionRegistration`, `TextSelectionRun`,
   `TextSelectionScopeId`, `TextSelectionSnapshot`, `TextSelectionWindowPoints` from `gpui_base`, so
   that virtualized text participants (e.g. a windowed markdown view) can register visible runs with
   the shared cross-element text-selection system without taking a direct `gpui_base` dependency.

**Overall read**: none of the six local diffs change what anything *looks like*. They are (a) a
`headless` feature so a screenshot/CI harness can boot a window without native OS hooks, and (b) an
accessibility/screenshot observability seam (focus/bounds/hover/press callbacks on Button and Input)
that a semantic-capture harness uses to bind evidence to the concrete rendered control. This is good
news for reuse: the vendored fork has not drifted from upstream in any way that would block pulling
future upstream fixes, and the local additions are all opt-in hooks, not behavior changes.

## 2. What apps/desktop actually uses today

`grep -rn "gpui_component::" apps/desktop/src` (45 hits, 13 files) — i.e. most of the library's
surface (menu/, command's Command palette beyond search_palette's narrow use, kbd, skeleton, spinner,
switch, slider, notification, most of chart/plot/table, native_menu, sidebar, title_bar,
window_border, dock, virtual_list, accordion, avatar, badge, etc.) is **not used anywhere in the app
yet**:

- `apps/desktop/src/harness.rs:20,236,270,1287,1309` — `gpui_component::init`, `Root::new(...).bordered(false)`, `input::AnyInputState` (visual-harness/screenshot tool).
- `apps/desktop/src/host/launch.rs:75,79,89` — same `Root::new(...).bordered(false)` + `init` pattern for the real app window.
- `apps/desktop/src/theme/mod.rs:345,397` — `gpui_component::Theme::global_mut(cx)` / `Theme::sync_base(cx)` — the app's custom-theme installer, see §3.
- `apps/desktop/src/views/mod.rs:31,112,113,202,230` — `WindowExt`, `Root::render_sheet_layer`, `Root::render_dialog_layer` (note: **not** `render_notification_layer` — notifications are not currently wired into the render tree), `input::InputState::new(...).placeholder(...)`, `Placement::Right`.
- `apps/desktop/src/views/shell.rs:18,278,284` — `Placement`, `Root`, `Selectable`, `WindowExt`, `sheet::Sheet`.
- `apps/desktop/src/views/project_admission.rs:16,17` — `dialog::{Cancel, Confirm, Dialog}`, `input::InputState`.
- `apps/desktop/src/views/onboarding.rs:19` and `apps/desktop/src/views/workspace_settings.rs:19` — `progress::Progress`.
- `apps/desktop/src/views/workspace_settings.rs:18,20,21,54` — `Sizable`, `setting::{SelectIndex, SettingGroup, SettingItem, SettingPage}`, `sheet::Sheet`, `Size::Large`.
- `apps/desktop/src/views/package.rs:15`, `apps/desktop/src/views/project_shelf.rs:18` — `Selectable`.
- `apps/desktop/src/ui/search_palette/mod.rs:19-21` — `command::{Command, CommandGroup, CommandItem, CommandState}`, `dialog::Dialog`, `Disableable`, `IndexPath`.
- `apps/desktop/src/ui/components/visual.rs:9-17,427,447,475` — the widest single consumer:
  `button::{Button, ButtonRounded, ButtonVariants}`, `input::{Input, InputState}`, `list::ListItem`,
  `popover::{Popover, PopoverState}`, `scroll::{Scrollable, ScrollableElement}`,
  `setting::{SettingPage, Settings}`, `tooltip::Tooltip`, `tree::{Tree, TreeEntry, TreeState, tree()}`,
  `Disableable`, `FocusableExt`, `Selectable`.
- `apps/desktop/src/runtime/ui_graph.rs:36` — comment only, references `gpui_component::Root` conceptually.

## 3. Cross-cutting: animation usage inside the library

`gpui_base::animation` is re-exported at vendor/gpui_ce_components/src/lib.rs:91
(`pub use gpui_base::animation;` — the actual easing/lerp/transition primitives, e.g.
`cubic_bezier`, `ease_out_cubic`, `ease_in_out_cubic`, `EffectTransition`, `Lerp`, live in `gpui_base`,
not in the vendored tree). Consumers inside `gpui_ce_components`:

| File | Import | Used for |
|---|---|---|
| vendor/gpui_ce_components/src/dialog/dialog.rs:14 | `animation::cubic_bezier` | dialog enter/exit |
| vendor/gpui_ce_components/src/notification.rs:17 | `animation::cubic_bezier` | toast enter/exit |
| vendor/gpui_ce_components/src/popover.rs:11 | `animation::ease_out_cubic` | popover open easing |
| vendor/gpui_ce_components/src/sidebar/mod.rs:16 | `animation::{EffectTransition, ease_in_out_cubic}` | sidebar collapse/expand |
| vendor/gpui_ce_components/src/tab/tab.rs:3 | `animation::{Lerp, ease_in_out_cubic}` | active-tab indicator slide |
| vendor/gpui_ce_components/src/tooltip.rs:15 | `animation::{EffectTransition, ease_out_cubic, ease_in_out_cubic}` | tooltip show/hide |

Also the theme layer times the scrollbar's own fade/slide motion (idle/enter/exit/expand durations,
fade vs. slide-and-fade entrance) at vendor/gpui_ce_components/src/theme/mod.rs:57-81,258-293, which
is the pattern to imitate for hooking a custom "lots of animation" requirement into the same
`gpui_base` easing primitives rather than reinventing interpolation.

---

## 4. `theme/` — global Theme / ActiveTheme, fonts, radius, custom-theme installation

**What it is**: the single global `Theme` struct (vendor/gpui_ce_components/src/theme/mod.rs:85-143)
plus `ActiveTheme`/`ThemeColor`/`ThemeConfig`/`ThemeRegistry`, read everywhere via `cx.theme()`.

**Key API**:
- `ActiveTheme::theme(&self) -> &Theme` (mod.rs:37-46), implemented for `App`; every component calls
  `cx.theme()`.
- `Theme::global(cx)` / `Theme::global_mut(cx)` (mod.rs:170-182) for direct field mutation.
- `Theme::change(mode, window, cx)` (mod.rs:233-256) — switches light/dark, applies the matching
  `ThemeConfig`, and rebuilds the `gpui_base::Theme` projection.
- `Theme::sync_base(cx)` (mod.rs:313-316) — **must be called after any direct field mutation on
  `Theme::global_mut`**, because `gpui_base` (which paints the scrollbar and resize handles) holds its
  own copy of the semantic tokens/colors and does not observe the vendored `Theme`'s fields directly;
  only `Theme::change`/`Theme::sync_base` push a fresh projection down (mod.rs:295-316). This is the
  single most important gotcha for anyone hand-mutating the theme.
- `ThemeRegistry::global(cx).load_themes_from_str(json)` / `ThemeRegistry::watch_dir(dir, cx, on_load)`
  (theme/registry.rs:98-119,151-159) — JSON theme files, hot-reloadable from disk.
- `Theme::apply_config(&mut self, config: &Rc<ThemeConfig>)` (theme/schema.rs:1072-1116) — applies a
  parsed `ThemeConfig` (font family/size, mono font, radius, radius_lg, shadow, per-color overrides,
  highlight/syntax theme) onto the live `Theme`.
- `ThemeColor` (theme/theme_color.rs:63-347) is a flat `Copy` struct of **~140 named `Hsla` fields**
  covering every semantic + per-component color (`background`, `foreground`, `primary*`, `danger*`,
  `button_primary*/button_danger*/...` for every button variant, `list*`, `table*`, `tab*`,
  `sidebar*`, `title_bar*`, `status_bar*`, `scrollbar*`, `switch*`, `slider*`, `chart_1..5`,
  `chart_bullish/bearish`, raw palette `red/green/blue/yellow/magenta/cyan` (+`_light` variants), etc.
- Fonts/radius live on `Theme` itself, not `ThemeColor`: `font_family`, `font_size`, `mono_font_family`,
  `mono_font_size`, `radius`, `radius_lg`, `focus_ring: bool`, `shadow: bool`
  (theme/mod.rs:98-138). `Theme::radius_full()` (mod.rs:383-389) special-cases radius=0 so pills/circles
  square off too — relevant for a chamfered/cut design where nothing should default back to round.

**How to install a fully custom theme** — apps/desktop already does exactly this
(apps/desktop/src/theme/mod.rs:333-397, fn `sync_components`): after `gpui_component::init(cx)`, mutate
every field on `gpui_component::Theme::global_mut(cx)` directly (`component.background = ...`,
`component.primary = ...`, `component.radius = Pixels::ZERO`, `component.focus_ring = true`,
`component.font_family = theme.ui_face()`, `component.mono_font_family = theme.specimen()`, etc.), then
call `gpui_component::Theme::sync_base(cx)` at the end. The in-repo comment
(apps/desktop/src/theme/mod.rs:339-343) states the rationale precisely: "GPUI CE owns focus rings,
popovers, tooltips, inputs, scrollbars, and virtual lists. It must see the same palette as Nudox-owned
elements... The projection intentionally sets only semantic roles; component geometry and interaction
behavior stay with GPUI CE." Desktop already sets `radius = 0`/`radius_lg = 0` to get "hard cuts" in
the same square grammar as app-owned plates — i.e. the "cut card, no rounding" language this app wants
is already the working pattern for `gpui_component`-owned chrome. `focus_ring` is kept `true`, meaning
CE's built-in outside-the-border focus ring (radius/width baked in at
vendor/gpui_ce_components/src/styled.rs:10-11, `FOCUS_RING_WIDTH`/`FOCUS_RING_OPACITY`) is still active
on CE-owned controls — worth an explicit decision on whether the two-tone bevel focus/state channel
replaces or layers with this ring.

**Custom syntax/highlight theme**: `Theme.highlight_theme: Arc<HighlightTheme>`
(theme/mod.rs:93,599) is a first-class part of the theme (see §6 for what it controls) and is also
settable from a `ThemeConfig.highlight` block compatible with Zed theme JSON
(theme/schema.rs:82-86 comment links to Zed's ayu theme as an example).

**Verdict: REUSE AS-IS.** The theme system is a plain data struct + projection function, not a
rendering component — there's nothing to "restyle," and apps/desktop's existing `sync_components`
function is already the intended integration point and already proves the "hard-edged / square"
aesthetic works end-to-end. The only action item is deciding what to do with `focus_ring` given the
bevel-as-state-channel plan, and whether `Theme::sync_base` needs to be called from more places if the
theme is mutated live from other views.

---

## 5. `root.rs` — Root, overlays, notification/dialog/sheet layers

**What it is**: `Root` (vendor/gpui_ce_components/src/root.rs:36-53) is the mandatory top-level
`Render` view for a window — it owns the active `Sheet`, a stack of `Dialog`s, the notification list,
tooltip overlay, and native-menu-fallback overlay, and renders the window border wrapper.

**Key API**:
- `Root::new(view, window, cx)` (root.rs:98-122) — wraps an app's root view.
- `.bordered(bool)` (root.rs:144-147) — apps/desktop always calls `.bordered(false)`
  (host/launch.rs:79, harness.rs:270) since it presumably paints its own window chrome; when `true`,
  Root wraps the content in `window_border()` (root.rs:601-608, see §window_border below).
- `.window_shadow_size(px)` (root.rs:152-155).
- `Root::update`/`try_update`/`read` (root.rs:157-183) — static helpers to reach the singleton Root
  from anywhere via `window.root::<Root>()`.
- Layer renderers, each returns `Option<impl IntoElement>` for the app to place explicitly in its own
  layout: `Root::render_notification_layer` (root.rs:186-213), `Root::render_sheet_layer`
  (root.rs:216-240), `Root::render_dialog_layer` (root.rs:243-289). **apps/desktop only renders the
  sheet and dialog layers** (views/mod.rs:112-113) — the notification layer is never mounted, so
  `Root::push_notification` currently has no visible effect in the app (see §2).
- `open_dialog`/`close_dialog`/`close_all_dialogs`, `open_sheet_at`/`close_sheet`,
  `push_notification`/`remove_notification`/`remove_notification1`/`clear_notifications` (root.rs:291-461).
- `Root` implements `Styled` (root.rs:568-572) over a `StyleRefinement`, merged via `.refine_style(&self.style)` at root.rs:595 — so the root container itself is stylable, though in practice this is
  rarely touched since Root sets `bg(cx.theme().tokens.background)` / `text_color(cx.theme().foreground)`
  right before the merge (root.rs:592-595).

**Styling**: reads `cx.theme().font_size` (root.rs:576, sets window rem size from it — so the whole
app's `rem()` unit follows the theme font size), `cx.theme().font_family`, `cx.theme().tokens.background`,
`cx.theme().foreground` (root.rs:592-594). Everything else (dialog/sheet/notification chrome) is owned
by those components, not Root itself.

**Keyboard/focus**: Root owns global Tab/Shift-Tab handling with focus-trap awareness
(root.rs:19-30,486-554, actions `Tab`/`TabPrev` bound in `init` at root.rs:22-30) — it special-cases
being inside a `gpui_base::active_focus_trap` (e.g. an open dialog) so Tab cycles within the trap
instead of escaping it. Also owns a global `cmd/ctrl-c` Copy action (root.rs:26-29,556-565) that copies
the current cross-element `TextSelection`. Opening a dialog/sheet allocates a fresh focus handle,
remembers the previously-focused handle to restore on close (root.rs:295-317,373-402), and clears any
window text selection so it can't leak under a modal (root.rs:315,331,355,369,400,415).

**Virtualization**: N/A (Root is a single-instance layout shell).

**Verdict: REUSE AS-IS.** This is plumbing (focus-trap-aware Tab cycling, dialog/sheet z-stack,
restore-focus-on-close, cross-window text-selection scoping) that would be substantial and easy to get
subtly wrong to reimplement, and it imposes almost no visual opinion of its own — the one line of
default styling (`bg`/`text_color`/`font_family`) is trivially overridden by setting the theme colors
per §4. Only action item: decide whether to mount `render_notification_layer` if toast notifications
are wanted (currently unused, see §2).

---

## 6. `input/` — text input, IME, editor rendering, code editor, restyling

**What it is**: a family of text-entry surfaces sharing one editing engine —
`Input`/`TextInput` (single/multi-line, input/input.rs), `Textarea` (input/textarea.rs), `Editor`
(source-code editor, input/editor.rs), `NumberInput` (input/number_input.rs), `OtpInput`
(input/otp_input.rs) — all built as thin `RenderOnce` presentation wrappers around state entities
(`InputState`, `TextareaState`, `EditorState`, `OtpState`) whose actual editing logic
(`InputBaseState<M>`, rope buffer, selection, undo, IME) is **not in this crate** — it's
`gpui_base::input` (see §0). `AnyInputState` (input/state.rs:17-54) is a small enum used to treat any
of these uniformly (e.g. for a modal's "restore focus to whichever input was focused" logic).

**Key API**:
- `Input::new(&Entity<InputState>)` (input/input.rs:175-182), or `Input::from_state(...)` for the
  other state kinds. Builders: `.prefix()/.suffix()` (261-271), `.h()/.h_full()` (272-283),
  `.appearance(bool)` (284-289, toggles the themed bg/border/radius entirely),
  `.bordered(bool)`/`.focus_bordered(bool)` (290-301, decouples resting border from the
  focus-ring border), `.cleanable(bool)` (clear button, 302-307), `.mask_toggle()` (password
  reveal, 308-316), `.content_type(InputContentType)` (317-324, feeds the macOS native text content
  type / autofill hints, gated by the `headless` feature per §1), `.disabled()/.readonly()`
  (331-346, readonly keeps focus/selection/copy but blocks edits), `.tab_index()/.tab_stop()`
  (347-363), `.context_menu(...)` (364-371, fully replace the native right-click menu),
  `.accessibility_id()/.aria_label()/.aria_description()` (218-234),
  `.on_focus_observed()/.on_hover_observed()/.on_press_observed()` (235-260, the local diff hooks,
  §1).
- `Editor::new(&Entity<EditorState>)` (input/editor.rs:37-51) — same shape, plus `.context_menu()`
  builder returning a `NativeMenu`, and forces monospace font/size/line-height from the theme before
  any instance style is applied (editor.rs:117-140, `.refine_style(&self.style)` last so instance
  overrides win — proven by its own test at editor.rs:190-197: a `.text_size()` override changes the
  row height).
- `InputState::new(window, cx).placeholder(...)` etc. is how apps/desktop constructs inputs today
  (views/mod.rs:202, views/project_admission.rs:17).

**Styling / override story**:
- `Input` implements `Styled` over its own `StyleRefinement` (input/input.rs:416-421) and merges it
  with `.refine_style(&self.style)` at input.rs:667-668, positioned **after** the themed
  `bg`/`rounded`/`border` block (input.rs:657-664) and **before** the focused-state border
  (input.rs:669-671) — so an instance can freely override background/border/radius, but the
  *focused* border color (`cx.theme().ring`) is applied via a separate `.styles(|styles|
  styles.focused(...))` pseudo-state block set up earlier on the underlying `BaseInput`
  (input.rs:609-616) and via `.focus_ring_style(window, cx)` (input.rs:670-671,
  vendor/gpui_ce_components/src/styled.rs's focus-ring helper) — these are not overridden by
  `.refine_style()`. **To fully replace the focus treatment with a custom two-tone bevel, an app must
  call `.appearance(false).bordered(false).focus_bordered(false)` and paint its own frame around
  `Input`**, rather than trying to out-style the built-in focus ring.
- Reads `cx.theme()` for: `foreground`, `muted_foreground`, `editor_background()`,
  `border`, `selection`, `caret`, per-severity diagnostic colors from `highlight_theme.style.status`
  (input.rs:432-448), plus `input`/`ring` for the resting/focused border and `radius` for corner
  rounding (input.rs:657-664).
- `Editor` always forces `mono_font_family`/`mono_font_size` from the theme first (editor.rs:124-125)
  — an instance-level `.font_family()`/`.text_size()` call still wins because it's applied via the
  `Styled` chain afterward on the returned `Input`, per the test above.

**Keyboard/focus/IME**: full `EntityInputHandler` IME support lives in `gpui_base` (state.rs proven by
CJK composition tests, see §0) — this crate's `Input`/`Editor` just wire `track_focus`,
`tab_index`/`tab_stop`, and `accessibility_role`/`aria_*` attributes on top
(input.rs:605-608,617-644). Right-click menu is a full native-style menu with
Cut/Copy/Paste/Select-All plus (for code-editor mode) Go to Definition / Show Code Actions
(input.rs:483-527) — LSP-shaped (`GoToDefinition`, `ToggleCodeActions`, `CompletionProvider`,
`CodeActionProvider`, `HoverProvider`, `DefinitionProvider` all re-exported at input/mod.rs:17-31)
which is far beyond what a docs reader needs but comes at no extra cost if unused.

**Virtualization**: not applicable to a single input; large documents inside `Editor`/`Textarea` are
handled by `gpui_base`'s rope + display-map machinery (`DisplayMap`, `DisplayPoint`, `BufferPoint`,
input/mod.rs:20), not by a windowed/virtualized element list.

**Popovers** (`input/popovers/`): `completion_menu.rs` (431 lines), `hover_popover.rs` (267 lines),
`code_action_menu.rs` (331 lines), `diagnostic_popover.rs` (62 lines) — LSP-style editor chrome
(autocomplete, hover docs, code actions, diagnostics squiggle popover). Irrelevant to a docs reader
unless it embeds a live code editor; otherwise dead weight that still compiles in.

**Verdict: REUSE BEHAVIOUR BUT RESTYLE.** The IME/rope/undo/selection engine (`gpui_base`) is exactly
the kind of correctness-critical, easy-to-get-wrong code worth keeping; `Input`'s own chrome
(`appearance`/`bordered`/`focus_bordered` all default off-able) is designed to be turned off so an app
can wrap it in its own frame, which is the right path for chamfered/bevel cards. `Editor`'s LSP-shaped
popovers are `SKIP`-worthy dead weight for a docs reader (no editing, no LSP) unless code blocks need
inline editing.

---

## 7. `highlighter/` — tree-sitter syntax highlighting (CRITICAL FINDING: currently fully disabled)

**What it is**: a tree-sitter-based syntax highlighter (`SyntaxHighlighter`,
highlighter/highlighter.rs:35-1791) plus a `Language` enum/registry
(highlighter/languages.rs, highlighter/registry.rs) and a `HighlightTheme`/`SyntaxColors` theme model
(registry.rs:63-531) that both the code `Editor` (§6) and Markdown fenced code blocks (§8) render
through.

**Key API**:
- `SyntaxHighlighter::new(lang: &str)` (highlighter.rs:337), `.update(edit, text, timeout)`
  (highlighter.rs:529) — incremental re-parse, `.edit_tree(...)` (504), `.styles(...)` (1064) —
  produces the `Vec<(Range<usize>, HighlightStyle)>` a renderer paints.
- `LanguageRegistry::singleton()` / `.register(lang: &str, config: &LanguageConfig)` /
  `.language(name)` (registry.rs:531-568) — apps can **register a custom language at runtime**
  without needing the language's tree-sitter grammar compiled in, as long as they supply their own
  `LanguageConfig`/highlighting (proven by the test
  `code_block_highlighter_cache_refreshes_after_language_registration`,
  vendor/gpui_ce_components/src/text/node.rs:3105-3142). This is a real escape hatch if the workspace's
  own OXC/tree-sitter-based analyzers (used elsewhere in this repo for the index/compile tooling) are
  preferred over pulling in more tree-sitter grammar crates.
- `HighlightTheme::default_dark()/default_light()` (registry.rs:515-523);
  `Theme.highlight_theme` is part of the app theme (§4).

**THE ACTUAL PROBLEM — every single language is individually cargo-feature-gated, and NONE are
currently turned on anywhere in this workspace:**
- highlighter/mod.rs:9-38 — `SyntaxHighlighter`/`languages`/`registry` (the real engine) only compile
  under `#[cfg(feature = "tree-sitter")]`; otherwise `wasm_stub.rs` is compiled instead, whose
  `SyntaxHighlighter::highlight()` is a no-op returning an empty `Vec`
  (highlighter/wasm_stub.rs:11-21) and whose `input_highlighter_factory()` fallback is
  `Rc::new(|_| None)` (highlighter/mod.rs:14-17).
- **Every** `Language` variant — not just Python/Java/C#/C++, but Rust, TypeScript, JavaScript, Go,
  HTML, JSON, Markdown, etc. too — is behind its own `#[cfg(feature = "tree-sitter-<lang>")]`
  (highlighter/languages.rs:5-72, e.g. `#[cfg(feature = "tree-sitter-rust")] Rust,` at line 60,
  `#[cfg(feature = "tree-sitter-python")] Python,` at line 58, `#[cfg(feature = "tree-sitter-csharp")]
  CSharp,`/`#[cfg(feature = "tree-sitter-cpp")] Cpp,` at lines 18-20, `#[cfg(feature =
  "tree-sitter-java")] Java,` at line 38). `Json` and `Plain` are the only always-on variants
  (languages.rs:6-7).
- The vendored `Cargo.toml` defines the base `tree-sitter` feature and ~40 per-language features
  (`[features]` table starts at vendor/gpui_ce_components/Cargo.toml:41, `headless = []` at :42,
  `tree-sitter = [...]` at :48) but **has no `default = [...]` key anywhere in the file** — an empty
  default feature set.
- Checked every `Cargo.toml` in the repo (`grep -rn "gpui_component\|gpui_ce_components"
  --include=Cargo.toml .`): the only place any feature is turned on for this crate is
  `apps/desktop/Cargo.toml:40` (`gpui_component/headless`, for the visual-harness build). **Nothing
  anywhere enables `tree-sitter` or any `tree-sitter-<lang>` feature.** (The `tree-sitter-rust`,
  `tree-sitter-python`, etc. crate names that do appear elsewhere — `frontends/rust/Cargo.toml:24`,
  `frontends/python/Cargo.toml:16`, `frontends/go/Cargo.toml:16`, `frontends/java/Cargo.toml:15`,
  `frontends/csharp/Cargo.toml:16`, `frontends/clang/Cargo.toml:17`,
  `frontends/typescript/Cargo.toml:24` — are direct tree-sitter grammar dependencies of this repo's
  own *index/compile* tooling, unrelated to `gpui_ce_components`'s cargo features.)

**Consequence**: as currently wired, apps/desktop compiles `gpui_ce_components` with the `tree-sitter`
feature off. Both the `Editor` code-editing surface (§6) and Markdown fenced code blocks
(text/node.rs:19,39,1221-1226, `CODE_BLOCK_HIGHLIGHTERS` cache keyed by language,
`SyntaxHighlighter::new(lang)`) currently render code with **zero syntax highlighting** — every
language falls through to the stub's empty-`Vec` highlight result. This is a one-line-per-language
Cargo.toml fix, not a code defect, but it means "does the highlighter support Rust/TS/JS/Python/Go/
Java/C#/C++" has two different true answers: the *code* supports all of them (assuming a grammar crate
exists — `tree-sitter-csharp`→`dep:tree-sitter-c-sharp`, `tree-sitter-cpp`→`dep:tree-sitter-cpp`, etc.,
Cargo.toml:68-74,108-114), but the *build as shipped today* supports none of them. To get all seven
requested languages: enable `gpui_component/tree-sitter` plus `tree-sitter-rust`,
`tree-sitter-typescript` (and `tree-sitter-tsx` for `.tsx`), `tree-sitter-javascript`,
`tree-sitter-python`, `tree-sitter-go`, `tree-sitter-java`, `tree-sitter-csharp`, `tree-sitter-cpp`.

**Bundled query overrides**: `highlighter/languages/{go,html,javascript,json,kotlin,lua,markdown,
markdown_inline,php,rust,typescript,zig}/` contain locally-bundled tree-sitter query files (e.g.
highlight queries) for those 12 languages specifically — i.e. those are the languages this fork ships
*custom/overridden* queries for. Python, Java, C#, C++ (and the rest of the ~40-language list) have no
local query folder and instead consume the upstream grammar crate's own exported
`tree_sitter_<lang>::HIGHLIGHT_QUERY` directly (confirmed for C++ at languages.rs:429-432,
`tree_sitter_cpp::LANGUAGE`/`tree_sitter_cpp::HIGHLIGHT_QUERY`) — functionally fine, just means no
local tuning has been done for those four of the seven requested languages.

**Styling**: highlight colors come from `Theme.highlight_theme: Arc<HighlightTheme>` → `SyntaxColors`
(registry.rs:113-176,242-317) resolved by scope name (`.style(name: &str)`) — fully re-themeable
through the same `ThemeConfig.highlight` JSON block used for the rest of the theme (§4), independent
of the base `ThemeColor` palette.

**Verdict: REUSE BEHAVIOUR BUT FIX THE BUILD FIRST.** The engine, registry, and runtime-registration
escape hatch are solid and exactly what's needed; but this is not "reuse as-is" until the Cargo feature
flags are turned on for the seven required languages, and it's worth deciding upfront whether to also
lean on the workspace's own OXC/tree-sitter frontends (`frontends/rust`, `frontends/typescript`, etc.)
via `LanguageRegistry::register` instead of pulling in a second, independent set of tree-sitter grammar
crates for the same seven languages.

---

## Architecture note (applies to almost every module below)

The vendored crate `vendor/gpui_ce_components` (lib name `gpui_component`) is a thin **presentation skin** over a separate crate, `gpui_ce_components_base` (imported as `gpui_base`, source at `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/gpui_ce_components_base-0.2.0/`). For several modules the vendor file is a near-empty re-export wrapper and all real behavior/virtualization/state-machine logic lives in `gpui_base`. I read both trees. This split is very good news for a heavy-custom-visual rebuild: behavior (virtualization, drag, resize, focus, persistence) and presentation (colors/chrome) are cleanly separated in the newer modules (virtual_list, tree, dock, scroll, resizable, popover/hover_card), so a custom skin can be written against `gpui_base` directly while discarding the vendor skin. Older modules (tab, list_item styling) hardcode theme colors with no override point.

---

## 8. `text/` — Markdown/HTML rich-text renderer

**What it is:** A rich-text view (`TextView`) that parses Markdown (GFM via the `markdown` crate, mdast) or HTML (via `html5ever`) into an internal node tree and renders it as styled GPUI elements, with selection, code blocks, tables, and syntax highlighting.

**Entrypoint / key API:**
- Constructors: `TextView::markdown(id, source)`, `TextView::html(id, source)`, free functions `markdown(source)` / `html(source)` (vendor/gpui_ce_components/src/text/mod.rs:37-48), or `TextView::new(&Entity<TextViewState>)` for externally managed state (text_view.rs:109).
- Builder methods: `.style(TextViewStyle)` (text_view.rs:169), `.selectable(bool)` (:176), `.selection_format(SelectionFormat)` (:184), `.scrollable(bool)` (:201), `.max_lines(n)` — clamps to whole lines, never splits a glyph line (:224), `.code_block_actions(F)` (:233), `.table_actions(F)` (:248), presumably `.link_click_handler(...)` (type defined text_view.rs:27-28).
- Render entrypoint: `impl Element for TextView` in text_view.rs:432, delegating the actual node tree render to `TextViewState::render` (state.rs:585) → `document.render_root(...)` (document.rs:178-216).
- Markdown parse entrypoint: `text::format::markdown::parse()` (format/markdown.rs:16-21), builds a `ParsedDocument` from `markdown::to_mdast`. HTML parse: format/html.rs uses `html5ever::parse_document` into an `RcDom`, then converts to the same `ParsedDocument`/`BlockNode`/`InlineNode` tree (format/html.rs:1-16).
- Node rendering (headings, paragraphs, lists, blockquotes, tables, code blocks, links, images, HR) lives in text/node.rs (3308 lines) — `BlockNode::CodeBlock(code_block) => code_block.render(...)` at node.rs:2422.
- Links: click handling centralized in `handle_link_click()` (style.rs:30-48) — opens URL via `cx.open_url` unless a custom `link_click_handler` is set; link color hardcoded via `highlight.color = Some(cx.theme().link)` (node.rs:1502, :1624).
- Code blocks / syntax highlighting: `CodeBlock` (node.rs:1145) caches a `SyntaxHighlighter` per code block (`crate::highlighter::{SyntaxHighlighter, LanguageRegistry}`, node.rs:19, :1221-1226). The highlighter module (vendor/gpui_ce_components/src/highlighter/) is tree-sitter based behind `#[cfg(feature = "tree-sitter")]` (highlighter/mod.rs:9-37), with a WASM stub fallback when the feature is off.

**Styling:** Heavily reads `cx.theme()` / `ActiveTheme`. Specific fields consumed: `theme().radius`, `tokens.muted`, `mono_font_family`, `mono_font_size`, `highlight_theme` (code blocks, node.rs:1324-1334); `theme().link` (link color); `theme().table_row_border`, `tokens.table_head`, `table_head_foreground`, `tokens.table`, `border` (tables, node.rs:2152-2295); `theme().muted_foreground`, `secondary_active` (blockquote, node.rs:2376-2378); `theme().border` (HR, node.rs:2436); `theme().primary`, `primary_foreground`, `tokens.primary` (selection/marks, node.rs:1839-1844).
Per-instance override is **partial**: `TextViewStyle` (style.rs) exposes explicit override points — `paragraph_gap`, `heading_font_size` (closure), `code_block`/`table`/`table_head`/`table_cell` (`StyleRefinement`s merged via `.refine_style(...)`), and `inline_code` (`HighlightStyle`, falling back to `cx.theme().accent` only if unset, style.rs:143-149). `TextView` also implements `Styled` (text_view.rs:101-105) for the outer container. But headings/paragraphs/blockquotes/links/HR colors are **hardcoded from theme with no override hook** — only sizing (heading_font_size) is customizable, not color.

**Keyboard/focus/IME:** `TextView` tracks a `FocusHandle` (text_view.rs:495, :509 `track_focus`) and binds `cmd/ctrl-c` → Copy and `cmd/ctrl-a` → SelectAll via `KeyBinding` (state.rs:40-46), handled in text_view.rs:515-525. No IME handling found (it's a read-only text view, not an editable input). `window_selection.rs` — despite implementing full window-selection test harnesses — is `#[cfg(test)]`-only per mod.rs:14, not shipped runtime code.

**Virtualization:** Conditional. Non-scrollable (default) mode renders the entire document tree eagerly. Scrollable mode (`.scrollable(true)`) passes a `ListState` into `document.render_root(Some(list_state), ...)` (state.rs:603-612) which uses `gpui::list(list_state, ...)` for block-level virtualization (document.rs:216).

**VERDICT: WRAP.** The Markdown/HTML→node parsing, code-block syntax highlighting, selection engine, and clamping logic are substantial and worth keeping; only the container/table/code-block styles are overridable, so a custom "cut card" visual would require wrapping `TextView` with `TextViewStyle` for the parts that are overridable and accepting the fixed theme-driven look for headings/links/blockquotes (or forking node.rs for those specific colors).

---

## 9. `virtual_list.rs` — top-level VirtualList / h_virtual_list / v_virtual_list

**What it is:** vendor/gpui_ce_components/src/virtual_list.rs is a **2-line re-export**: `pub(crate) use gpui_base::virtual_list;` and `pub use gpui_base::{VirtualList, VirtualListScrollHandle, h_virtual_list, v_virtual_list};` (virtual_list.rs:1-2). All logic is in `gpui_base`.

**Key API** (gpui_ce_components_base-0.2.0/src/virtual_list.rs): `v_virtual_list(view, id, item_sizes: Rc<Vec<Size<Pixels>>>, f)` (:140-151) and `h_virtual_list(...)` (:161-172) build a `VirtualList` element. Builder methods on `VirtualList`: `.track_scroll(&VirtualListScrollHandle)` (:237), `.with_sizing_behavior(ListSizingBehavior)` (:244), `.with_item_to_measure_index(usize)` (:250). `VirtualListScrollHandle` supports `.scroll_to_item(ix, strategy)` (:108), `.scroll_to_bottom()` (:124), and implements `crate::ScrollbarHandle` (:62-78) so it plugs directly into the Scrollbar component.

**Styling:** Implements `Styled` (virtual_list.rs:230-234, delegates to inner `Stateful<Div>`), so full per-instance style override via chained `.bg()`, `.p_x()`, etc. No hardcoded theme colors in this file at all — it is a pure layout/virtualization primitive.

**Keyboard/focus/IME:** None — it's an unfocusable layout container; focus/keys belong to whatever it renders.

**Virtualization:** Real, custom two-axis virtualization (not `uniform_list`) supporting **variable item sizes** along the scroll axis (only height used for vertical, width for horizontal; cross-axis inferred by measuring one item). Visible-range computation happens in `prepaint` (:656-716) by walking cumulative item sizes; only elements in that range are laid out/painted (:718-748). Confirmed by an in-repo test asserting only a partial range renders (virtual_list.rs test `vertical_visible_range_and_deferred_scroll_are_preserved`, :854-856).

**VERDICT: REUSE AS-IS.** Pure virtualization primitive, fully `Styled`, zero hardcoded visuals — perfect as the low-level scrolling engine under custom-styled rows/cards.

---

## 10. `list/` — List component (search, sections, selection, keyboard nav)

**What it is:** A delegate-driven, virtualized, optionally-searchable selection list (`ListState<D: ListDelegate>` + `List<D>` element), built on `v_virtual_list` (list/list.rs:14, :537).

**Key API:**
- `ListDelegate` trait (list/delegate.rs:10-45+): `items_count(section, cx)`, `render_item(ix, window, cx) -> Option<Self::Item>` (`Item: Selectable + IntoElement`), `sections_count`, `render_section_header/footer`, `perform_search` (async `Task<()>`), `load_more`/`has_more`.
- `ListState::new(delegate, window, cx)` (list.rs:94), builders `.searchable(bool)` (:125), `.selectable(bool)` (:136); runtime methods `set_selected_index`, `scroll_to_item`, `scroll_to_selected_item`, `scroll_handle()` (returns `&VirtualListScrollHandle`).
- `List::new(&Entity<ListState<D>>)` element (list.rs:717) with `.scrollbar_visible(bool)` (:726), `.search_placeholder(...)` (:732), and `Sizable`/`Styled`.
- Row rendering wraps delegate items in `ListItem` (list/list_item.rs), which implements `Selectable`/`Disableable`.

**Styling:** Reads `cx.theme()` extensively — `theme().border` (search input divider, list.rs:666), `ListItem` uses `theme().foreground`, `tokens.list_hover` (hover bg, list_item.rs:212), `theme().list_active`/`accent`/`list_active_border` for selected state (list_item.rs:239-256), gated by `cx.theme().list.active_highlight`. Override: `ListItem` implements `Styled` and applies `.refine_style(&self.style)` early (list_item.rs:196), **but** the selected/hover backgrounds are applied *after* that refinement (list_item.rs:211-213, :238-258) — so a caller's `.bg(...)` on the base item is overridable for the resting state but is **clobbered by the hardcoded accent/hover color for the selected/hover states** — same pattern/limitation as `Tab` (see §5).

**Keyboard/focus/IME:** `ListState` is `Focusable` (list.rs:593-604, falls back to the search `InputState`'s focus handle when searchable). Key bindings registered globally in `init()` (list.rs:26-35): Escape→Cancel, Enter→Confirm, Ctrl/Cmd+Enter→secondary confirm, Up/Down→Select prev/next, dispatched via `on_action` (list.rs:680-684). No IME (it's a selection list, not text entry — the search box itself is a full `InputState`/`Input`, which likely does support IME, but that's outside this scope's list.rs/list_item.rs files).

**Virtualization:** Real — uses `v_virtual_list` internally for rows/section headers/footers (list.rs:536-585), plus `load_more_if_need` triggered from the visible range's end for infinite scroll (list.rs:325-348, :542-547).

**VERDICT: REUSE BEHAVIOUR BUT RESTYLE.** Delegate model, virtualization, search-with-debounce, keyboard nav, and infinite scroll are all solid and reusable; `ListItem`'s selected/hover colors need a fork or a wrapper since they aren't overridable via `Styled` alone.

---

## 11. `tree.rs` — Tree component

**What it is:** vendor/gpui_ce_components/src/tree.rs (124 lines) is a **thin legacy-API wrapper** over `gpui_base::Tree`/`TreeState` (tree.rs:15, :82). All expand/collapse/selection/keyboard state lives in `gpui_base`.

**Key API (vendor wrapper):** `tree(state: &Entity<TreeState>, render_item)` (tree.rs:18-23), `Tree::new(state, render_item)` (:41), `.context_menu(F)` to attach a right-click `PopupMenu` (:55-62). Renders via `gpui_base::Tree::new(&self.state).item(...).list_style(...)` and adds `.vertical_scrollbar(&scroll_handle)` (tree.rs:82-111).

**Key API (gpui_base real impl, `.../src/tree.rs`):** `TreeItem::new(id, label)` with `.child()`, `.children()`, `.expanded(bool)`, `.disabled(bool)` (base tree.rs:97-127); `TreeState::new(cx)` with `.items(Vec<TreeItem>)`, `set_items`, `set_selected_index`, `set_selected_item` (auto-expands ancestors, base tree.rs:228-239), `reveal_item`, `scroll_to_item`; `Tree::new(&Entity<TreeState>).item(render_item_fn).list_style(StyleRefinement)` (base tree.rs:467-491) — note this is a **different, unstyled** `Tree` than the vendor wrapper of the same name, extended by it.

**Styling:** Vendor's `Tree` implements `Styled` (tree.rs:65-69, applied via `.refine_style(&self.style)`, :110) for the outer container; the base `Tree` also implements `Styled` (base tree.rs:494-498) and exposes `.list_style(StyleRefinement)` for the internal `uniform_list` (base tree.rs:488-491, applied at :453). Row appearance is 100% delegate-supplied (`render_item` returns app-owned `ListItem`/`AnyElement`) — the Tree itself paints **no** chrome/colors of its own (no `cx.theme()` calls in either tree.rs file). This is the best-decoupled component reviewed.

**Keyboard/focus/IME:** Full keyboard nav — `FocusHandle` (base tree.rs:183, :278 `focus()`), `KeyBinding`s for Up/Down/Left/Right (expand/collapse)/Confirm bound in `init()` under key context `"Tree"` (base tree.rs:15-30), dispatched via `on_action` (base tree.rs:512-516). `track_focus` (base tree.rs:511). No IME (not text entry).

**Virtualization:** Real — base `TreeState::render` uses `gpui::uniform_list("entries", entries.len(), ...)` (base tree.rs:421-454), so only visible rows render; `UniformListScrollHandle` supports `scroll_to_item`.

**VERDICT: REUSE AS-IS.** Zero hardcoded visuals, full keyboard/expand-collapse/selection behavior, real virtualization, and row rendering is 100% delegate-owned — ideal substrate for chamfered custom tree rows.

---

## 12. `tab/` — Tabs component

**What it is:** A themed tab strip (`Tab` + `TabBar`) with 5 visual variants (Tab/Outline/Pill/Segmented/Underline), a spring-animated sliding selection indicator, and an overflow menu; built on `gpui_base::Tab`/`gpui_base::Tabs` for interaction primitives.

**Key API:**
- `Tab::new()` (tab.rs:479), `.label(...)`, `.icon(...)`, `.with_variant(TabVariant)` / `.pill()`/`.outline()`/`.segmented()`/`.underline()` (tab.rs:506-533), `.prefix()`/`.suffix()`, `.disabled()`, `Selectable::selected()`.
- `TabBar::new(id)` (tab_bar.rs:69), `.with_variant(...)`, `.menu(bool)` (overflow dropdown), `.max_width(px)` (truncates long labels, exempts icon-only tabs), `.track_scroll(&ScrollHandle)`, `.selected_index(usize)`, `.on_click(F)` (group-level, overrides child `on_click`).

**Styling:** Extremely theme-driven — each `TabVariant` has 4 state functions (`normal`/`hovered`/`selected`/`disabled`, tab.rs:129-333) each pulling ~5 theme fields (`tab_foreground`, `tab_active_foreground`, `tokens.tab_active`, `border`, `primary`, `tokens.secondary`/`secondary_hover`, `tokens.background`, etc.). `Tab` implements `Styled` (tab.rs:607-611, `self.base.style()`), **but** `render()` unconditionally calls `.text_color()`, `.bg()`, `.border_*()` with the variant-derived colors (tab.rs:757-828) on the same builder chain — since these are the same `StyleRefinement` slots the caller's pre-render `Styled` calls would have set, **the variant's hardcoded colors overwrite any caller override of background/text/border color**. Non-conflicting properties (padding, width, etc.) are unaffected. The sliding indicator (`TabBar::render_indicator`, tab_bar.rs:192-289) is also entirely theme-colored with no override hook.

**Keyboard/focus/IME:** Delegated to `gpui_base::Tab`/`gpui_base::Tabs` (interactivity via `InteractiveElement`/`StatefulInteractiveElement`, tab.rs:599-605); click-based selection only in the files read — no explicit arrow-key tab-switching logic found in this vendor layer.

**Virtualization:** None (tabs render eagerly; horizontal overflow handled by scroll + an overflow menu, not virtualization).

**Animation:** Uses `gpui_base::{Spring, spring}` directly (tab_bar.rs:8) for a physically-interruptible sliding indicator (`INDICATOR_SPRING`, tab_bar.rs:26-28, spring calls at :235-248), and `crate::animation::{Lerp, ease_in_out_cubic}` (tab.rs:3) for an epoch-keyed text-color fade via GPUI's native `with_animation` (tab.rs:745-755).

**VERDICT: SKIP the vendor visuals / WRAP the interaction primitive.** Because color overrides are clobbered unconditionally in `render()`, achieving a two-tone chamfered/bevel look means either forking `tab.rs`'s variant-style functions or building a fresh `Tab`/`TabBar` on top of `gpui_base::Tab`/`Tabs` (which are undocumented in this pass but are the interactive base class) — the spring-indicator technique is worth copying regardless.

---

## 13. `dock/` — Dock/panel docking system

**What it is:** A full docking-window system (draggable/resizable/persistable panel layout: center + left/right/bottom docks, tab groups, floating tiles) split cleanly into **behavior** (`gpui_base::dock`, ~10.7k lines: layout tree, drag geometry, active-panel state machine, serialization) and **presentation** (vendor `dock/`, ~2.9k lines: the "skin"). Per the module doc comment: *"A `DockArea` built without `DockSkin` still docks, drags and persists — it simply draws no chrome at all."* (dock/mod.rs:1-16).

**Key API:**
- `DockSkin::dock_area(id, version, window, cx) -> (Entity<DockArea>, Rc<DockSkin>)` (dock/mod.rs:141-158), or manually `DockArea::new(id, version, window, cx).with_renderer(DockSkin::new(cx))`.
- `DockSkin` settings: `.set_panel_style(PanelStyle)` (Auto tab-bar-only-if-multiple vs always-TabBar, dock/panel.rs:31-37), `.set_toggle_button_visible(bool)`, `.set_tiles_scrollbar_mode(Option<ScrollbarMode>)`.
- Panel trait (presentation half, dock/panel.rs:71-140): `title()`, `tab_name()`, `title_style()` (custom `TitleStyle{background,foreground}` per panel), `title_suffix()`, `toolbar_buttons()`, `dropdown_menu()`, `zoom_control()`. Extends `gpui_base::dock::Panel` (behavior half: `panel_name()`, `visible`, `closable`, `zoomable`, `set_active`).
- **The extension point that matters most for a custom visual system:** `pub trait DockAreaRenderer` (gpui_base dock_area.rs:1922+) with hooks `frame()`, `split_frame()`, `center_frame()`, `render_split_handle()` (divider appearance; `None` keeps a 1px default line), `render_dock()` (chrome around one dock), `build_placeholder()`, `tab_group_renderer() -> Rc<dyn TabGroupRenderer>`, `tiles_renderer() -> Rc<dyn TilesRenderer>` — **every hook has a do-nothing default** (plain `div()` or `None`). The vendor's own test suite includes a `ChromelessDockSkin`/`ChromelessTabs`/`ChromelessTiles` (dock/dock.rs:264-301) proving a from-scratch skin needs only to implement `render_tab_bar`/`render_drag_bar` — everything else can be left at defaults.
- Drag-to-dock: `DragPanelPreview` (dock/tab_panel.rs:60-78) shows a themed preview while dragging a tab; drop targets highlighted via `cx.theme().tokens.drop_target` (tab_panel.rs:593, :607, :776). Behavior (`AnyDrag`, `DropIndicator`, `DropTarget`, `InsertTarget`) lives in `gpui_base::dock`.
- Resizing: dock-level resize uses the same `resize_handle`/`ResizeHandleContext` primitive as the standalone resizable-panel module (dock/dock.rs:22, :106-133); resizing a dock's own box is entirely handled by `gpui_base::dock` state plus a window-level mouse tracker (`DockResizeTracker`, dock/dock.rs:144-238).
- **Serialization:** `DockAreaState`, `DockState`, `PanelState`, `TileMeta` (gpui_base dock/state.rs:9-83) all derive `Serialize`/`Deserialize`/`PartialEq` — this is a persisted on-disk schema (doc comment: "mirrors a persisted, on-disk schema shipped to end users").

**Styling:** The default `DockSkin` (dock/dock.rs), `TabGroupSkin` (dock/tab_panel.rs:196+) and `TilesSkin` (dock/tiles.rs:58+) are all heavily theme-driven (`tokens.tab_bar`, `tokens.background`, `tokens.tiles`, `tile_radius`, `drag_border`, `drop_target`, etc.) — but because they're just one possible implementation of `DockAreaRenderer`/`TabGroupRenderer`/`TilesRenderer`, **a fully custom visual skin (chamfered cards, two-tone bevel) can be written from scratch as a new struct implementing these traits**, with zero forking of behavior code.

**Keyboard/focus/IME:** Each docked `Panel` is `Render + Focusable` (gpui_base dock/panel.rs:21); `actions!(dock, [ToggleZoom, ClosePanel])` (dock/mod.rs:63).

**Virtualization:** Not applicable at the dock level (panels are a bounded tree, not a long list); tiles/tab groups render eagerly. (List/Tree/TextView used *inside* panels bring their own virtualization.)

**VERDICT: WRAP (write a custom `DockAreaRenderer`).** This is the strongest case in the whole survey for "keep 100% of the behavior, replace 100% of the paint": persisted layout, drag/resize/zoom state machine, and panel registry are all in `gpui_base` and skin-agnostic; a from-scratch renderer implementing the two dozen hooks (most already default to "draw nothing") gets a docking multi-pane app with the exact custom visual language desired.

---

## 14. Resizable panels — `gpui_base::resizable` (NOT in the vendored tree)

Confirmed via `find ~/.cargo/registry/src -maxdepth 1 -iname '*gpui*'` (misses it — nested one level deeper) and `find ... -iname '*gpui_ce_components_base*'`: the source is at `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/gpui_ce_components_base-0.2.0/src/resizable/{mod.rs,panel.rs,resize_handle.rs}`. Vendor's `pub mod resizable { pub use super::{...} }` (vendor lib.rs:61-66) is a pure re-export.

**Key API:** `h_resizable(id)` / `v_resizable(id)` → `ResizablePanelGroup` (resizable/mod.rs:17-24); `.with_state(&Entity<ResizableState>)`, `.axis(Axis)`, `.child(panel)`/`.children(...)`, `.size(px)` (cross-axis), `.on_resize(F)`, `.with_handle_appearance(ResizeHandleRenderer)` (panel.rs:59-62, hands the **painted part** of every divider to a caller closure while base keeps the hit area/cursor/drag). `resizable_panel()` → `ResizablePanel` with `.size(px)`, `.size_range(Range<Pixels>)`, `.visible(bool)`; implements `Styled` — doc comment explicitly states caller style is applied *between* the panel's flex defaults and its runtime size constraints (panel.rs:198-224), so overriding e.g. `.flex_none()`/padding/colors is a supported, documented pattern (unlike Tab/ListItem). `ResizableState` (panel.rs, actually mod.rs:33-386) exposes `resize_panel(ix, size, ...)`, `insert_panel`, `remove_panel`, `reset_panel`, `sizes()`.

**Styling:** The divider itself (`resize_handle`, resize_handle.rs) is the only themed part: `handle_color()` resolves `theme.resizable.active_handle` / `theme.resizable.handle`, falling back to `theme.tokens.colors.ring`/`border` (animation.rs:282-291 — note: this file is `gpui_ce_components_base-0.2.0/src/animation.rs`, not vendor's). **Fully overridable per-instance**: `ResizeHandleContext{axis, is_active}` is passed to a caller-supplied `ResizeHandleRenderer` (`Rc<dyn Fn(&ResizeHandleContext, &mut Window, &mut App) -> Option<AnyElement>>`, resize_handle.rs:27-37); returning `None` keeps the built-in line, so partial overrides (one handle) are possible.

**Keyboard/focus/IME:** None — pure mouse-drag resizing (`on_drag`/`on_mouse_event` for move/up, panel.rs:342-366, mod.rs's `ResizePanelGroupElement::paint`).

**Virtualization:** N/A.

**VERDICT: REUSE AS-IS.** Panel-size math (min/max clamping, redistribution on container resize, drag-follows-pointer) is nontrivial and well-tested (mod.rs tests at :456-557); both the panel content style and the divider's paint are documented, supported per-instance override points — exactly what a bevel/chamfer divider needs.

---

## 15. `scroll/` — Scrollbar / scroll area

**What it is:** Two independent pieces: (a) `Scrollbar`/`ScrollbarHandle` — a themed custom scrollbar, thin-wrapped from `gpui_base` (scroll/mod.rs:4-11 re-exports `gpui_base::{Scrollbar, ScrollbarAxis, ScrollbarEntrance, ScrollbarHandle, ScrollbarMode, ScrollbarMotion, ScrollbarStyles, ScrollbarThumbStyle, ScrollbarTrackStyle}`), and (b) `ScrollableElement`/`Scrollable<E>`/`ScrollableMask` (vendor's own scrollable.rs / scrollable_mask.rs) — extension traits that attach scrollbars to any element and a wheel-routing helper for nested horizontal scroll areas.

**Key API:**
- `ScrollableElement` trait (scrollable.rs:16-63): `.scrollbar(&handle, axis)`, `.vertical_scrollbar(&handle)`, `.horizontal_scrollbar(&handle)`, `.overflow_scrollbar()`/`.overflow_x_scrollbar()`/`.overflow_y_scrollbar()` (wraps the element in `Scrollable<E>` while preserving it as the scroll area, not a wrapper div).
- `Scrollbar::vertical(&handle)` / `::horizontal(&handle)` (gpui_base scrollbar.rs:802-808); `.styles(|styles| styles.track(...).thumb(...).thumb_hover(...).thumb_active(...))` (scrollbar.rs:666-704, :873) — **fully overridable per-instance** per-state colors/width/inset/radius, confirmed by an in-repo test (scroll/mod.rs tests:17-32) chaining all four state closures with custom `bg()`.
- `ScrollableMask::new(axis, &ScrollHandle)` (scrollable_mask.rs:69-99) + `horizontal_scroll_area(id, &scroll_handle, style, child)` (scrollable_mask.rs:22-45) — a wheel-input router so nested horizontal scrollers don't fight vertical ones; paints nothing of its own.

**Styling:** `Scrollbar::resolve_track`/`resolve_thumb` (gpui_base scrollbar.rs:895-927) read `cx.theme()` first, then apply the per-instance `.styles()` override on top (i.e., theme is the default, caller wins) — this is the best-designed override contract of anything surveyed. `ScrollableMask`/`Scrollable<E>` (vendor files) have **no** `cx.theme()` calls — pure interaction/layout utilities, delegate `Styled` to the wrapped element (scrollable.rs:96-103).
`ScrollbarMotion` (scrollbar.rs:284-370) supports fade/slide-and-fade thumb-hover entrance animation (`ScrollbarEntrance`, `.with_thumb_hover_entrance(...)`).

**Keyboard/focus/IME:** None (mouse-only).

**Virtualization:** N/A — scrollbars overlay whatever virtualized/non-virtualized content they're attached to.

**VERDICT: REUSE AS-IS.** Track/thumb/thumb-hover/thumb-active colors, width, inset and radius are all explicitly overridable per instance with theme as fallback only — ready to restyle into a thin two-tone bevel scrollbar without forking anything.

---

## 16. `popover.rs`

**What it is:** A click/programmatically-triggered floating panel (`Popover`), thin-wrapped around `gpui_base::Popover` (behavior: positioning, focus, outside-click dismiss) — popover.rs:15, :301-322.

**Key API:** `Popover::new(id)`, `.anchor(Anchor)`, `.mouse_button(MouseButton)`, `.trigger(impl Selectable + IntoElement)`, `.content(F)` (closure rebuilt every render, receives `&mut PopoverState`), `.default_open(bool)` / `.open(bool)` + `.on_open_change(F)` for controlled mode, `.appearance(bool)` (popover.rs:246-249 — **`false` strips bg/border/shadow/padding entirely**, i.e. a documented full-opt-out for custom chrome), `.overlay_closable(bool)`, `.track_focus(&FocusHandle)`.

**Styling:** `render_popover_content()` (popover.rs:274-291) applies `.popover_style(cx)` (theme border/bg/shadow/padding, from `ThemeStyled` trait) only `when(appearance, ...)`; caller's `Styled`-accumulated `self.style` is applied last via `.refine_style(&style)` (popover.rs:312) — full override available, and the panel can be made **fully unstyled** via `appearance(false)`, at which point the caller's own children/style define 100% of the visual. Includes a documented shadcn-style open animation (`dropdown_popup`, popover.rs:82-102, cube-eased shadow fade to work around GPUI's lack of group compositing).

**Keyboard/focus/IME:** `FocusHandle` tracking via `.track_focus()`/`track_focus` on the base popover, `tab_group()` on the content wrapper (popover.rs:283) for tab-cycling inside the popup. Behavior (open/close/outside-click) lives in `gpui_base::Popover`.

**Virtualization:** N/A (single floating panel).

**VERDICT: REUSE AS-IS.** `appearance(false)` plus `Styled` gives a documented, first-class path to a fully custom chamfered card with zero fighting against hardcoded colors; positioning/focus/dismiss behavior is solid and worth keeping.

---

## 17. `hover_card.rs`

**What it is:** Hover-triggered variant of Popover with open/close delays, thin-wrapped around `gpui_base::HoverCard` (hover_card.rs:7, :132-149). Explicitly reuses `Popover::render_popover_content` for its panel chrome (hover_card.rs:11, :138).

**Key API:** `HoverCard::new(id)`, `.anchor(Anchor)`, `.trigger(impl IntoElement)`, `.content(F)`, `.open_delay(Duration)` (default 600ms), `.close_delay(Duration)` (default 300ms), `.appearance(bool)` (same full opt-out as Popover), `.on_open_change(F)`.

**Styling:** Identical contract to Popover — `appearance(false)` disables all theme-derived chrome, `.refine_style(&style)` applied last (hover_card.rs:144). No component-unique `cx.theme()` calls in this file at all (it delegates entirely to `Popover::render_popover_content`).

**Keyboard/focus/IME:** None specific to this file (hover-driven, not focus-driven); underlying behavior in `gpui_base::HoverCard`.

**Virtualization:** N/A.

**VERDICT: REUSE AS-IS.** Same reasoning as Popover — trivial to keep the delayed-hover behavior and swap in fully custom card visuals via `appearance(false)`.

---

## 18. `tooltip.rs`

**What it is:** Two things in one file: (1) `Tooltip` — a themed content view (text, key-binding hint via `Kbd`, or custom element) rendered inside `gpui_base::Tooltip`; (2) a **managed/global tooltip system** (`ManagedTooltipExt::managed_tooltip`, tooltip.rs:229-293) that shows tooltips through a single per-window `Root::tooltip_overlay()` (`BaseTooltipOverlay`), so hovering different triggers reuses one overlay with slide/fade transitions.

**Key API:** `Tooltip::new(impl Into<Text>)` (:43) or `Tooltip::element(F)` for custom content (:53), `.action(&dyn Action, context)` — auto-resolves and displays the bound key combo via `Kbd::binding_for_action` (:97-100), `.key_binding(Option<Kbd>)`, `.build(window, cx) -> AnyView`. Extension methods any `StatefulInteractiveElement` gets for free: `.managed_tooltip(build_fn)`, `.managed_tooltip_at(placement, build_fn)`.

**Styling:** Hardcoded theme defaults — `cx.theme().font_family`, `tokens.popover`, `popover_foreground`, `border`, `radius`, `muted_foreground` (tooltip.rs:112-138) — but unlike `Tab`, **caller overrides win**: `.refine_style(&self.style)` (tooltip.rs:126) is the *last* call in the chain before children are attached, so a `Tooltip::new(...).bg(...)` set by the caller (captured into `self.style` via the `Styled` impl at :87-91) overrides the hardcoded theme background/border/etc. Confirmed by reading the call order directly (theme-derived `.bg()`/`.border_color()`/etc. all precede `.refine_style(&self.style)` in the same builder chain).

**Keyboard/focus/IME:** None (display-only on hover; dismissed via `on_mouse_down` :285-291). Managed-tooltip trigger tracking uses `.on_prepaint`/`.on_hover` to compute trigger bounds and forward them to the overlay (tooltip.rs:253-284).

**Animation:** Uses `crate::animation::{EffectTransition, ease_in_out_cubic, ease_out_cubic}` (tooltip.rs:15) for slide+fade on first show (`Enter`, tooltip.rs:178-186) and a horizontal slide when switching between adjacent triggers on the same row (`Switch`, tooltip.rs:158-177) — a nice small reference for the animation system in action.

**Virtualization:** N/A.

**VERDICT: REUSE BEHAVIOUR BUT RESTYLE.** The managed single-overlay-per-window architecture, key-binding display, and switch/enter transitions are all worth keeping as-is; the default color chrome is trivially restylable per-instance via `Styled` (no clobbering issue, unlike Tab), so a straight restyle (not a fork) suffices here.

---

## Animation / motion system (`gpui_base::animation`, `gpui_base::motion`)

Two related but distinct systems, both in `gpui_ce_components_base-0.2.0/src/{animation.rs, motion.rs}`:

- **`animation.rs`** (re-exported by vendor as `pub use gpui_base::animation;`, vendor lib.rs:91): `cubic_bezier(x1,y1,x2,y2) -> impl Fn(f32)->f32` (CSS-like easing builder, animation.rs:14-63), presets `ease_out_cubic`/`ease_in_cubic`/`ease_in_out_cubic` (:66-84), a `Lerp` trait (f32/Pixels/Point<Pixels>/Hsla, :91-125), and `EffectTransition` — a composable one-shot fade/slide/width/height transition applied via GPUI's native `with_animation` (:150-233; used by `tooltip.rs`'s enter/switch transitions).
- **`motion.rs`** (items re-exported individually at gpui_base's crate root: `pub use motion::{Interpolate, Spring, Transition, TransitionId, spring, transition};`, gpui_base lib.rs:114 — **not** forwarded under a `motion` name from the vendor crate's own `lib.rs`, so a consumer needs `gpui_base`/`gpui_ce_components_base` as a direct dependency to reach `Spring`/`spring` by that path, though vendor code itself imports them as `gpui_base::{Spring, spring}`, e.g. `tab_bar.rs:8`): `Transition` — CSS-like duration/delay/easing policy for a value `transition(id, target, policy, window, cx) -> T` (motion.rs:142-199, keyed by `window.use_keyed_state`, respects `cx.reduce_motion()`); `Spring` — physically correct, interruption-safe spring `spring(id, target, policy, window, cx) -> T` (motion.rs:352-420) that carries velocity across a retargeted animation instead of restarting (used for `TabBar`'s sliding indicator, `tab_bar.rs:26-28, 235-248`, and internally in scrollbar entrance motion).

Usage sites actually observed while reading these modules: `vendor/gpui_ce_components/src/tab/tab.rs:3` (`use crate::animation::{Lerp, ease_in_out_cubic};`, used at tab.rs:750), `vendor/gpui_ce_components/src/tab/tab_bar.rs:8` (`use gpui_base::{Spring, spring};`, used at tab_bar.rs:235-248), `vendor/gpui_ce_components/src/popover.rs` (`use crate::animation::ease_out_cubic;` at popover.rs:11, used at :94), `vendor/gpui_ce_components/src/tooltip.rs:15` (`use crate::animation::{EffectTransition, ease_in_out_cubic, ease_out_cubic};`, used at :169-186).

**Takeaway for the custom app:** both `animation::EffectTransition` (one-shot) and `motion::{transition, spring}` (continuous, retarget-safe) are generically reusable outside these specific components for driving the "live motion" and bevel/state transitions the design system calls for — they don't depend on any particular component's visuals.


---

Scope: `vendor/gpui_ce_components/src/{menu,command,searchable_list*,notification.rs,dialog,sheet.rs,kbd.rs,skeleton.rs,spinner.rs,switch.rs,slider.rs,setting,title_bar.rs,window_border.rs}`. All styling reads go through `cx.theme()` (the `ActiveTheme` trait, `crate::theme::Theme`), imported as `use crate::ActiveTheme` (or `ActiveTheme as _`) in essentially every file below.

---

## 19. `menu/` — context menus, dropdown menus, popup menu, app menu bar

**Files**: `menu/mod.rs`, `popup_menu.rs`, `context_menu.rs`, `menu_item.rs`, `dropdown_menu.rs`, `app_menu_bar.rs`.

1. **What it is**: A shared `PopupMenu` primitive (list of items/submenus/separators) plus three front-ends that place it: `ContextMenuExt::context_menu()` (right-click), `DropdownMenu::dropdown_menu()` (click-triggered, via `Popover`), and `AppMenuBar` (Windows/Linux menu bar, reads `GlobalState::global(cx).app_menus()`).

2. **Key public API**:
   - `PopupMenu::build(window, cx, |menu, window, cx| menu...)` → `Entity<PopupMenu>` (`menu/popup_menu.rs:348`). Builder methods: `.menu(label, action)`, `.menu_with_icon(...)`, `.menu_with_check(...)`, `.label(...)`, `.separator()`, `.submenu(label, window, cx, |menu,_,_| ...)`, `.item(PopupMenuItem)`, `.min_w/.max_w/.max_h`, `.scrollable(bool)`, `.check_side(Side)`, `.external_link_icon(bool)`, `.action_context(FocusHandle)`, `.rebuild(...)` for async content (`popup_menu.rs:401-737`).
   - `PopupMenuItem` builder: `::new/::element/::submenu/::separator/::label/::link`, `.icon/.action/.disabled/.checked/.on_click` (`popup_menu.rs:69-226`).
   - `ContextMenuExt::context_menu(f)` on any `InteractiveElement + ParentElement + Styled` (`context_menu.rs:19-36`); `ContextMenu::new(id, element)` wraps the target element and defers a `PopupMenu` at the mouse position on right-click (`context_menu.rs:143-341`).
   - `DropdownMenu::dropdown_menu(f)` / `.dropdown_menu_with_anchor(anchor, f)`, implemented for `Button` (`dropdown_menu.rs:11-33`); returns `DropdownMenuPopover` which wraps `Popover` (`dropdown_menu.rs:81-137`).
   - `AppMenuBar::new(cx)` → `Entity<AppMenuBar>`, `.reload(cx)` (`app_menu_bar.rs:32-62`).

3. **Styling**: Heavy `cx.theme()` use — `PopupMenu::render` reads `cx.theme().radius` (clamped to 8px, `popup_menu.rs:1425`), `.popover_foreground` (`:1441`), `border` for separators (`:1219`), and `menu_item.rs` (the shared row element `MenuItemElement`) reads `cx.theme().foreground`, `.tokens.accent`, `.accent_foreground`, `.muted_foreground` (`menu_item.rs:105-130`). `MenuItemElement` and `PopupMenu` items build on plain `div`/`h_flex` + `Styled`, but the row look (background/hover/selected/padding/rounding) is hardcoded inside `render_item`/`MenuItemElement::render` — there is **no per-instance `StyleRefinement` hook** exposed on `PopupMenu` or `PopupMenuItem` for the row chrome (only content, icon, check, disabled). `ContextMenu<E>` forwards `Styled` to the *wrapped trigger element*, not to the menu popup itself (`context_menu.rs:99-107`), so restyling the menu surface itself requires touching this crate's source, not builder calls.

4. **Keyboard/focus**: Full keyboard nav. `PopupMenu` has its own `FocusHandle` (`:284`), binds `Confirm/Cancel/SelectUp/SelectDown/SelectLeft/SelectRight` under key-context `"PopupMenu"` (`popup_menu.rs:20-29`, `:1433-1438`), tracks focus via `.track_focus(&self.focus_handle)` (`:1432`), and restores focus to a captured `previous_focus_handle`/`action_context` on dismiss (`:1029-1056`). `AppMenuBar` binds `Cancel/SelectLeft/SelectRight` under `"AppMenuBar"` (`app_menu_bar.rs:16-23`) and has its own `action_context` restoration logic (`:94-111`). `Role::Menu`/`Role::MenuItem`/`Role::MenuBar` and `aria_label`/`aria_selected` are set throughout for a11y (`popup_menu.rs:1430,1210`; `menu_item.rs:97-99`).

5. **Virtualization**: None — `scrollable(true)` just turns on `overflow_y_scroll()` + a real scrollbar over all rendered rows (`popup_menu.rs:1452-1456,1467-1469`); every menu item is rendered eagerly, capped informally at "auto-scrollable past 20 items" (`popup_menu.rs:795-797`).

6. **VERDICT: WRAP.** The keyboard nav, submenu/dismiss/focus-restoration machinery, and `Popover`/`ContextMenu` positioning logic (`context_menu.rs`, `dropdown_menu.rs`) are exactly the kind of fiddly, easy-to-get-wrong behavior worth keeping, but the visual surface (`MenuItemElement`, `PopupMenu::render_item`) is hardcoded with no `StyleRefinement` override point for chamfered/bevel visuals — so wrap the behavior and re-skin the row/panel rendering (fork `MenuItemElement`/the render body, or intercept via a themed radius/colors) rather than reusing pixel-for-pixel.

---

## 20. `command/` — command palette

**Files**: `command/mod.rs`, `command.rs`, `item.rs`, `state.rs`.

1. **What it is**: A fuzzy-searchable command palette (`Command` = render/config, `CommandState` = the entity holding query/selection/scroll state), with groups, separators, keybinding hints, and virtualized rows.

2. **Key public API**:
   - `CommandState::new(window, cx)` (entity constructor, `state.rs:141`); `.query()/.set_query()/.selected_index()/.set_selected_index()/.matched_count()/.focus()/.set_loading()` (`state.rs:198-300`).
   - `Command::new(&state)` then `.item(CommandItem)`, `.items(...)`, `.group(CommandGroup)`, `.separator()`, `.searchable(bool)`, `.filterable(bool)`, `.on_query/.on_select/.on_confirm/.on_cancel`, `.placeholder(...)`, `.empty(|state,window,cx| ...)`, `.max_h(...)`, `.bordered(bool)`, `.header(...)/.footer(...)` (`command.rs:70-231`).
   - `CommandItem::new().label(...).icon(...).action(Box<dyn Action>).checked(bool).keywords([...]).child(|window,cx| ...)` (`item.rs:35-104`); `CommandGroup::new().label(...).item(...)` (`item.rs:167-198`).

3. **Styling**: Reads `cx.theme().popover/.popover_foreground/.border/.radius_lg/.accent/.accent_foreground/.muted_foreground` throughout `render_row`/`item_row`/`heading_row`/the top-level `Render` (`state.rs:663-826`). `Command` implements `Styled` (`command.rs:234-238`) forwarding into `CommandOptions.style`, applied via `.refine_style(&self.options.style)` on the outer container only (`state.rs:827`) — so the frame (bg/border/rounding/max-height) is instance-overridable, but individual row/heading styling (colors, padding, selected background) is not exposed per instance, only globally via the theme.

4. **Keyboard/focus**: `CommandState` owns a `FocusHandle` and delegates to the search `InputState`'s focus handle when searchable (`state.rs:117,278-284,782-789`); binds `Cancel/Confirm/SelectUp/SelectDown` under key-context `"Command"` (`state.rs:61-69`, applied at `:812-817`). Kbd hints are rendered per-item via `Kbd::binding_for_action_in`/`Kbd::binding_for_action` (`state.rs:713-721`, and `menu`'s equivalent at `popup_menu.rs:1093-1117`).

5. **Virtualization**: **Yes.** Rows (headings/items/separators) are measured once (`measure_row_sizes`, `state.rs:602-629`) and rendered lazily via `v_virtual_list(command_state.clone(), "command-list", row_sizes, |this, visible_range, window, cx| ...)` (`state.rs:902-911`) — only the visible range is built each frame.

6. **VERDICT: REUSE BEHAVIOUR BUT RESTYLE.** Fuzzy filtering, grouping/heading logic, virtualized scrolling and the query/select/confirm/cancel state machine are substantial and worth keeping as-is; row/heading colors are theme-driven and not overridable per instance, so a heavily-custom look requires either re-skinning the global theme tokens it reads or forking `render_row`/`item_row`/`heading_row`.

---

## 21. `searchable_list.rs` + `searchable_list/` — shared list-search infrastructure

**Relationship**: `searchable_list.rs` (13 lines) is just the module declaration/re-export shim; `searchable_list/` holds the real code: `adapter.rs`, `change.rs`, `delegate.rs`, `item.rs`, `state.rs`, `vec.rs` (`searchable_list.rs:1-13`).

1. **What it is**: Not a standalone visible component — it's the shared plumbing behind `Select`/`ComboBox`/`MultiComboBox` (outside my scope): a `SearchableListDelegate`/`SearchableListItem` trait pair, an `adapter.rs` that bridges into the underlying (out-of-scope) `list::ListDelegate`/`ListState`, and a per-row `SearchableListItemElement`.

2. **Key public API**:
   - `SearchableListItem` trait: `.title()/.value()/.matches(query)/.disabled()/.render()/.display_title()` (`delegate.rs:8-43`).
   - `SearchableListDelegate` trait: `.sections_count/.items_count/.item/.position/.perform_search/.render_item/.render_section_header/.is_item_enabled/.is_item_checked/.on_will_change/.on_confirm` (`delegate.rs:46-200`) — a rich hook surface for custom filtering/rendering/selection semantics.
   - `SearchableListState::new(delegate, selected_indices, on_confirm, on_cancel, on_render_empty, on_blur, window, cx)` (`state.rs:56-130`), with `.selection()/.selected_values()/.is_open()/.add_selected_index/.remove_selected_index/.set_selected_indices` (`state.rs:134-233`).
   - `SearchableListItemElement::new(ix)` builder: `.checked(bool)/.check_icon(icon)`, plus `Styled/Selectable/Sizable/Disableable` (`item.rs:19-95`).
   - `Vec<T: SearchableListItem>` and blanket impls for `String`/`SharedString`/`&str` (`vec.rs:9-63`) give a zero-boilerplate delegate for simple lists.
   - `SearchableListChange::{Select,Deselect}` describes proposed selection edits a delegate can veto/rewrite in `on_will_change` (`change.rs:8-13`).

3. **Styling**: `SearchableListItemElement::render` reads `cx.theme().radius/.foreground/.accent.opacity(0.7)/.tokens.accent/.muted_foreground` (`item.rs:105-121`); it implements `Styled` (`.refine_style(&self.style)` at `item.rs:112`) so the row's outer box (padding, extra classes) is overridable per instance, but the selected/hover background colors are hardcoded to theme tokens, not parameterized. `adapter.rs`'s `render_section_header` fallback likewise hardcodes `text_color(cx.theme().muted_foreground)` (`adapter.rs:93-98`) — but a delegate can bypass this entirely by implementing `render_item`/`render_section_header` to return `Some(_)`, which fully replaces the row visuals (`delegate.rs:94-103,110-117`; `adapter.rs:118-125`).

4. **Keyboard/focus**: None directly here — it delegates to `list::ListState`/`ListDelegate` (out of scope) via `adapter.rs`; `SearchableListAdapter::confirm/cancel/set_selected_index` (`adapter.rs:146-171`) are just pass-throughs invoked by that underlying list.

5. **Virtualization**: Delegated — `SearchableListState` wraps `Entity<ListState<SearchableListAdapter<D>>>` (`state.rs:22`), and `list::ListState` is the same virtualized list infrastructure command/menu code elsewhere in the crate builds on (not itself in my scope, but confirmed as the mechanism).

6. **VERDICT: REUSE BEHAVIOUR BUT RESTYLE — but note this is a headless framework, not a visible component.** The delegate/hook design already gives full escape hatches for custom row rendering (`render_item` returning `Some`), so for a docs-reader app that needs `Select`/`ComboBox`-like searchable pickers, implementing a custom `SearchableListDelegate` + custom `render_item` is the intended reuse path rather than forking; only the small default `SearchableListItemElement` chrome needs restyling if used at all.

---

## 22. `notification.rs` — toast notifications

1. **What it is**: `Notification` (single toast, builder + `Render`) plus `NotificationList` (per-window manager, stacks by `Anchor` placement), built on `gpui_base::{ToastManager, ToastStack, ToastStackState, ToastMotion, ToastOptions, ToastTransitionStatus}` (`notification.rs:10-13`). Also supports OS-level system notifications via `NotificationDelivery`.

2. **Key public API**: `Notification::new()/.info()/.success()/.warning()/.error()`, `.title/.message/.icon/.with_type/.placement/.delivery/.system()/.in_app_and_system()/.autohide/.on_click/.on_close/.action(|self,window,cx| Button)/.content(...)/.id::<T>()/.id1::<T>(key)` (`notification.rs:162-383`). `NotificationList::new(window,cx)`, `.push(notification, window, cx)`, `.close/.close_by_type/.clear/.notifications()` (`:700-950`). `NotificationSettings { placement, margins, max_items, width, delivery }` (`:527-561`) is the theme-level default, overridable per-`Notification`.

3. **Styling**: `Notification::render` reads `cx.theme().info/.success/.warning/.danger` for the type icon color (`:41-44`), `cx.theme().border/.tokens.popover/.radius_lg` for the card chrome (`:421-424`), and `cx.theme().notification.placement` (`:412`). It implements `Styled` (`.style` → `StyleRefinement`, `notification.rs:388-392`) applied via `.refine_style(&self.style)` (`:429`) — so per-instance override of the toast card is possible for anything not hardcoded after that call (icon slot, close button, animation values are still fixed).

4. **Keyboard/focus**: `NotificationList` and each `AnchorStack` carry a `FocusHandle` — the primary placement's stack uses `self.focus_handle` (`tab_stop(true)`, `:720`), and secondary-placement stacks get their own `cx.focus_handle().tab_stop(true)` (`:982`) — wired into `gpui_base::ToastStack::focus_handle(...)` (`:997`), so the stack participates in tab order (sonner-style expand-on-focus is presumably handled inside `gpui_base::ToastStack`, out of scope).

5. **Virtualization**: None — bounded by `NotificationSettings::max_items` (default 10, `:557`), not a virtualized list.

6. **Animation (relevant to the `gpui_base::animation`/`crate::animation` question)**: Imports `crate::animation::cubic_bezier` (`notification.rs:17`), which is a re-export of `gpui_base::animation::cubic_bezier` (confirmed via `vendor/gpui_ce_components/src/lib.rs:91: pub use gpui_base::animation;`). Used in `Notification::render`'s `.with_animation(ElementId::NamedInteger("slide-down", closing as u64), Animation::new(duration).with_easing(cubic_bezier(0.25,0.1,0.25,1.)), move |this, delta| {...})` (`notification.rs:476-523`) to drive a combined slide + opacity + cubed-shadow-fade enter/exit transition, with different durations/offsets for enter (`NOTIFICATION_TRANSITION_DURATION` = 400ms) vs. exit (`NOTIFICATION_EXIT_DURATION` = 200ms) and direction chosen by `Anchor` placement (`:23-26,488-522`).

7. **VERDICT: REUSE BEHAVIOUR BUT RESTYLE.** The stacking/auto-hide/placement/system-notification bridging logic is substantial and non-trivial (timer loop at `:703-716`, `ToastManager` integration); the card visuals are theme-token-driven but overridable via `Styled`, so restyle via `.refine_style` + theme tokens rather than rebuilding the manager.

---

## 23. `dialog/` — modal dialogs

**Files**: `dialog/mod.rs`, `dialog.rs`, `content.rs`, `header.rs`, `footer.rs`, `title.rs`, `description.rs`, `alert_dialog.rs`.

1. **What it is**: `Dialog` is the styled wrapper around `gpui_base::Dialog`/`gpui_base::AlertDialog` (the actual modal-stack/focus-trap/backdrop host, out of scope), providing declarative `DialogHeader/DialogTitle/DialogDescription/DialogContent/DialogFooter` pieces and an `AlertDialog` convenience type built on top of `Dialog`.

2. **Key public API**: `Dialog::new(cx)`, `.trigger(...)`, `.content(|content,window,cx| DialogContent)`, `.title(...)/.header(...)/.footer(...)`, `.button_props(DialogButtonProps)`, `.on_close/.on_ok/.on_cancel`, `.close_button(bool)`, `.margin_top/.w/.width/.max_w`, `.overlay(bool)/.overlay_closable(bool)/.keyboard(bool)` (`dialog.rs:263-425`). `DialogButtonProps::ok_text/.ok_variant/.cancel_text/.cancel_variant/.show_cancel/.on_ok/.on_cancel` (`dialog.rs:52-134`). `AlertDialog::new(cx).confirm()/.trigger(...)/.content(...)/.icon/.title/.description/.button_props/.width/.show_cancel/.close_button/.keyboard/.on_close/.on_ok/.on_cancel` (`alert_dialog.rs:74-266`).

3. **Styling**: Extremely theme-driven — `Dialog::render` reads `cx.theme().tokens.background/.border/.radius_lg` for the popup surface (`dialog.rs:574-577`). It implements `Styled` (`:440-444`) and applies `.refine_style(&self.style)` on the popup `v_flex` (`:582`) — **but immediately after**, `.px_0()` unconditionally zeroes horizontal padding (`:583`, comment: *"There style is high priority, can't be overridden"*) and hardcoded `.absolute().relative().left(x).top(y).w(width)` position/size calls follow (`:585-591`) that also cannot be overridden by the caller's `StyleRefinement`. So bg/border/rounding are overridable, but layout geometry and horizontal padding are fixed by the component itself regardless of instance styling. `DialogContent/DialogHeader/DialogFooter/DialogTitle/DialogDescription` are thin `Styled` wrappers (each has its own `StyleRefinement` applied via `.refine_style`) around theme colors (`content.rs:40`, `footer.rs:50`, `title.rs:42-45`, `description.rs:49-50`).

4. **Keyboard/focus**: `Dialog` owns a `FocusHandle` (`dialog.rs:250,268`) wired into the base host via `.focus_handle(...)` and `.close_on_escape(self.props.keyboard)` (`:546-547`); `Confirm`/`Cancel` actions are re-exported from `gpui_base::actions` (`dialog.rs:22`) and dispatched by the OK/Cancel buttons (`:115-116,131`). Actual focus-trap/tab cycling lives in `gpui_base::Dialog` (out of scope).

5. **Virtualization**: None (dialogs aren't lists); body content scrolls via `.overflow_y_scrollbar()` (`dialog.rs:629`), not virtualized.

6. **Animation**: Imports `crate::animation::cubic_bezier` (`dialog.rs:14`). Uses a custom curve `cubic_bezier(1./3., 0.72, 2./3., 1.)` chosen specifically because those x-control-points collapse the bezier's time-remap to the identity (comment at `:518-521`, verified against a dedicated unit test in the underlying crate — `cubic_bezier_with_thirds_x_maps_time_identically`). Drives two simultaneous `.with_animation(...)` calls: a "slide-down" `top(y*delta)` + growing double-`BoxShadow` on the popup (`:660-683`), and a "fade-in" `opacity(delta)` on the backdrop layer (`:687`).

7. **VERDICT: WRAP.** The modal stack, focus trap, backdrop/escape/outside-click dismissal and layered z-index handling (`gpui_base::Dialog`) are substantial infrastructure worth keeping; but the *popup shell itself* (a plain `v_flex` with theme bg/border/radius) is trivial to reproduce with chamfered/bevel visuals, and since geometry/padding are hardcoded past the refinement point anyway, plan to supply your own popup shell inside `.content(...)`/`.popup(...)` rather than fighting `refine_style` precedence.

---

## 24. `sheet.rs` — slide-in drawer

1. **What it is**: `Sheet`, a side-anchored (`Placement::{Top,Right,Bottom,Left}`) slide-in panel, built on `gpui_base::Sheet` (focus/overlay/dismiss host, out of scope) the same way `Dialog` wraps `gpui_base::Dialog`.

2. **Key public API**: `Sheet::new(window, cx)`, `.title(...)/.footer(...)`, `.size(impl Into<DefiniteLength>)` (default 350px), `.resizable(bool)`, `.overlay(bool)/.overlay_closable(bool)`, `.on_close(...)` (`sheet.rs:57-120`).

3. **Styling**: Reads `cx.theme().sheet.margin_top` (top offset under the title bar, `:144`), `cx.theme().tokens.background/.border` for the surface (`:171-172`). Implements `Styled`, applied via `.refine_style(&self.style)` on the surface `v_flex` (`:174`) — here the size/placement calls (`.w(self.size)`/`.top(top).right_0()...`) come *after* refine_style in the builder chain but are conditioned on `self.placement`/`self.size` fields rather than raw style properties, so a caller's own explicit `.w()`/`.h()` etc. on the `Sheet` (via `Styled`) would still be overwritten by the placement `.map()` block at `:175-188`. Padding fields (`self.style.padding.*`) ARE read back out and honored for the body's `pl`/`pr` (`:148-159,215-216`), unlike Dialog's hardcoded `.px_0()`.

4. **Keyboard/focus**: `Sheet` owns a `FocusHandle` (`:43,61`) wired into `BaseSheet::focus_handle(...)` (`:253`); its close button dispatches `gpui_base::actions::Cancel` (`:205`), handled by the base host.

5. **Virtualization**: None; body is `.overflow_y_scrollbar()` (`:214`).

6. **Animation**: Uses raw `Animation::new(Duration::from_secs_f64(0.15))` with **no custom easing** (default engine easing, not `cubic_bezier`) driving a simple slide-in via `top`/`right`/`bottom`/`left` offset from `px(-100.)` to `0` (`:231-243`) — notably this file does *not* import `crate::animation`, unlike dialog/notification.

7. **VERDICT: WRAP.** Same reasoning as Dialog: keep the underlying overlay/focus/dismiss host (`gpui_base::Sheet`), replace the plain themed surface with a custom bevel/chamfer panel — the surface is a small, self-contained `v_flex` that's easy to swap out entirely.

---

## 25. `kbd.rs` — keyboard shortcut display

1. **What it is**: `Kbd`, a small tag rendering a single `Keystroke` in platform-native notation (⌘/⌃/⌥/⇧ on macOS, `Ctrl+`/`Alt+`/... elsewhere), plus static helpers to resolve the active keybinding for an `Action`.

2. **Key public API**: `Kbd::new(stroke: Keystroke)`, `.appearance(bool)` (chip vs. bare text), `.outline()`; statics `Kbd::binding_for_action(action, context, window)`, `Kbd::binding_for_action_in(action, focus_handle, window)`, `Kbd::format(&Keystroke) -> String` (`kbd.rs:28-209`) — these statics are reused by `menu/popup_menu.rs:1105,1109` and `command/state.rs:716-717` to render inline keybinding hints.

3. **Styling**: Reads `cx.theme().muted_foreground/.tokens.muted/.border/.tokens.background/.radius.half()` for the chip look (`kbd.rs:224-235`). Implements `Styled`, applied via `.refine_style(&self.style)` (`:240`) *before* `.child(...)` — so the chip's own base classes are set first and the instance `StyleRefinement` is layered on top last, meaning per-instance override generally wins for anything GPUI's style-merge doesn't special-case. `.appearance(false)` bypasses all chip styling and returns bare text (`:219-221`).

4. **Keyboard/focus**: N/A (display-only), but its binding-resolution statics *are* how the rest of the crate discovers/display keybindings — worth reusing even if the tag visuals are rebuilt.

5. **Virtualization**: N/A.

6. **VERDICT: REUSE BEHAVIOUR BUT RESTYLE.** The `binding_for_action`/`format` platform-notation logic is exactly the kind of correctness-sensitive plumbing (macOS symbol ordering, key-name mapping) worth keeping; the chip visual is trivial and fully overridable via `Styled`, or bypass it with `.appearance(false)` and wrap `Kbd::format(...)` in your own bevel-styled tag.

---

## 26. `skeleton.rs` — loading skeleton

1. **What it is**: `Skeleton`, a pulsing placeholder bar.

2. **Key public API**: `Skeleton::new()`, `.secondary()` (dimmer variant); implements `Styled` for size/shape overrides (`skeleton.rs:9-36`).

3. **Styling**: `cx.theme().skeleton` (or `.opacity(0.5)` for secondary) is the *only* color source (`:43-47`); `.refine_style(&self.style)` is applied right after, so instance overrides (e.g. `.bg(...)`, `.rounded(...)`) win over the theme default per normal GPUI style-merge semantics (`:48`). Default shape is `w_full().h_4()` (`:41-42`).

4. **Keyboard/focus**: N/A.

5. **Virtualization**: N/A.

6. **Animation**: `Animation::new(Duration::from_secs(2)).repeat().with_easing(bounce(ease_in_out))` (gpui built-ins, not `crate::animation`) driving `opacity` between 1.0 and 0.5 (`:49-58`).

7. **VERDICT: REUSE AS-IS.** It's a two-line opacity pulse with full `Styled` override and a theme-color hook — cheap to keep, cheap to reskin if ever needed (swap `cx.theme().skeleton` usage sites or override via `.bg()`), no behavior worth rebuilding.

---

## 27. `spinner.rs` — loading spinner

1. **What it is**: `Spinner`, a continuously-rotating icon.

2. **Key public API**: `Spinner::new()`, `.icon(impl Into<Icon>)` (default `IconName::Loader`), `.color(Hsla)`, `.ease(impl Fn(f32)->f32)`, and `Sizable::with_size` (`spinner.rs:18-58`).

3. **Styling**: No direct `cx.theme()` read in this file — the `Icon` itself carries theme-color defaults, and `Spinner::color()` overrides it explicitly with a caller-supplied `Hsla` (`:41-44,66`). No `Styled` impl on `Spinner` itself (it's a thin `div().child(icon...)` wrapper), so layout/position must be styled on a parent `div`.

4. **Keyboard/focus**: N/A.

5. **Virtualization**: N/A.

6. **Animation**: `Animation::new(self.speed).repeat().with_easing(self.easing)` (gpui built-ins; default `ease_in_out`) driving `Transformation::rotate(percentage(delta))` (`:67-70`) — fully configurable speed/easing/icon/color per instance.

7. **VERDICT: REUSE AS-IS.** Fully parameterized (icon, color, speed, easing) with no hardcoded chrome to fight; trivially swaps to a custom icon for a bevel-styled loading indicator.

---

## 28. `switch.rs` — toggle switch

1. **What it is**: `Switch`, a toggle built on `gpui_base::{Switch as BaseSwitch, SwitchTrack, SwitchThumb}` with a spring-animated thumb.

2. **Key public API**: `Switch::new(id)`, `.checked(bool)`, `.label(impl Into<Text>)`, `.accessibility_label(...)`, `.on_click(Fn(&bool,...))`, `.color(Hsla)` (track-checked color override), `.tooltip(...)`, plus `Sizable`/`Disableable` (`switch.rs:35-118`).

3. **Styling**: Reads `cx.theme().tokens.primary` (checked bg default, overridable via `.color()`), `.tokens.switch` (unchecked bg), `.tokens.switch_thumb` (thumb color), `.radius` (`:129-150`). `Switch` itself has **no `Styled` impl** — it wraps everything in a bare `div().refine_style(&self.style)`... actually re-checking: `Switch` doesn't derive `Styled`; the outer `div()` at `:169` has no refinement source from `self.style` because `Switch` has no `style` field — sizing/geometry (track width/height, thumb size, inset) are fully hardcoded per `Size` variant (`:137-145`), not exposed as builder methods or `Styled` overrides. Color is the only override surface (`.color()`).

4. **Keyboard/focus**: Delegated to `gpui_base::Switch` (`BaseSwitch`) — test coverage confirms Tab focus, Enter/Space activation, and disabled-inert behavior (`switch.rs:330-359`).

5. **Virtualization**: N/A.

6. **Animation (Spring, not `gpui_base::animation`)**: Imports `gpui_base::{Spring, spring}` (`switch.rs:9`) — a **sibling** physics module to `gpui_base::animation` (the `EffectTransition`/`cubic_bezier` doc-comments explicitly distinguish "motion" spring-based state transitions from keyframe `Animation`/`cubic_bezier` transitions). `THUMB_SPRING = Spring::new(Duration::from_millis(180)).with_epsilon(0.1)` — critically damped so the thumb can't overshoot the track (`:16`). `thumb_x = spring((id,"thumb"), target_x, THUMB_SPRING, window, cx)` (`:157-167`) drives the thumb's `left` position continuously, reversing smoothly mid-travel if toggled again before settling — this is real per-frame state-driven physics, not a fixed-duration keyframe tween.

7. **VERDICT: REUSE BEHAVIOUR BUT RESTYLE.** The spring-physics thumb travel (reversible mid-flight, critically damped) is genuinely valuable and non-trivial to reproduce; but geometry is fully hardcoded (no way to get a chamfered/bevel track shape without touching source), so keep `gpui_base::Spring`/the toggle interaction model and re-skin the track/thumb draw calls.

---

## 29. `slider.rs` — slider

1. **What it is**: `Slider`, built on `gpui_base::{Slider as BaseSlider, SliderTrack, SliderThumb, SliderIndicator, SliderState}`, with a spring-animated hover/press "ring" per thumb.

2. **Key public API**: `Slider::new(&state: &Entity<SliderState>)`, `.horizontal()/.vertical()`, `.disabled(bool)`, `.reverse()` (fill-from-thumb-to-max for remaining-amount displays) (`slider.rs:105-148`). State type `SliderState`/`SliderValue`/`SliderScale`/`SliderEvent` re-exported from `gpui_base::slider` (`:4`).

3. **Styling**: Implements `Styled`, and unusually *reads back* the caller's own `StyleRefinement` fields to derive colors/corners: `bar_color` from `self.style.background` if set, else `cx.theme().tokens.slider_bar` (`:170-175`); `thumb_bg` from `self.style.text.color` if set, else `cx.theme().tokens.slider_thumb` (`:176-181`); `corner_radii` from `self.style.corner_radii` if set, else `cx.theme().radius_full()` (`:182-203`). This is the most instance-overridable component in the batch — `.bg(color)`/`.text_color(color)`/`.rounded(...)` on the `Slider` builder directly retarget the bar/thumb/track visuals without needing to touch source.

4. **Keyboard/focus**: Delegated to `gpui_base::Slider`/`SliderThumb` (out of scope); drag interaction (mouse down/up, hover) is handled locally for the ring effect only (`:231-247`).

5. **Virtualization**: N/A.

6. **Animation (Spring)**: Same `gpui_base::{Spring, spring}` primitive as `switch.rs` (`slider.rs:5`). `THUMB_RING_SPRING = Spring::new(Duration::from_millis(150))` grows/shrinks a translucent ring around the thumb on hover/press, decelerating through reversal so a fast click-release doesn't glitch (`:14-23,49-79`).

7. **VERDICT: REUSE BEHAVIOUR BUT RESTYLE (best candidate to REUSE AS-IS among the styled controls).** The color/corner-radius override hooks already read the caller's `StyleRefinement`, so a chamfered bevel-styled slider thumb/track is achievable via builder calls alone (custom `bg`, `text_color`, `corner_radii`) without forking source — worth trying as REUSE AS-IS first before falling back to restyle.

---

## 30. `setting/` — settings-page framework

**Files**: `setting/mod.rs`, `page.rs`, `group.rs`, `item.rs`, `settings.rs`, `fields/{mod,bool,dropdown,number,string,element}.rs`.

1. **What it is**: A declarative settings-page builder: `Settings` (sidebar + pages) → `SettingPage` (virtualized list of groups) → `SettingGroup` (a `GroupBox`) → `SettingItem` (title/description/field row, or a fully custom element) → typed `SettingField<T>` (switch/checkbox/input/dropdown/number/custom-element) bound to get/set closures.

2. **Key public API**:
   - `Settings::new(id)`, `.page(SettingPage)/.pages(...)`, `.sidebar_width/.sidebar_size_range/.sidebar_style`, `.with_group_variant(GroupBoxVariant)`, `.default_selected_index(SelectIndex)`, `.header_style(...)`, `Sizable::with_size` (`settings.rs:46-246`).
   - `SettingPage::new(title)`, `.title_suffix(|window,cx| elem)`, `.icon/.description/.default_open/.resettable(bool)`, `.group(SettingGroup)/.groups(...)`, `.header_style(...)` (`page.rs:33-109`).
   - `SettingGroup::new()`, `.title/.description`, `.item(SettingItem)/.items(...)`, implements `Styled` (`group.rs:24-66`).
   - `SettingItem::new(title, field: impl AnySettingField)` or `SettingItem::render(|options,window,cx| elem)` for a fully custom row, `.on_reset(is_dirty, reset)`, `.keywords([...])`, `.disabled(bool)`, `.description(...)`, `.layout(Axis)` (`item.rs:44-163`).
   - `SettingField<bool>::switch/::checkbox`, `SettingField<SharedString>::input/::dropdown/::scrollable_dropdown/::element/::render`, `SettingField<f64>::number_input`, all with `.default_value(...)` (enables the reset button) and `.on_reset(is_dirty, reset)` (`fields/mod.rs:154-317`).

3. **Styling**: `SettingPage::render` reads `cx.theme().border` for the header divider (`page.rs:168`); `SettingGroup::render` wraps items in `GroupBox` (out-of-scope component) and reads `cx.theme().muted_foreground` for the description (`group.rs:94`); `SettingItem::render_item` reads `cx.theme().muted_foreground` for description text (`item.rs:300`). `SettingPage`/`SettingGroup` both expose `header_style`/`style` `StyleRefinement` hooks applied via `.refine_style(...)` (`page.rs:169`, `group.rs:110`), so header/group chrome is overridable; individual field rendering (`fields/bool.rs` etc.) delegates entirely to other in-crate components (`Switch`, `Checkbox`, dropdown `Button`+`PopupMenuItem`, number/string `Input`) whose own styling rules apply (see `Switch`/`Slider` sections above; `.refine_style(style)` is threaded through from `SettingField::style()` at `fields/bool.rs:37`).

4. **Keyboard/focus**: Not implemented directly in this module (delegates to `Input`/`Switch`/`Checkbox`/dropdown `Button` for their own focus/keyboard handling); the settings search field is a normal `InputState` (`settings.rs:366-371`).

5. **Virtualization**: **Yes**, at the group level — `SettingPage::render` builds a `gpui::list(list_state, |group_ix, window, cx| ...)` (`page.rs:212-227`) keyed with `ListState::new(groups_count, ListAlignment::Top, px(100.))` (`:143`), so only visible groups within a page are rendered/laid out; items *within* a visible group are not separately virtualized.

6. **VERDICT: REUSE BEHAVIOUR BUT RESTYLE.** The page/group/item/field composition, search-filtering (`is_match`), reset-button dirty-tracking, and per-page virtualization are a lot of well-thought-out plumbing (see the `RenderOptions` threading pattern) worth keeping wholesale for a settings surface; visuals ride on `GroupBox`/`Label`/`Switch`/`Input` theme tokens throughout, so a heavy custom look means re-skinning those underlying atoms (which is anyway required project-wide) rather than this orchestration layer.

---

## 31. `title_bar.rs` — custom window title bar

1. **What it is**: `TitleBar`, a custom-drawn title bar (drag region, traffic-light padding on macOS, min/max/close buttons on Windows/Linux under client-side decorations).

2. **Key public API**: `TitleBar::new()`, `.on_close_window(...)` (Linux-only close override), statics `TitleBar::title_bar_options() -> TitlebarOptions` and `TitleBar::window_options() -> WindowOptions` (use these as the base `WindowOptions` for any window hosting this title bar, since it sets `app_owns_titlebar_drag: true`) (`title_bar.rs:50-104`). `pub const TITLE_BAR_HEIGHT: Pixels = px(34.)` (`:16`) is referenced by `Dialog`, `Sheet`, and `Notification` margin defaults elsewhere in the crate.

3. **Styling**: Reads `cx.theme().title_bar_border`, and blends `cx.theme().title_bar`/`.background` into a `linear_gradient` for the bar background via `default_title_bar_background()` (`:22-38,339-343`); `ControlIcon` (min/max/close buttons) reads `cx.theme().foreground/.danger/.danger_foreground/.secondary_foreground/.secondary_hover/.secondary_active/.danger_active` (`:169-193,218`). `TitleBar` implements `Styled`, applied via `.refine_style(&self.style)` on the outer bar `div` (`:344`) — so background/border/height overrides are achievable per instance (the gradient default can be replaced by an instance `.bg(...)` call since refine_style is applied after the hardcoded bg).

4. **Keyboard/focus**: N/A (it's chrome, not a focusable control); window-move/drag is handled via `WindowControlArea::Drag` + manual mouse-down/move tracking (`:351-371`), and control buttons use `WindowControlArea::{Min,Max,Close}` on Windows or explicit click handlers on Linux (`:156-243`).

5. **Virtualization**: N/A.

6. **VERDICT: REUSE BEHAVIOUR BUT RESTYLE.** The platform-conditional logic (macOS traffic-light padding vs. Windows fixed-width native-feeling controls vs. Linux client-decoration detection, drag-vs-click disambiguation) is exactly the kind of cross-platform correctness code worth keeping; the visual (gradient bar + icon buttons) is straightforward to re-skin via the `Styled` hook and by supplying custom child content, since children are fully composable (`.children(self.children)`, `:399`).

---

## 32. `window_border.rs` — custom window border/chrome

Exported from `lib.rs` as `WindowBorder`/`window_border()`/`window_paddings()` per the task description; confirmed present at crate root re-export (not re-verified line number since out of my file list, but the module itself:

1. **What it is**: Linux-only (mostly) client-side-decoration chrome: draws the outer shadow, 1px inner border, rounded top corners, and resize-hit-testing zones around a window; ported from a Zed example (`window_border.rs:1-2`).

2. **Key public API**: `window_border() -> WindowBorder` / `WindowBorder::new()`, `.shadow_size(impl Into<Pixels>)`, `.resize_hit_size(impl Into<Pixels>)` (`:25-66`); free function `window_paddings(window: &Window) -> Edges<Pixels>` used by `Dialog`/`Sheet` to inset their anchored positioning from this border (`:88-94`, consumed at `dialog.rs:492`, `sheet.rs:138`).

3. **Styling**: Reads `cx.theme().is_dark()` to pick a fixed light/dark border color constant (not a theme token — hardcoded `hsla(0.,0.,0.2,1.)`/`hsla(0.,0.,0.8,1.)`, `:124-128`). `WindowBorder` has **no `Styled` impl** — shadow size and resize-hit size are the only two builder-configurable values; border thickness (`BORDER_SIZE = 1px`), radius (`BORDER_RADIUS = 0px`, explicitly kept square because GPUI can't clip a rounded content mask yet, `:16-22`), and shadow blur/spread are compile-time constants.

4. **Keyboard/focus**: N/A; it does implement mouse-based edge/corner resize hit-testing (`resize_edge`, `resize_hit_zones`, `:236-403`) and per-edge cursor styling.

5. **Virtualization**: N/A.

6. **VERDICT: REUSE BEHAVIOUR BUT RESTYLE.** The tiling-aware inset math and resize-edge hit-testing (respecting which edges are already tiled by the WM) is intricate platform-integration code worth keeping; the visual (plain 1px border + soft shadow, explicitly square-cornered due to a GPUI clipping limitation noted in the source) is the opposite of a chamfered/bevel aesthetic and has no override hook, so a "cut" window frame would need new code here regardless — but keep `window_paddings()`/the resize-hit logic and swap only the drawn border/shadow.

---

## Cross-cutting: `crate::animation` / `gpui_base::animation` usage summary

`vendor/gpui_ce_components/src/lib.rs:91` re-exports the whole module: `pub use gpui_base::animation;`, so `crate::animation` **is** `gpui_base::animation` (source at `~/.cargo/registry/.../gpui_ce_components_base-0.2.0/src/animation.rs`, not vendored in-repo). It provides:
- `cubic_bezier(x1,y1,x2,y2) -> impl Fn(f32)->f32` — CSS-style bezier easing (registry source lines 9-63), used by:
  - `dialog.rs:14,522-527` — `cubic_bezier(1./3., 0.72, 2./3., 1.)` for the dialog's slide+fade+shadow entrance.
  - `notification.rs:17,483` — `cubic_bezier(0.25, 0.1, 0.25, 1.)` (CSS `ease`) for toast slide/opacity/shadow.
- `ease_out_cubic`/`ease_in_cubic`/`ease_in_out_cubic` presets and a `Lerp` trait (f32/Pixels/Point/Hsla) (registry source lines 67-132).
- `EffectTransition` (formerly `Transition`) — a composable `.slide_x/.slide_y/.fade/.width/.height` builder that produces a ready `AnimationElement` via `.apply(element, id)` (registry source lines 152-247) — **not used by any file in my scope**; `dialog.rs`/`notification.rs`/`sheet.rs` instead hand-roll their own `.with_animation(...)` closures directly rather than using this combinator.

Separately, `Spring`/`spring(...)` (used by `switch.rs:9,157-167` and `slider.rs:5,58-68`) is a **different, sibling** physics-based motion primitive in `gpui_base` (the `animation.rs` doc-comments explicitly distinguish it from `crate::motion::Transition`/`EffectTransition`) — critically-damped spring interpolation toward a live target, reversible mid-flight, as opposed to the fixed-duration keyframe `Animation`/`cubic_bezier` approach used by Dialog/Notification/Sheet/Skeleton/Spinner. Both are relevant prior art for the "heavy animation" requirement: **keyframe+bezier** for one-shot enter/exit transitions (dialogs, sheets, toasts), **spring** for continuous state-tracking motion (toggle thumbs, hover rings) — worth reusing `gpui_base::animation`'s bezier solver and the `Spring` primitive directly even where the surrounding component is being restyled or rebuilt, since both are self-contained math with no visual assumptions baked in.


---

## 33. Group D — remaining leaf and complex components (button/, chart/, plot/, combobox.rs, select.rs, color_picker.rs, form/, stepper/, time/, sidebar/, status_bar.rs, table/, and all simple leaf components)

Full inventory of `vendor/gpui_ce_components/src/` against the plan for a custom-styled desktop docs-reader with chamfered "cut" cards, two-tone bevel-border focus/state channels, custom fonts, and heavy animation. Compiled from direct reading (leaf components, `styled.rs`, `component_traits.rs`, spot-checks) plus five parallel deep-dive passes over the complex clusters (button/, chart/, plot/, table/+sidebar/+status_bar.rs, combobox/select/form/color_picker/stepper/time). All citations are `file:line`.

### Cross-cutting foundations (read first)

**`component_traits.rs`** (2 lines): re-exports `Collapsible`, `Disableable`, `Selectable` from the separate `gpui_base` crate as this crate's own trait names — a shared checked/disabled/selected vocabulary every component below implements instead of ad hoc booleans.

**`styled.rs`** (272 lines): defines `ThemeStyled`, a blanket-impl extension trait over anything implementing gpui's `Styled`, giving `.focus_ring_style(window, cx)` (`styled.rs:151-261`, draws the shared focus ring), `.popover_style(cx)` (`styled.rs:263-271`, the **one shared popup skin** — `bg(theme.popover)`, `text_color(theme.popover_foreground)`, `popover_shadow(...)`, `.rounded(theme.radius)` — used by Popover, PopupMenu, Select, Combobox, DatePicker, ColorPicker), and `.rounded_full_style(cx)` (`styled.rs:169-171`, rounds to a circle/pill unless `Theme::radius_full()` is zero). These are ordinary setter calls, not a separate rendering path, so a caller's own later `.bg()/.border()/.rounded()` always wins through gpui's `refine_style` composition — nothing here blocks per-instance override. **No chamfer/clip-path/polygon primitive exists anywhere in this vendor tree** (confirmed by grep across the whole crate) — every corner call, in every component below, is a plain rounded-rect radius; a "cut" corner always requires new custom-painted geometry, not a style-property change.

**`native_menu/`**: OS-native **popup/context** menu integration (not the app's top menu bar) — real AppKit `NSMenu` on macOS (`native_menu/macos.rs`), Win32 popup menus on Windows (`native_menu/windows.rs`), and a GPUI-drawn `PopupMenu` fallback elsewhere (`native_menu/fallback.rs`), unified because a GPUI-drawn popup is clipped to the window while a native one can extend beyond it (`native_menu/mod.rs:1-21`).

---

### Simple leaf components (compact, read directly)

**`accordion.rs`** (557 lines) — vertically stacked expand/collapse items, the only leaf component with **real custom-painted animation**: `AnimatedAccordionPanel` (`accordion.rs:170-291`) is a hand-built `Element` that measures natural height in `prepaint` and animates it via a critically-damped `Spring` (`PANEL_SPRING`, `accordion.rs:22`), re-requesting animation frames as needed. Constructors: `Accordion::new(id)` + `.multiple()/.bordered()/.disabled()/.item(closure)/.on_toggle_click()`; `AccordionItem::new()` + `.icon()/.title()/.open()/.title_style()/.hover()/.content_style()`. Reads `cx.theme().border`, `.radius_lg` (`accordion.rs:119`, only on the bordered variant), `.tokens.accordion` (item bg, `accordion.rs:506`), `.foreground`/`.muted_foreground`; fully `Styled`/`.refine_style()` overridable per item. No dedicated keyboard nav beyond the base trigger's click/`on_change`. **VERDICT: REUSE BEHAVIOUR BUT RESTYLE** — the spring-height-animation machinery is a genuine asset worth keeping regardless of skin; only one `.rounded(radius_lg)` call fights a chamfer.

**`alert.rs`** (255 lines) — inline banner (Default/Info/Success/Warning/Error variants), icon, optional title, closable. `Alert::new(id, message)` + `.info()/.success()/.warning()/.error()/.icon()/.title()/.banner()/.visible()/.on_close()`. Per-variant fg/bg/border resolved via `AlertVariant::fg/bg/border_color` (`alert.rs:26-54`) using `.mix_oklab()` tinting off theme semantic colors; radius `cx.theme().radius`/`radius_lg` by size (`alert.rs:179-184`), skipped entirely in `.banner()` mode. Fully overridable via `.refine_style()`. Non-interactive except a plain clickable close icon. **VERDICT: REUSE BEHAVIOUR BUT RESTYLE** — variant-color logic reusable; single radius call is the only conflict.

**`avatar/avatar.rs`** (174) + **`avatar/avatar_group.rs`** (131) — circular user avatar (image / initials-fallback / icon-placeholder) and an overlapping-stack group with a "+N" overflow avatar. `Avatar::new()` + `.src()/.name()` (auto-derives 1-2 letter initials, `avatar.rs:137-150`)/`.placeholder()`; `AvatarGroup::new()` + `.child()/.children()/.limit()/.ellipsis()`. **Both hardcode `.rounded_full_style(cx)`** (`avatar.rs:100,118,129`) baked directly into `render()` — the single biggest shape conflict in the leaf set, since an avatar's roundness *is* its visual identity; initials-fallback background is a hashed hue off `cx.theme().blue` (`avatar.rs:87-90`); border reads `cx.theme().border`; group overflow avatar bg reads `cx.theme().tokens.secondary`. No keyboard/focus. **VERDICT: WRAP** — initials/hash-color/overflow logic is reusable, but a "cut" avatar needs the `rounded_full_style()` calls replaced with custom polygon geometry.

**`badge.rs`** (165) — small count/dot/icon badge anchored to a corner of its children. `Badge::new()` + `.dot()/.count(usize)/.icon()/.max()/.color()`. The badge shape itself is `.rounded_full_style(cx)` (`badge.rs:124`) — same hardcoded pill/circle as Avatar; bg defaults to `cx.theme().red`, overridable via `.color()`. **VERDICT: WRAP** — same shape conflict as Avatar, otherwise trivial.

**`breadcrumb.rs`** (178) — see button/nav cluster below (covered together with `link.rs`/`pagination.rs`).

**`checkbox.rs`** (499) — checked/unchecked checkbox with label, **spring-animated checkmark fade** (`MARK_SPRING`, `checkbox.rs:20`, applied in `checkbox_check_icon`, `checkbox.rs:173-212`, shared with `radio.rs`). `Checkbox::new(id)` + `.label()/.accessibility_label()/.checked()/.on_click()/.tab_stop()/.tab_index()/.role()/.tooltip()`. Reads `cx.theme().input` (unchecked border), `.primary`/`.tokens.primary` (checked), `.muted_foreground` (disabled text); box radius `cx.theme().radius.min(px(4.))` (`checkbox.rs:237`) — a small capped radius, but square, not circular, so cheaper to chamfer than Avatar/Badge/Radio. Full keyboard: `track_focus`, `tab_stop/tab_index`, `focus_ring_style`; tests prove Enter/Space activation and correct pointer-vs-keyboard/disabled semantics (`checkbox.rs:442-472`). Fully overridable via `.refine_style()`, including inside `.styles(|s| s.disabled(...))` pseudo-state blocks (`checkbox.rs:243-249`). **VERDICT: REUSE BEHAVIOUR BUT RESTYLE** — strong keyboard+animation model; low-cost chamfer since the box is already square.

**`clipboard.rs`** (125) — copy-to-clipboard `Button` wrapper that swaps its icon to a checkmark for 2 seconds after copying (`clipboard.rs:77-117`, timer via `cx.spawn`+`cx.background_executor().timer`). No direct theme reads — 100% delegates chrome to `Button::ghost().xsmall()`. **VERDICT: REUSE AS-IS (behaviorally)** — free once `Button` itself is restyled.

**`collapsible.rs`** (94) — minimal show/hide container, no built-in trigger visuals and **no height animation** (instant toggle, unlike Accordion). `Collapsible::new()` + `.open(bool)/.content(impl IntoElement)`. Zero theme reads — pure `v_flex()` + `.refine_style()`. **VERDICT: REUSE AS-IS** — no visual opinion to fight; pair with Accordion's spring machinery if animated collapse is wanted here too.

**`description_list.rs`** (384) — label/value grid (horizontal or vertical), column-spanning items, separators. `DescriptionList::new()/.horizontal()/.vertical()` + `.label_width()/.layout()/.bordered()/.columns()/.item(label,value,span)/.separator()`. Reads `cx.theme().radius`, `.border`, `.description_list_label_foreground`, `.tokens.description_list_label`; inter-cell borders use per-edge calls (`border_r_1`/`border_l_1`/`border_b_1`) but always the same uniform `Hsla`, never two-tone. **VERDICT: REUSE BEHAVIOUR BUT RESTYLE** — grid/spanning logic useful for a metadata panel; one outer `.rounded(radius)` and the uniform border color are the only frictions.

**`group_box.rs`** (191) — titled container (Normal/Fill/Outline variants). `GroupBox::new()` + `.id()/.title()/.title_style()/.content_style()`, `GroupBoxVariants::normal()/.fill()/.outline()`. Fill bg = `cx.theme().tokens.group_box` (`group_box.rs:134`); Outline border = `cx.theme().border`; inner box `.rounded(cx.theme().radius)` (`group_box.rs:160`). **VERDICT: REUSE BEHAVIOUR BUT RESTYLE** — structurally a good "cut card" candidate (title + bordered/filled box); one radius call to fork.

**`icon.rs`** (200) — SVG icon wrapper (size/rotation/color) over a generated `IconName` enum of bundled assets. `Icon::new(impl Into<Icon>)` + `.path()/.rotate()/.transform()/.view(cx)`, `Sizable::with_size()`. Color falls back to ambient `window.text_style().color`/`cx.theme().foreground` if unset (`icon.rs:149,178`); no corner/border concept exists at all. **VERDICT: REUSE AS-IS** — pure glyph primitive, no chrome to conflict with anything.

**`label.rs`** (396) — text label with optional muted secondary text, password-style masking, and substring/prefix highlighting (`highlight_ranges`, `label.rs:100-147`, extensively unit-tested including Unicode). `Label::new(text)` + `.secondary()/.masked(bool)/.highlights(HighlightsMatch)`. Reads `cx.theme().foreground`/`.muted_foreground`; highlight color is **hardcoded to `cx.theme().blue`** with no override method (`label.rs:178`). No box chrome at all — just `StyledText`, so zero chamfer conflicts. **VERDICT: REUSE AS-IS** — the fixed highlight-blue is the only restyle item, trivial to patch.

**`link.rs`** (190) / **`breadcrumb.rs`** (178) / **`pagination.rs`** (242) — from the button/nav agent's pass: `Link` colors text via `cx.theme().link` with hover/active opacity steps (`link.rs:77-89`) and has **no box chrome to conflict with a chamfer**, but is proven **pointer-only with no keyboard focus at all** (`link.rs:182-189` test; `.disabled()` is dead code per `link.rs:172-180`). `Breadcrumb`/`BreadcrumbItem` similarly color text via `muted_foreground`/`foreground` (`breadcrumb.rs:101-105`) with a `ChevronRight` separator, and also appear to lack keyboard focus (plain `div().on_click()`, no `track_focus`). `Pagination` is a thin composition of `Button`s (prev/next/page-number/ellipsis-with-popup-menu) with **zero direct theme reads of its own** — every color/radius/focus/keyboard behavior is 100% inherited from `Button`. **VERDICT: `Link`/`Breadcrumb` = WRAP** (need keyboard focus/activation added before use in a keyboard-navigable docs reader); **`Pagination` = REUSE BEHAVIOUR BUT RESTYLE** (page-window/ellipsis logic is useful, but every pixel is a `Button`, so it inherits Button's full restyle cost).

**`progress/mod.rs`, `progress/progress.rs`, `progress/progress_circle.rs`** — linear bar + circular ring, both determinate and indeterminate. `Progress::new(id)` + `.loading(bool)/.color()/.value(f32 0-100)/.accessibility_label()`; `ProgressCircle::new(id)` — same API plus a children slot. Linear bg defaults `cx.theme().tokens.progress_bar`; pill radius auto-squares when `cx.theme().radius.is_zero()` (`progress.rs:105-109`) but is otherwise a hardcoded half-height pill. Circle paints via a raw `canvas()` + `plot::shape::Arc` (`progress_circle.rs:85-142`) — same rounded-arc-only limitation as `chart`/`plot`'s `Arc`. **Both use real `gpui::Animation`/`with_animation` + `ease_in_out` easing** for the indeterminate sweep and value-change tweening (`progress.rs:149-168`; `progress_circle.rs:210-234`) — genuine, already-built animation infrastructure worth reusing regardless of skin. **VERDICT: REUSE BEHAVIOUR BUT RESTYLE** — keep the animation/interpolation code; reshape the pill/arc geometry for the chamfer (linear bar is cheap to reshape, the stroked circular ring is harder).

**`radio.rs`** (431) — Radio + RadioGroup, same checked-glyph/spring pattern as Checkbox (shares `checkbox_check_icon`, `radio.rs:232`). `Radio::new(id)` + `.label()/.checked()/.on_click()/.tab_stop/.tab_index`; `RadioGroup::vertical(id)/.horizontal(id)` + `.child()/.children()/.selected_index()/.on_click()/.disabled()`. Reads `cx.theme().primary` (checked)/`.input` (unchecked); **the outer ring itself calls `.rounded_full_style(cx)`** (`radio.rs:224`) — same hardcoded-circle conflict as Avatar/Badge, this time on the interactive control's own hit target, not just a decoration. Full keyboard support identical to Checkbox (`radio.rs:126-135,186-187`, `track_focus`/`tab_stop`/`focus_ring_style`). **VERDICT: WRAP** — keyboard/state model is solid and reusable, but the radio dot's circular shape is baked into `render()` and needs replacing for a "cut" marker.

**`rating.rs`** (203) — star rating (click/hover to set 0..max). `Rating::new(id)` + `.with_size()/.disabled()/.color()/.value()/.max()/.on_click()`. Active-star color defaults to `cx.theme().yellow`, overridable via `.color()`; no radius/border/box chrome at all (pure `Icon` swap). Mouse-only, no keyboard nav. **VERDICT: REUSE AS-IS** — icon-only, nothing to fight visually; keyboard support would be a worthwhile addition, not a blocker.

**`select.rs`**, **`separator.rs`**, **`sidebar/`**, **`status_bar.rs`**, **`stepper/`**, **`table/`**, **`tag.rs`**, **`time/`** — see dedicated sections below.

**`separator.rs`** (155) — horizontal/vertical divider (solid or dashed via hand-built `PathBuilder`), optional centered label chip. `Separator::horizontal()/.vertical()/.horizontal_dashed()/.vertical_dashed()` + `.label()/.color()/.dashed()`. Default color `cx.theme().border`; label chip bg `cx.theme().tokens.background`, no rounding on the chip at all (blank slate). The dashed path (`separator.rs:90-117`) is real custom paint (`canvas()` + `PathBuilder::stroke().dash_array()`) — a good ready-made template for a two-tone bevel line effect since it's already hand-drawn geometry, not a CSS border. **VERDICT: REUSE AS-IS.**

**`tag.rs`** (270) — small status/category chip with the richest variant system of any leaf component (Primary/Secondary/Danger/Success/Warning/Info/`Color(ColorName)`/fully-`Custom{color,foreground,border}`), plus an outline mode. `Tag::new()/.primary()/.secondary()/.danger()/.success()/.warning()/.info()/.custom(color,fg,border)/.color(ColorName)` + `.outline()/.rounded(impl Into<AbsoluteLength>)/.rounded_full()`. Per-variant bg/border/fg resolved in `TagVariant::bg/border/fg` (`tag.rs:28-119`), heavily theme-token-driven with a Tailwind-style `ColorName::scale(950/800/300…)` option. Default radius is `cx.theme().radius/2` (small sizes) or full `radius` (`tag.rs:244-249`) — but **Tag already exposes a public per-instance radius override** (`.rounded()`/`.rounded_full()`), unlike almost everything else in this inventory. **VERDICT: REUSE BEHAVIOUR BUT RESTYLE** — cheapest chamfer win among leaf components: the variant-color machinery is reusable outright, and adding a `.chamfered()` builder method following the existing `.rounded_full()` pattern is a small, additive change rather than a fork.

---

### `button/` (complex — full pass)

*(button/button.rs, button/button_group.rs, button/button_icon.rs, button/dropdown_button.rs, button/toggle.rs, button/mod.rs)*

**Shared finding across the family**: every component captures the caller's own `StyleRefinement` (`instance_style = base.style().clone()`) and replays it **last**, via `.refine_style(&instance_style)` — including inside hover/active/selected/disabled/pressed pseudo-state blocks (`button.rs:722,785,793`; `toggle.rs:210,213`, proven by test `instance_style_remains_the_final_visual_override`, `toggle.rs:575-579`). Per-instance color/background/border/opacity overrides genuinely win, in every state. But: **no chamfer primitive exists** (every corner API — `rounded()`, `rounded_tl/tr/bl/br()`, `ButtonRounded`— is a curve radius, never an angled clip); **no per-edge border color** exists anywhere (always one uniform `Hsla` via a single `.border_color()` call, so a two-tone bevel isn't an existing style axis); and **zero animation/transition/easing code** exists in the whole family (confirmed by grep) — all hover/active/selected/pressed changes are instant GPUI pseudo-class swaps.

#### `button/button.rs` (1939 lines)

The workhorse `Button`: 11 variants (Default/Primary/Secondary/Danger/Info/Success/Warning/Ghost/Link/Text/Custom), 5 sizes, loading/disabled/selected/toggled states, tooltips, and instrumentation-only observer hooks for accessibility/screenshot harnesses.

- **Constructor/API**: `Button::new(id)` (`button.rs:240-282`, defaults `rounded: Medium`, all corners/edges `true`, `size: Medium`). Content: `.label()` (326), `.icon()` (361), `ParentElement` children (566-570). Accessibility: `.accessibility_label()` (349), `.accessibility_description()` (355), `.toggled()` (497, pure `aria-pressed`, independent of `.selected()`). Variant/state via `ButtonVariants` trait (`.primary()/.secondary()/.danger()/.warning()/.success()/.info()/.ghost()/.link()/.text()/.custom(ButtonCustomVariant)`, 45-97); `Disableable::disabled()` (517); `Selectable::selected()` (535); `.loading()`/`.loading_icon()` (390,464); `Sizable::with_size()` (546). Shape: `.outline()` (302); `.rounded(impl Into<ButtonRounded>)` (308, enum `Small/Medium/Large/Size(px)/None`) — an **inherent method that shadows `Styled::rounded()`**. `.border_corners()`/`.border_edges()` (314,320) are **`pub(crate)` only** — unreachable from a downstream app, used internally by `ButtonGroup`/`DropdownButton` to join segments.
- **Styling**: corner radius is theme-driven (`ButtonRounded::Small ⇒ radius*0.5, Medium ⇒ radius, Large ⇒ radius*2.0`, `button.rs:630-636`), applied per-corner (682-693, booleans only toggle whether that corner gets the *same* radius). Colors resolve per-variant/state through `bg_color()`/`text_color()`/`border_color()` (997-1109) and `normal()/hovered()/active()/selected()/disabled()` (1125-1391), all reading semantic theme tokens (e.g. `tokens.button_primary_hover`) — no hardcoded hex anywhere in these paths. Sizing (height/padding) is hardcoded pixel presets (`h_5/px_1`, `h_8/px_3`, etc., 653-680), overridable per instance via chained `Styled` calls that win through `refine_style`. Border color is one uniform `Hsla` (700-703).
- **Keyboard/focus**: `track_focus`, `.tab_index()/.tab_stop()` (809-811); focus ring via `focus_ring_style()` when focused (884-886). Tests prove Enter/Space→click (`button.rs:1466-1520`) and correct disabled/loading pointer+keyboard blocking (1423-1605).
- **Animation**: none — `.opacity(0.8)` on loading is a flat value, not a fade.
- **VERDICT: REUSE BEHAVIOUR BUT RESTYLE.** State machine, theming plumbing, accessibility, keyboard, and instance-override ordering are all solid. Chamfer/bevel/animation must be added on top; the one hook that would help (`border_corners`/`border_edges`) is crate-private, so per-corner shape control from outside the vendor tree is only reachable via GPUI's own `rounded_tl/tr/bl/br()` (curves only) unless forked.

#### `button/button_group.rs` (450) — joins `Button`s into a segmented bar, aggregates single/multi-select click state. No direct theme reads — delegates all color/radius to children; its own contribution is stripping shared borders/corners at segment boundaries via `Button`'s crate-internal `border_corners`/`border_edges` (182-229). **Nuance**: a keyboard-triggered (Enter) click fires the child's own `on_click` but **not** the group's aggregate `.on_click(&Vec<usize>)` (only pointer clicks reach it, test at `button_group.rs:372-378`). **VERDICT: REUSE BEHAVIOUR BUT RESTYLE** — inherits every one of Button's chamfer/bevel/animation gaps.

#### `button/button_icon.rs` (168) — internal (`#[doc(hidden)]`) content-slot dispatcher between `Icon`/`Spinner`/`ProgressCircle`, auto-swapping to a spinner when `loading` (116-131). No theme reads, no chrome. **VERDICT: REUSE AS-IS.**

#### `button/dropdown_button.rs` (261) — split button (action `Button` + caret `Button` opening a `PopupMenu`), visually merged unless `Ghost`+unselected (14-18,154-155). No direct theme reads; fuses inner edges via `Button`'s crate-internal corner/edge hooks (157-202). **VERDICT: REUSE BEHAVIOUR BUT RESTYLE** — 100% inherited `Button` chrome.

#### `button/toggle.rs` (619) — `Toggle` (pressable icon/label toggle) + `ToggleGroup` (segmented row). `Toggle::new(id)` + `.label()/.icon()/.checked(bool)/.on_click()`, `ToggleVariants::ghost()`(default)`/.outline()`; `ToggleGroup::new(id)` + `.child()/.children()/.on_click()/.segmented()`. Reads `cx.theme().radius` (always plain, no `ButtonRounded`-style scaling, `toggle.rs:154`), `tokens.accent`/`accent_foreground` for pressed state (155-156), outline border/bg `cx.theme().border`/`tokens.background` (196-197). Sizes are hardcoded pixel presets per `Size` (173-178). **Best keyboard coverage of the non-Button files** — tests prove tab/Enter/Space and pointer-vs-keyboard-vs-disabled distinctions (`toggle.rs:470-494`). No animation (pressed state is a discrete style block). **VERDICT: REUSE BEHAVIOUR BUT RESTYLE** — same chamfer/bevel/animation gaps as Button.

#### `button/mod.rs` (11) — pure re-export wiring; `button_icon` is `pub(crate)`-only. **N/A** (not a component).

---

### `chart/` (complex — full pass)

**Shared plumbing** (`chart/mod.rs`, 89 lines): thin — re-exports the 7 chart types plus two label-building helpers, `build_point_x_labels` (28-57, for point-scale charts) and `build_band_labels` (65-89, for band-scale charts). **No shared `ChartData`/`Series` trait** — every chart is its own generic struct over `Rc<dyn Fn(&T) -> X>` closures. The real engine sits one level down in `crate::plot` (see next section): every chart implements `plot::Plot` via `#[derive(IntoPlot)]`, painting with raw `window.paint_quad`/`paint_path`/`PathBuilder` — **none compose a `div()`/`Styled` element tree**. The only two places gpui's rounded-rect `Corners` primitive appears anywhere in `chart/` are `BarChart.corner_radii` (`bar_chart.rs:51,78,251-253,564`, defaults **square**) and `SankeyChart.node_corner_radius` (`sankey_chart.rs:83,155-158,396,403`, also defaults square) — everything else is raw path/line/arc drawing with **no rounding concept at all**, which is actually favorable ground for a chamfer treatment. **Zero animation/transition/ease code anywhere in `chart/`** (confirmed by grep) — hover (dot/crosshair/tooltip) is recomputed synchronously from cursor position every frame, with no persistent animated state.

- **`area_chart.rs`** (313): multi-series filled area (`AreaChart::new(data)` + `.x()/.y()` per series, `.name()/.stroke()/.fill()`, `.natural()/.linear()/.step_after()`, `.tick_margin()/.x_axis()/.grid()/.id()`). Default fill/stroke = fixed `chart_2` swatch (`area_chart.rs:210-215`). **VERDICT: REUSE BEHAVIOUR, RESTYLE (cheap)** — pure vector path, scale math cleanly separated from paint.
- **`bar_chart.rs`** (824, canonical pattern): vertical/horizontal bars, band+value axes (`BarChart::new(data)` + `.band()/.value()/.fill(closure)/.fill_gradient(closure)/.label()/.label_axis/.value_axis/.value_tick_count/.grid()/.alignment()/.corner_radii(Corners<Pixels>)`). Default fill = fixed `chart_2` (`bar_chart.rs:530`); `corner_radii` defaults to 0 — plain rects, not chamfer-expressible via gpui's `Corners`. **VERDICT: REUSE BEHAVIOUR, RESTYLE (moderate)** — layout math well isolated; the bar body's `corner_radii`-based quad is the one call needing replacement with a hand-built polygon path for a chamfered bar.
- **`candlestick_chart.rs`** (238): OHLC, hand-paints its own candle body/wick directly (`candlestick_chart.rs:213-235`) rather than via a `plot::shape` type. No `.id()`, no tooltip, no color-override hook — uses `cx.theme().chart_bullish`/`chart_bearish` with zero override (`candlestick_chart.rs:200-204`). **VERDICT: WRAP or rewrite from reference** — thinnest/plainest file; cheapest to reimplement its ~20-line draw routine chamfered from scratch, keeping only the scale-math as reference.
- **`line_chart.rs`** (285): single-series line + optional dots (`LineChart::new(data)` + `.x()/.y()/.stroke()/.natural()/.linear()/.step_after()/.dot()/.tick_margin()/.x_axis()/.grid()/.id()/.name()`). Stroke width hardcoded `2.` (`line_chart.rs:209`). **VERDICT: REUSE BEHAVIOUR, RESTYLE (cheap)** — mirrors AreaChart's separation.
- **`pie_chart.rs`** (329): pie/donut with a genuinely valuable bespoke two-pass leader-line label-overlap-avoidance algorithm (`spread_labels`, `pie_chart.rs:279-329`). Default slice color = fixed `chart_2` when `.color()` unset (180) — no built-in palette cycling. No `.id()`/tooltip at all. **VERDICT: REUSE BEHAVIOUR, RESTYLE** — arc math delegated to `plot::shape`; keep the label-collision algorithm regardless of restyle.
- **`radar_chart.rs`** (701): multi-series spider chart, uniquely supports arbitrary `AnyElement` axis labels via a `prepaint` pass (`radar_chart.rs:342-393`). **Default stroke cycles the full 5-swatch theme palette** by series index (249-262) — one of only two chart types that do this by default. Hover tooltip via angle-based hit test, unit-tested (`radar_chart.rs:654-700`). **VERDICT: REUSE BEHAVIOUR, RESTYLE** — best-suited of the eight to a custom visual language since it already accepts arbitrary elements as labels (a bespoke chamfered label chip drops in with zero internal changes).
- **`sankey_chart.rs`** (579): d3-sankey-ported flow diagram; the real value is the force-relaxation graph-layout algorithm living in `plot::shape::Sankey` (called at `sankey_chart.rs:258,347-355`), which this file only consumes as position data. Node color cycles the full 5-swatch palette (357-371). No `.id()`/tooltip/animation at all — the idiomatic "flow particle" affordance is absent. **VERDICT: WRAP** — wrap the layout math, rewrite the presentation (rect nodes, gradient ribbons) which is comparatively small.

**Overall chart/ verdict**: layout/scale math is consistently separated from paint, making "reuse behaviour, restyle" cheap for area/line/bar/pie/radar (and candlestick, though thin enough that rewrite-from-reference may be faster); Sankey's value is specifically its ported layout algorithm. **Animation is the one universal gap** — it must be built as an external layer regardless of reuse strategy, most naturally by re-invoking `paint()` per animation frame with interpolated closures (gpui's `Plot::paint` already re-runs per repaint; there's no internal animated-state machine to fight). Default per-series coloring is inconsistent (only radar/sankey cycle the full palette by default) but every chart exposes an explicit override hook.

---

### `plot/` (complex — full pass)

**What it is**: a **math/geometry + low-level-paint layer**, strictly internal plumbing for `chart/` (every `chart/*.rs` file imports from `plot::`; a repo-wide grep found **zero** references back from `plot/` into `chart/` — a clean one-directional layering, no duplication). Most shapes produce raw `gpui::Path`/`PaintQuad` values and paint directly; `Pie::arcs`, `Stack::series`, and all of `Sankey`'s topology/layout are **pure numeric data with no painting at all**. Only `tooltip.rs` builds real `Div`/`RenderOnce` elements that go through the normal element tree.

- **`mod.rs`** (123): defines the `Plot` trait (`mod.rs:23-90`) every chart implements; re-exports `AXIS_GAP`, `AxisLabelSide`, `AxisText`, `PlotAxis`, `Grid`, `PlotLabel`, `scale`, `shape`, `tooltip`; `StrokeStyle` (Natural/Linear/StepAfter) shared by Area/Line/RadialLine. No theme usage.
- **`scale/`** (band.rs, linear.rs, ordinal.rs, point.rs, sealed.rs, scale.rs): `Scale<T>` trait (`tick()`, `least_index()`) implemented by `ScaleBand`, `ScaleLinear`, `ScaleOrdinal`, `ScalePoint`. **Confirmed zero rendering/color/theme coupling across all four** — pure `Vec<T>` → `f32` math. `ScaleBand::band_width()` has a **hardcoded 30px ceiling** (`band.rs:37`) — a baked-in design choice, trivial to fork if a chamfered wide-bar chart wants wider bands. **VERDICT: REUSE AS-IS**, unconditionally — this math has no "look" to clash with anything.
- **`shape/`** (arc, area, bar, line, pie, radial_line, sankey [1323 lines, the largest file], stack): pure-geometry pieces (`Pie::arcs`, `Stack::series`, `Sankey`'s full d3-sankey-style relaxation solver) have **no visual opinion at all** and are directly reusable. Actual `.paint()` calls go through gpui's rounded-corner `PaintQuad`/`Corners` primitives in exactly two places (`Bar::corner_radii`, `bar.rs:169-172`, defaults square; hardcoded-circular dots in `Line`/`RadialLine`, `line.rs:119`) — neither expressible as a chamfer without swapping the final paint call for a hand-built `PathBuilder` polygon. **VERDICT: REUSE BEHAVIOUR BUT RESTYLE**, with the pure-math portions being unconditional REUSE AS-IS.
- **`axis.rs`+`grid.rs`+`label.rs`**: entirely color/font-agnostic — every stroke color is threaded in by the caller from `cx.theme()` (e.g. `PlotAxis::new().stroke(cx.theme().border)` at `chart/bar_chart.rs:419`), so a bevel/rose palette drops straight in with zero edits here. `AXIS_GAP = 18.` (`axis.rs:11`) is the only hardcoded pixel constant (label-offset math, not a visual shape). `label.rs`'s `measure_text_width`/`truncate_text_to_width` are useful standalone text-metric utilities; font *family/weight* comes from the ambient `window.text_style().font()` (a custom font is a one-line theme-config change, not a code change) while font *size* is a hardcoded 10px constant (`label.rs:10`), overridable per instance. **VERDICT: REUSE AS-IS** — visuals limited to 1px lines and shaped text, nothing rounded to clash with anything.
- **`tooltip.rs`**: the **one file in `plot/` that reads `cx.theme()` directly** — `CrossLine`'s hairline uses `cx.theme().border.mix(cx.theme().foreground, 0.8)` (118-122); the row swatch dot uses `cx.theme().radius.half()` (386); and critically the deferred content box calls `.popover_style(cx)` (424), which per `styled.rs:263-271` applies `theme.popover`/`popover_foreground`/a theme shadow/**`.rounded(theme.radius)`** — the single biggest rounded-vs-chamfer collision in the module. `Tooltip` itself is generic/chart-agnostic (`.title()/.row(color,label,value)`, arbitrary `.child()` content via `.appearance(false)` as an existing escape hatch, `tooltip.rs:334-337`) with cursor-quadrant-aware positioning reused across every chart type. Being real `Div` elements (unlike the raw-paint shape files), the tooltip is the only thing in `plot/` that participates in gpui's normal animatable layout system. **VERDICT: WRAP** — reuse the crosshair/dot/hover-box machinery wholesale; replace only the `popover_style()` panel skin.

**Overall plot/ verdict**: a clean, well-isolated geometry/paint-primitive library with no chamfer-hostile chrome except two rounded-corner call sites (`Bar::corner_radii`, `tooltip.rs`'s `popover_style`) and one hardcoded ScaleBand width ceiling — everything else is raw line/arc/path drawing that a chamfer treatment slots into without a rewrite.

---

### `combobox.rs` (complex — full pass, 1645 lines)

**What it is**: single/multi-select combobox with optional search and a **virtualized** dropdown list, built on `SearchableListState`/`ListState` (`combobox.rs:110-137`). This vendor crate is a thin styling facade over a separate unvendored crate (`gpui_ce_components_base`, aliased `gpui_base`) that owns the base `BaseCombobox` behavior primitive; local-to-this-repo list rendering/keyboard/virtualization live in `list/list.rs` and `searchable_list/`.

- **API**: `ComboboxState::new(delegate, selected_indices, window, cx)` (145) wrapping `SearchableListState::new(...)` (156); `.multiple(bool)` (282), `.searchable(bool)` (288). Element `Combobox::new(&state)` (764) with `.menu_width/.menu_max_h/.placeholder/.icon/.check_icon/.search_placeholder/.cleanable/.disabled/.empty/.appearance/.render_trigger(fully custom trigger closure)/.footer`. State is **hybrid controlled/internal**: selection lives in `ComboboxState` but is externally readable/settable (`selected_values()/set_selected_values()/set_selected_indices()/add_selected_index()/clear_selection()`, 294-377); no `on_change` prop — instead **event-based** via `ComboboxEvent::Change`/`Confirm` (`cx.emit`, 129-137,203,208,471,481) consumed through `EventEmitter`/`cx.subscribe`. Delegate model (`SearchableListDelegate`) supplies items/search/veto hooks (`on_will_change`, `on_confirm`).
- **Styling**: trigger reads `cx.theme().muted_foreground`, `.input`/`.ring`/`.radius`/`.transparent` (`combobox.rs:983-999`); accepts a caller `StyleRefinement` via `.refine_style(style)` (993), layered **after** the hardcoded appearance block, so overrides win, but the base border/bg logic when `appearance(true)` is baked in. Corner radius is theme-driven (`cx.theme().radius`, 989) — never a chamfer. Popup surface: `render_popup_shell` → `crate::popover::dropdown_popup(...)` + `.popover_style(cx)` (1042-1051) — the **shared popover look**, with **no builder hook on `Combobox` to override the popup's shadow/border/radius**; `render_popup_shell` is a private free function, not public API, so restyling the dropdown shell means bypassing it entirely.
- **Keyboard/focus**: delegates to `ListState` in `list/list.rs`, which binds under context `"List"`: `escape→Cancel`, `enter→Confirm`, `secondary-enter→Confirm{secondary:true}`, `up→SelectUp`, `down→SelectDown` (`list.rs:26-35`), handled at `list.rs:355-425` with wraparound via `rows_cache.prev/next`. The base crate independently binds the same keys under `"Combobox"` for the outer trigger. **No type-ahead anywhere in the vendor tree** (confirmed by grep). Focus trapping: `focus_handle()` returns the list's handle while open, else the trigger's (729-735); `on_blur` closes only if focus left both (487-496). Highlighted-item indication is a **background fill** in `list/list_item.rs:237-262`: `bg(cx.theme().list_active)` plus, when `theme().list.active_highlight` is on, an **absolute-positioned 1px border overlay** using `cx.theme().list_active_border` — this border-overlay seam is the closest existing hook for a bevel-indicator swap.
- **Virtualization**: confirmed real — `list.rs:537` calls `v_virtual_list(...)` with a `visible_range` closure that only renders in-view rows and drives `load_more_if_need` (`list.rs:325-348,541-547`); requires uniform per-section row height; has a `Scrollbar::vertical` overlay (587-589).
- **VERDICT: WRAP.** The delegate/selection/keyboard/virtualization machinery (`SearchableListState`, `ListState`, `v_virtual_list`) is the hard part and is worth keeping as-is — reimplementing sectioned virtualization, search debouncing, and veto-able selection changes would be expensive. The visual layer (`render_trigger_container`, `render_popup_shell`, `ListItem`'s selected-state styling) is baked around rounded rectangles and a fixed shared `popover_style` with no restyle hook, so getting chamfered corners or a two-tone bevel requires a source-level fork of this file plus `list_item.rs` and `styled.rs::popover_style`.

### `select.rs` (complex — full pass, 910 lines)

Structurally Combobox's single-select sibling — same `SearchableListState` plumbing, no multi-select/custom-trigger/footer hooks. `SelectState::new(delegate, selected_index, window, cx)` (149) + `.searchable(bool)` (273); mutators `set_selected_index/set_selected_value/set_items`; element `Select::new(&state)` with `.menu_width/.menu_max_h/.placeholder/.accessibility_label/.icon/.title_prefix/.cleanable/.search_placeholder/.disabled/.empty/.appearance`. Same hybrid state model, single `SelectEvent::Confirm(Option<Value>)` event (70-75,201). Same theme fields and `.refine_style()` override pattern (481-497) as Combobox; same `popover_style(cx)` popup (548-557) with the same non-overridability caveat. Keyboard: same `list/list.rs` bindings, plus its own `escape` handler (381-391) and an `on_blur` that **reverts the cursor to the committed index** if focus is lost with an uncommitted highlight (351-367) — Combobox instead commits on blur/dismiss, a real behavioral difference. Same virtualized `List`/`ListState`/`v_virtual_list` (559). **VERDICT: WRAP**, identical reasoning to Combobox — and since the two share almost the entire rendering approach, a single restyled trigger/popup/list-item skin can serve both rather than two separate reskins.

### `color_picker.rs` (complex — full pass, 610 lines)

Popover-based color picker: featured swatches + Tailwind-style palette grid + an HSLA-slider tab + hex input. `ColorPicker::new(&state: Entity<ColorPickerState>)` (68) — `ColorPickerState` is **re-exported from the base crate**, this file is a pure rendering facade; value/callback state (`state.value()/.preview_color()/.select_color()`, `ColorPickerEvent`) all live externally. Builders: `.featured_colors()/.icon()/.label()/.accessibility_label()/.anchor(Anchor)`. Reads `cx.theme().radius` for swatch/hover-preview squares (199,593), palette defaults (`red/red_light/blue/…`, 207-220), `foreground.opacity(0.7)` for slider labels (269); individual swatches derive their own hover/active border/bg from **the swatch's own hue** (`color.darken()/.lighten()`, 141-143) rather than theme tokens — correct by design since a swatch's chrome must relate to its own color. Popup uses the `Popover` component wrapper (492) which internally still calls `.popover_style(cx)` (`popover.rs:284`) — same shared surface as Combobox/Select. Slider tracks are plain rectangular bars with no rounding at all (415-441) — easy to reshape. Selection indication is a 2px border on the active swatch (base-crate `ColorSwatch::selected()`, not analyzed — out of vendored source). No keyboard arrow-nav in this file; navigation is mouse/drag-driven. No virtualization (small fixed 9-row palette grid). **VERDICT: WRAP** (lighter-weight than combobox/select) — keep the external HSLA/slider/hex-sync state; the chrome is simple flat rects/squares so a chamfered restyle is cheap, with the shared `popover_style()` and swatch/button radius calls being the only real friction, both easily forked since this file doesn't own a private popup-shell function the way Combobox does.

### `form/` (complex — full pass: mod.rs, form.rs, field.rs)

**`form/mod.rs`** (20): pure wiring — `v_form()`/`h_form()`/`field()` convenience constructors.

**`form/form.rs` — `Form`**: a grid/flex *container* for `Field`s. `Form::vertical()/.horizontal()` (30-37) + `.label_width(Pixels)` (46), `.label_text_size(Rems)` (52), `.child()/.children()` (58-67), `.columns(usize)` (72). No selection/value state — pure layout composer, fields passed in fully built. `Sizable` only changes the gap (4/6/8/12px, 95-99); render is `v_flex().grid().grid_cols(n)` (101-106) — **no border, background, radius, or shadow anywhere in this file.**

**`form/field.rs` — `Field`**: label + description + arbitrary child content, one row/column of a `Form`. `.label(impl Into<FieldBuilder>)`/`.label_fn()` (119-144), `.description()`/`.description_fn()` (147-162), `.required(bool)` (renders a red `*`, 313-317), `.items_start/end/center()` (186-201), `.col_span/.col_start/.col_end` grid placement (206-221). Children are opaque `AnyElement`s via `ParentElement` (224-228) — Field never inspects or restyles whatever input widget you place inside. **Confirmed: no validation and no error-message slot exist anywhere in `form/`** (`grep -n "error\|valid"` returns nothing) — `description` is the only secondary text; styling touches are just `text_sm().font_medium()` for the label, `cx.theme().danger` for the required asterisk (315), `cx.theme().muted_foreground` for description (338). `.visible(bool)` is a **dead field**, never consulted in `render()`.

**Group VERDICT: REUSE AS-IS.** Exactly "cheap layout plumbing regardless of visual skin" — zero baked-in look beyond two text-color reads and a grid layout; there is no validation/error system to even trade off against a restyle.

### `stepper/` (complex — full pass: mod.rs, stepper.rs, item.rs, trigger.rs)

**`stepper/stepper.rs` — `Stepper`**: horizontal/vertical step-progress bar. `Stepper::new(id)` (28) + `.layout(Axis)/.vertical()` (49-58), `.selected_index(usize)` (61-64, called `step`), `.item(StepperItem)/.items()` (67-76), `.disabled()`, `.on_click(Fn(&usize,…))` (87-93) — **fully controlled**, no internal `Entity` state (`RenderOnce`, not backed by an entity); the caller owns `step` and re-renders. No theme reads in this file itself.

**`stepper/item.rs` — `StepperItem`**: one step = a `StepperTrigger` bubble + (if not last) a `StepperSeparator` line. `.icon()/.disabled()` (can override the parent's disabled per-item, 51-56)/`.text_center()`. `StepperSeparator` (169-264) is a flat bar: `bg(cx.theme().border)` normally, `bg(cx.theme().tokens.primary)` when passed (261-262) — hardcoded flat fill, no gradient/animation, trivial to restyle; positioning math is layout-only and reusable regardless of skin.

**`stepper/trigger.rs` — `StepperTrigger`**: the circular numbered/iconed step bubble. **`.rounded_full_style(cx)` (trigger.rs:121)** forces the indicator circular (or square only if `theme.radius==0`) — cannot be a chamfered shape without replacing this call. Background is fully theme-driven (`tokens.secondary` idle, hover/active tokens, `tokens.primary`/`primary_foreground` when checked, 124-133) — no hardcoded hex. Diameter is a literal per-`Size` pixel value (24/18/8/32px, `item.rs:117-121`), not `Styled`-overridable.

**Group VERDICT: REUSE BEHAVIOUR BUT RESTYLE.** The controlled `step`/`on_click`/`checked_step` model and cross-orientation separator-positioning math are genuinely reusable. The step "dot" — the single most important element for a bevel-focus channel here — is hardcoded to `rounded_full_style()`; a chamfered stepper needs `stepper/trigger.rs`'s indicator div and `stepper/item.rs`'s separator forked, while the outer control-flow can be kept nearly verbatim.

### `time/` (complex — full pass: mod.rs, calendar.rs, date_picker.rs, utils.rs)

**`time/mod.rs`** (2): declares only `pub mod calendar; pub mod date_picker;` — **`utils` is never declared as a module anywhere in the crate.**

**`time/utils.rs` — dead code.** `days_in_month(year, month, first_day) -> Vec<Vec<NaiveDate>>` (31) computes a well-tested 5×7 day grid (89-200) but **is never wired in**: no `mod utils;` declaration exists, and a crate-wide grep for `days_in_month` shows zero call sites outside this file. It does not compile as part of the crate and its tests never run. The real date-grid math now lives in the external `gpui_base::CalendarState` (per `calendar.rs:36`'s own doc comment: "styled facade... complete behavior and structure in gpui-base"). The logic is sound, portable pure-`chrono` math with no GPUI/theme dependency if ever needed as a starting point, but as shipped it is inert. **VERDICT: SKIP.**

**`time/calendar.rs` — `Calendar`**: a styled facade over `gpui_base::BaseCalendar` — supplies month/weekday labels (`rust_i18n::t!`) and per-cell render styling only; all state (day selection, month/year nav, disabled-date `Matcher`) is external. `Calendar::new(&state)` (48) + `.number_of_months()` (58), `.first_day_of_week()` (63). Heavy `cx.theme()` use: per-`Size` radius for day cells (114-116), `muted_foreground`, `accent`/`accent_foreground` (in-range/today), `tokens.primary`/`primary_foreground` (selected), `tokens.secondary_hover` (hover) (169-191), outer frame `.border_1().border_color(theme.border).rounded(theme.radius_lg)` (194-196) — all theme-driven, but rounded-rect only. **No keyboard bindings in this file or the base crate** (confirmed by grep) — day navigation is mouse/click-only, nothing to preserve.

**`time/date_picker.rs` — `DatePicker`**: input-like trigger opening a popover with `Calendar` + optional preset buttons, single-date or range. `DatePickerState::new`/`::range` (96-103) + `.date_format()/.number_of_months/.first_day_of_week/.disabled_matcher()/.set_year_range`; element `.placeholder/.cleanable/.presets(Vec<DateRangePreset>)/.number_of_months/.appearance`. **Event-driven** value flow (`DatePickerEvent::Change(Date)`, 37-39,194), like Combobox/Select. Same trigger theme fields as combobox/select; popup is the same `dropdown_popup(...)` + `.popover_style(cx)` (496-502) shared surface, same non-overridable-shadow/radius caveat. The embedded `Calendar` is de-chromed inside the popup (`.border_0().rounded_none().p_0()`, 535-537) — useful precedent: only the outer shell needs chamfering, not nested containers. Local key bindings under `"DatePicker"`: `enter→Confirm`, `escape→Cancel`, `delete`/`backspace→Delete` (26-32,207-220); `focus_back_if_need` (228-238) restores focus after stray mouse-down-out. No arrow-key day-grid navigation (absent in base crate too). No virtualization needed (small grid).

**Group VERDICT: WRAP** for `calendar.rs`/`date_picker.rs` — keep calling the external `CalendarState`/`BaseCalendar`/`BaseDatePicker` for behavior; every visual surface worth chamfering (day cells, popover shell, trigger box) is styled in *this* vendor layer with `.rounded(theme.radius)` and the shared `popover_style()`, so a full restyle means forking `calendar.rs`'s cell-rendering closure and `date_picker.rs`'s trigger+popup construction while leaving the state entities untouched. `time/utils.rs` is out of the decision entirely (dead code).

---

### `sidebar/` + `status_bar.rs` (complex — full pass)

#### `sidebar/mod.rs` (769) — `Sidebar<E: SidebarItem>`

Collapsible side-panel container (shadcn-style `Icon`/`Offcanvas`/`None` collapse modes) with header/content-list/footer regions. `trait SidebarItem: Collapsible + Clone { fn render(...) }` (211-218) is the extension point any top-level entry implements (in practice `SidebarGroup<E>` or `SidebarMenu`). `Sidebar::new(id)` + `.side(Side)/.collapsible(impl Into<SidebarCollapsible>)/.collapsed(bool)/.header()/.footer()/.child(E)/.children()`. Reads `cx.theme().tokens.sidebar` (bg, 413), `sidebar_foreground` (414), `sidebar_border` (415-419). Default width `DEFAULT_WIDTH=255px`, collapsed `COLLAPSED_WIDTH=48px` (27-28) — expanded width is overridable per-instance via `Styled::w()`. **Note**: `self.style.padding` is force-reset at the top of `render` (381) — padding overrides specifically won't stick, everything else will. **Collapse/expand is real animated width-tweening**: `crate::animation::EffectTransition` with `ease_in_out_cubic` over `SIDEBAR_TRANSITION_DURATION=200ms` (29,540-544), with a full `SidebarAnimationState` state machine handling interrupted transitions and stale hide-requests, covered by 12 unit tests (548-769) — **genuine, valuable animation infrastructure directly relevant to a "heavy animation" goal.** Virtualization: top-level content uses gpui's native `list(list_state, closure)` with `ListState::new(content_len, ListAlignment::Top, overdraw=30%)` (385-396,447-469) — windowed only at the **top-level group granularity**; a `SidebarMenu`'s items inside one group render eagerly (menu.rs:74-83), so a sidebar with one giant flat group of thousands of items is not virtualized. No keyboard nav across items in this file itself. **VERDICT: REUSE BEHAVIOUR BUT RESTYLE** — the collapse/expand animation state machine is a strong foundation to extend; visually plain (flat bg + 1px border, no radius calls at all in this file) so restyling is low-cost.

#### `sidebar/menu.rs` (424) — `SidebarMenu`/`SidebarMenuItem`

Primary nav-list content type — clickable, optionally-nested (submenu), icon+label items. `SidebarMenu::new().child()/.children()`; `SidebarMenuItem::new(label)` + `.icon()/.active(bool)/.on_click()/.collapsed()/.default_open()/.click_to_open()/.click_to_toggle()/.children()` (recursive submenu)/`.suffix()/.disable()/.context_menu()`. Plain builder/value type, **not a delegate pattern** (unlike table/). Reads `cx.theme().radius` (item rounding, 275), `sidebar_accent`/`sidebar_accent_foreground` for hover (279-281) **and** active-item highlight (283-294) — **active-item indication is purely a flat background-color swap, no border/indicator-bar** — this is the one place in `sidebar/` where a bevel/border focus channel would need genuine new code (add a border/overlay branch analogous to `table/state.rs`'s row-selection overlay, see below) rather than a config flip. Submenu indent uses a plain 1px `.border_l_1().border_color(sidebar_border)` (388-389). **Row height is a hardcoded literal `.h_7()`** (296), not derived from a `Size` enum — a genuine hardcoded constant to fork for taller chamfered rows. No keyboard arrow-nav; mouse/click-driven only, with a collapsed-icon tooltip via `managed_tooltip_at` (210-212,365-372). No virtualization within a single menu. **VERDICT: WRAP (thin) / REUSE BEHAVIOUR BUT RESTYLE** — submenu open/close state and tooltip logic are reusable, but the active-item indicator needs actively reworking (not just retheming), and the fixed row height needs overriding.

#### `sidebar/group.rs` (87) — `SidebarGroup<E>`: labeled section wrapper (dimmed title + child list). `cx.theme().radius` at `group.rs:68` is essentially dead visually (no bg/border to round); `sidebar_foreground.opacity(0.7)` for the label (70); fixed `.h_8()` label height. No selection/active state at this level. Children render eagerly. **VERDICT: REUSE BEHAVIOUR BUT RESTYLE** — trivial, nothing here visually conflicts with a chamfered system.

#### `sidebar/header.rs` (100) / `sidebar/footer.rs` (88) — near-identical header/footer row elements, both `Selectable`+`Collapsible`+`impl DropdownMenu` (so either can be a dropdown trigger). `.rounded(cx.theme().radius)` (`header.rs:88`/`footer.rs:77`); hover and `.selected` both apply the **same flat `sidebar_accent`/`sidebar_accent_foreground` swap** as `menu.rs` (`header.rs:90-97`/`footer.rs:78-85`) — same active-indicator limitation. **VERDICT: REUSE BEHAVIOUR BUT RESTYLE**, same assessment as menu.rs.

**Overall `sidebar/` verdict**: the collapse/expand animation state machine (`mod.rs`) and submenu disclosure state (`menu.rs`) are genuinely valuable and visually neutral. Friction points are small and consistent: every active/selected indicator across menu/header/footer is a flat bg swap today with no border precedent (unlike table/, see below) — `table/state.rs:2117-2137`'s bordered-overlay pattern is a direct template to copy in here; four single-line `cx.theme().radius` calls are trivial removals; menu-item row height is a hardcoded `.h_7()` needing a fork. Virtualization exists only at top-level-group granularity via gpui's native `list()`, fine for a typical docs-reader nav tree.

#### `status_bar.rs` (106) — `StatusBar`

Three-region (`left`/center/`right`) horizontal bar. `StatusBar::new().left()/.right()` (append multiple, 49-59); `.child()/.children()` add to the self-aligning center region (20-23,96-104). Reads `cx.theme().status_bar_border` (top border, 91) and `tokens.status_bar` (bg, 92); text `.text_xs()` + `muted_foreground` (93-94). **No radius, no fixed colors beyond theme tokens** — a blank, trivially-restyled shell (confirmed independently by direct reading). No keyboard/focus (interactivity comes from whatever child elements — e.g. `Button`s — are passed in). No virtualization needed. **VERDICT: REUSE AS-IS** — ~30-line layout primitive with nothing hardcoded to fight.

---

### `table/` (complex — full pass)

#### `table/mod.rs` (18) — aggregator; `init(cx)` registers `data_table::init`. **N/A** (glue).

#### `table/table.rs` (668) — `Table`/`TableHeader`/`TableBody`/`TableFooter`/`TableRow`/`TableHead`/`TableCell`/`TableCaption`

A **static, non-virtualized, purely compositional** HTML-`<table>`-like set of elements, explicitly documented as distinct from `DataTable`: "a simple, stateless, composable table without virtual scrolling or column management" (`table.rs:16-19`). `Table::new()` then chain `.child(TableHeader::new().child(TableRow::new().child(TableHead::new()...)))` — you hand it fully-built rows, no delegate. `TableHead`/`TableCell` support `.col_span()/.text_center()/.text_right()` (450-465,540-556). Every element implements `Styled` for full per-instance override. Reads `cx.theme().tokens.table` (bg, 113), `tokens.table_head`/`table_head_foreground` (199-200), `tokens.table_foot`/`table_foot_foreground` (340-341), `table_row_border` (203,343,415), `muted_foreground` for caption (663). **Hardcoded `MIN_CELL_WIDTH = px(100.)`** applied to every head/cell (14,505,596) — a floor to override per-cell for a dense chamfered layout. **No rounding/radius anywhere in this file** — a blank slate, favorable for a chamfered redesign. No keyboard/focus; no virtualization (every row renders unconditionally, 115-120,272-278). **VERDICT: REUSE BEHAVIOUR BUT RESTYLE** (or re-derive) — cheap for a small static table (e.g. a docs-reader metadata panel) but not a substitute for `DataTable` on real data.

#### `table/column.rs` (297) — `Column` config value type (key, name, alignment, sort mode, width bounds, fixed/resizable/movable/selectable flags) plus `ColGroup`, `DragColumn`, `ResizeColumn`, `ColumnGroup`. `Column::new(key,name)` + `.sortable()/.sort()/.ascending()/.descending()/.width()/.min_width()/.max_width()` (auto-clamps current width, 201-225), `.fixed_left()/.resizable()/.movable()/.selectable()/.paddings()`. Defaults: width 100px, min 20px, max unbounded, resizable/movable/selectable all `true` (67-84). `DragColumn` (the drag-preview chip) reads `tokens.table_head`/`muted_foreground`/`border` + `.shadow_md()` (278-283) — a plain border-only chip, no radius call, easy to restyle. **VERDICT: REUSE AS-IS** — pure column metadata/builder, nothing fights a chamfer.

#### `table/data_table.rs` (173) — `DataTable<D: TableDelegate>` public entry point, a thin `RenderOnce` wrapper around `Entity<TableState<D>>`. `.stripe(bool)/.bordered(bool)`(default true)`/.scrollbar_visible(v,h)/.with_size(Size)`. `init(cx)` (15-29) registers the `"DataTable"` key-context: Escape→Cancel, Up/Down→SelectUp/Down, Left/Right→SelectPrevColumn/NextColumn, Home/End, PageUp/PageDown, Tab/Shift-Tab→Next/PrevColumn. Styling: always `bg(tokens.table)`; when `bordered` (default) also `.rounded(cx.theme().radius)` + `.border_1().border_color(border)` (165-170) — **the one call in this whole cluster that would visibly fight a chamfered design**, but it's a single boolean-gated call, trivial to not invoke in a fork. `track_focus` + 9 `.on_action(...)` wires (151-164) — the full keyboard-attachment point. **VERDICT: REUSE BEHAVIOUR BUT RESTYLE (thin wrap)** — small enough to copy-and-restyle while keeping 100% of the key-binding wiring.

#### `table/delegate.rs` (232) — `TableDelegate` trait, the delegate model the brief expected. Required: `columns_count/rows_count/column/render_td`. Optional-with-defaults: `perform_sort` (no-op default — **all actual sort comparison lives in the consumer's delegate**), `render_header/render_th/render_tr/render_group_th/render_empty/render_loading/render_last_empty_col`, `group_headers` (multi-level headers), `context_menu`, `move_column` (no-op default — you own reordering your backing data), `loading/has_more/load_more_threshold/load_more` (infinite scroll), `visible_rows_changed`/`visible_columns_changed` (called on every virtualization-window change, explicitly documented as needing to be fast, 197-223), `cell_text` (CSV export). `render_empty` default uses `muted_foreground.opacity(0.6)` + an `Inbox` icon (139-145), fully overridable per-delegate. **VERDICT: REUSE AS-IS** — cleanly separates render-contract from sort/filter policy (which lives entirely in your own delegate impl); none of it constrains visuals.

#### `table/loading.rs` (109) — default skeleton (1 header + 4 body rows via `crate::skeleton::Skeleton`); reads `tokens.table_head`/`table_row_border`; row height from `Size::table_row_height()` halved. Fully swappable via `TableDelegate::render_loading`. **VERDICT: SKIP** (or trivially replace) — override the delegate method with your own skeleton, costs nothing.

#### `table/state.rs` (2496 lines — the engine)

`TableState<D>` holds: focus handle; delegate; `TableOptions`; layout caches (`col_groups`, `header_layout`, `bounds`); feature-flag builders (`.loop_selection/.col_movable/.col_resizable/.sortable/.row_selectable/.col_selectable/.cell_selectable/.row_header`, 305-375); scroll handles (`vertical_scroll_handle: UniformListScrollHandle`, `horizontal_scroll_handle: VirtualListScrollHandle`, 227-228); selection state (three mutually-exclusive `SelectionMode`s: Row/Column/Cell, 25-47); resize/drag state; `visible_range: TableVisibleRange` (public accessor, 100-119,564-570).

**Major responsibilities**: selection state machine (row auto-scroll via `ScrollStrategy`, column selection with synchronous header-offset resolution to avoid virtualization lag, cell selection, right-click tracked separately as a distinct visual state); full keyboard action set — Escape/Up/Down/Left/Right/Home/End/PageUp/PageDown/Tab, all honoring `loop_selection` (814-1090); column resize via drag handle + edge-autoscroll (1090-1430), emits `TableEvent::ColumnWidthsChanged`; column reordering via drag-and-drop (1172-1225), emits `TableEvent::MoveColumn`; sorting — **this file only manages the tri-state sort icon and calls `delegate.perform_sort()`; all comparator logic is the consumer's** (1140-1172); fixed/pinned left columns (691-702, rendered as a visually separate non-scrolling block); multi-row header/column-group layout (621-691); load-more/infinite scroll (1225-1247); cell-render perf instrumentation (2207-2249); CSV-style export/dump (`headers()/dump()/dump_range()`, 572-621).

**Styling / theme touches** (every `cx.theme()` call): `table_active` (selection overlay bg, 1331,1933,2128); `table_active_border` (overlay border, 1936,1949,2059,2075,2130); `table_row_border` (separators, 1371,1444,1889,2168); generic `border` (resize-handle hover, dividers, 1372,1730,1768,1791,1992); `tokens.table_head`/`table_head_foreground` (header bg/text, 1445,1682-1683,1723,1783); `tokens.table_even` (stripe, 1891,2169); `tokens.table_hover` (row hover, 1893-1899); `tokens.secondary`/`secondary_active`/`secondary_foreground` (sort-icon states, 1487-1495); `radius/2.` (sort-icon hover chip only, 1482 — the **only literal corner-radius call in this whole 2496-line file**); `drag_border` (drop-gap indicator, 1567); `tokens.accent` (flat row-selected bg, used only when `theme.list.active_highlight==false`, 2133); `selection` (right-clicked-row border, 2148).

**The single most important finding for the bevel/focus-channel goal**: selected-row indication is a **theme-driven branch, not a fixed look** (`state.rs:2117-2137`). If `cx.theme().list.active_highlight` is `true`, the selected row gets an **absolutely-positioned overlay div** with `bg(table_active)` + `border_1().border_color(table_active_border)` — a bordered ring drawn on top of the row, separate from the row's own background. If `false`, it falls back to a flat `.bg(tokens.accent)` swap. This is architecturally the right seam for a two-tone bevel focus channel — today it's a global per-theme boolean (shared with plain list rows in `list/list_item.rs:239-251`), not a per-table override, but you'd fork this branch (or set `active_highlight=true` and re-skin `table_active`/`table_active_border` as bevel colors) rather than build indicator logic from scratch. Right-click uses a similar overlay pattern with a single `border_color(cx.theme().selection)` — currently one uniform 1px border, but the same overlay-div technique generalizes directly to two nested/offset borders for a true bevel.

Row height (`Size::table_row_height()`) is a fixed lookup table (26/30/32/40px) but **also supports arbitrary custom height via `Size::Size(px)`**, and both `DataTable`/`TableState` implement `Sizable::with_size` — per-instance override to any height is possible, just uniform across all rows in one table (a hard requirement of `uniform_list`). No colors are literal hex anywhere in this file — everything routes through `ActiveTheme`, itself backed by a JSON-configurable theme (re-skinning colors needs no Rust changes).

**Keyboard/focus**: `Focusable` returns the stored handle (2284-2291); `DataTable::render` wires `track_focus` + all action handlers. Full arrow/Home/End/PageUp/PageDown/Tab navigation with loop-around and Escape-to-clear. **No separate focus-ring concept independent of selection** — the table itself doesn't draw its own ring; the selected-row/cell overlay *is* the focus indicator, meaning you only need to design one indicator state and can drive it directly off the bevel-border channel.

**Virtualization — three separate, independently-implemented mechanisms**:
1. **Rows (vertical)**: gpui's native `uniform_list` (imported directly from `gpui`, not this crate's `virtual_list.rs`), called once in `Render::render` (2374-2439), tracked via `UniformListScrollHandle` (227,2443); requires uniform row height; also drives `load_more_if_need` and visible-range tracking from the same closure.
2. **Body columns (horizontal, per row)**: delegates to **the sibling `crate::virtual_list::virtual_list(...)`** (called at 2003-2111), tracked via `VirtualListScrollHandle` (228,264) — **this is the one explicit cross-dependency onto `virtual_list.rs` the brief asked to flag: `table/state.rs` cannot be lifted without also bringing `virtual_list.rs`.**
3. **Header columns (horizontal)**: a **third, bespoke, hand-rolled scheme** — `calculate_visible_leaf_col_range()` (1592-1639) manually scans column widths against scroll offset with a 200px overdraw buffer, justified by an explicit doc comment that rendering every header cell every frame "drops FPS below 60" past 1000+ columns (1650-1665).

**VERDICT for `table/` as a whole: REUSE BEHAVIOUR BUT RESTYLE.** `state.rs`'s selection/keyboard/resize/reorder/sort-hook/fixed-columns/three-axis-virtualization/load-more/export machinery is large, well-tested, genuinely valuable infrastructure that would take real effort to rebuild, and almost none of it cares about visuals — every color is theme-token-driven, the two radius calls (`state.rs:1482`, `data_table.rs:167`) are trivial to strip, and the selected-row indicator is already implemented as a themeable choice between flat-bg and bordered-overlay, with the overlay path a near-perfect seam for a two-tone bevel. Two costs to budget: (a) `table.rs`'s `MIN_CELL_WIDTH=100px` and `uniform_list`'s uniform-row-height requirement are minor layout constraints, not blockers; (b) taking `state.rs` means also taking the `crate::virtual_list` dependency.

---

### Summary for the reuse decision

**Genuinely reusable behavior, largely independent of visuals**: `table/delegate.rs`, `table/column.rs`, `form/`, the button/toggle/checkbox/radio state machines and keyboard handling, `Progress`/`ProgressCircle`'s animation/easing infrastructure, Accordion's spring height-animation, Sidebar's collapse/expand animation state machine, and every pure-math file in `plot/scale/` and the data-only parts of `plot/shape/` (`Pie::arcs`, `Stack::series`, `Sankey`'s layout solver).

**Nothing in the entire vendor tree supports chamfered corners natively** — confirmed by grep across the whole crate: every rounding call anywhere is a plain scalar `.rounded()`/`Corners`/`rounded_full_style()`. A "cut" visual language is new custom-painted geometry regardless of which component is wrapped, but the cost varies sharply: components that are already flat rectangles with theme-driven radius (Button, Toggle, Tag, GroupBox, Table, Sidebar chrome, Alert, Accordion) need only a handful of `.rounded()` calls forked per file; components whose *shape itself* is baked to a circle via `rounded_full_style()` (Avatar, Badge, Radio's dot, StepperTrigger's bubble) need that call replaced entirely, which is a bigger, more surgical change per component.

**Nothing supports a two-tone bevel border as an existing style axis** — every border color everywhere in the crate is one uniform `Hsla`. The best existing precedent to build from is `table/state.rs:2117-2150`'s selected/right-clicked-row overlay pattern (an absolutely-positioned div separating background-fill from border-draw) — it already has the right *shape* of seam (a themeable choice between flat-swap and bordered-overlay) and generalizes cleanly to two nested/offset border colors. `sidebar/menu.rs`, `header.rs`, `footer.rs`, and `list/list_item.rs` (shared by combobox/select) use flat background-only indicators today and would need that overlay pattern actively added, not just retheming a boolean.

**Animation is present in exactly three places and absent everywhere else within this section's scope** (`button/`, `chart/`, `plot/`, `table/`, `combobox.rs`/`select.rs`, `stepper/`, `time/`, leaf components): `accordion.rs`'s spring-based panel-height animation, `checkbox.rs`/`radio.rs`'s shared spring-based checkmark fade, and `progress/`'s `gpui::Animation`+`ease_in_out` value/loading tweening (`sidebar/mod.rs`'s `ease_in_out_cubic` width-collapse tweening is covered above in this same section). Every hover/active/selected/pressed transition elsewhere in this scope is an instant GPUI pseudo-class style swap (confirmed by grep). "Heavy animation" is a from-scratch layer for most of *this* cluster regardless of reuse strategy — but see the report-wide picture below, which adds several more animated components from outside this cluster.

## 34. Report-wide synthesis (all four research passes)

Pulling together §0.2, §3, §7, the "Animation / motion system" note after §18, the "Cross-cutting: `crate::animation`" note after §32, and §33's summary:

1. **Full animation inventory across the whole vendored crate**: keyframe/bezier one-shot transitions in `dialog.rs` (§23), `notification.rs` (§22), `popover.rs` (§16), `tooltip.rs` (§18), `tab/tab.rs`'s text-color fade (§12); raw `gpui::Animation`+built-in easing in `sheet.rs` (§24, no custom bezier), `skeleton.rs`/`spinner.rs` (§26-27), `progress/` (§33), and `accordion.rs`'s/`checkbox.rs`'s/`radio.rs`'s spring-based motion (§33); retarget-safe `Spring`/`spring()` physics in `switch.rs`/`slider.rs` (§28-29), `tab/tab_bar.rs`'s sliding indicator (§12), and `sidebar/mod.rs`'s collapse/expand (§33). That is a genuinely substantial existing animation surface to study and extend — "lots of animation" does not mean starting from zero, it means learning `gpui_base::animation`'s bezier/`EffectTransition` and `gpui_base::motion`'s `Spring`/`Transition` (§0's load-bearing fact: both live in the un-vendored `gpui_base` crate, reusable directly) and applying the same two techniques to the many components that currently have neither (`button/`, `menu/`, `command/`, `chart/`, `table/`, `combobox.rs`/`select.rs`, most leaf components).
2. **The chamfer/bevel visual language has no existing style-property shortcut anywhere in the crate** (confirmed independently by all four passes) but **does have a first-class low-level primitive to build one from**: `PathBuilder`/`canvas()`/`Window::paint_path()` (§0.2). The practical plan implied by every module's verdict is the same shape every time — build one shared "cut card" paint primitive (polygon fill + two-tone stroke) once, then swap it in wherever a component currently calls `.rounded()`/`rounded_full_style()`/`border_color()`, prioritizing by the cost tiers §33 lays out (cheap: flat-rect components like Button/Tag/GroupBox/Table/Sidebar chrome; expensive: shape-is-identity components like Avatar/Badge/Radio/StepperTrigger).
3. **The two-tone bevel border-as-focus-channel has one clear existing seam to extend**: `table/state.rs`'s themeable flat-bg-vs-bordered-overlay selection indicator (§33), which already separates "fill" from "border" as two composable layers — the same overlay-div technique is directly portable to `sidebar/menu.rs`/`header.rs`/`footer.rs` and `list/list_item.rs` (shared by List/Tree/Combobox/Select), all three of which currently only do a flat background swap and would need this pattern actively added rather than merely re-themed.
4. **One build-configuration fix is required before any of this matters for a docs reader**: the syntax highlighter is fully compiled out today (§7) — `gpui_component/tree-sitter` plus the seven `tree-sitter-<lang>` features for Rust/TS/JS/Python/Go/Java/C#/C++ must be turned on in a `Cargo.toml` before code blocks show any color at all, independent of any restyling work.
5. **Apps/desktop today uses a small fraction of the library's surface** (§2) — `Root`, `sheet::Sheet`, `dialog::Dialog`, `input::InputState`, `tree::Tree`, `popover::Popover`, `scroll::Scrollable`, `setting::*`, `tooltip::Tooltip`, `button::Button`, `list::ListItem`, `command::*` (search palette only), `progress::Progress` — and already has a working full-custom-theme installer (`apps/desktop/src/theme/mod.rs:333-397`, §4) that zeroes radius for a "hard cuts" look, which is the natural integration point for whatever chamfer/bevel primitive gets built.

