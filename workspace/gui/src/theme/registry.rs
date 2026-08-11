//! `ThemeRegistry` — every bundled theme, which one is live, and how to change
//! that without leaving a stale colour anywhere in the window.
//!
//! # The two-globals problem this fixes
//!
//! The application has always had two theme globals: gpui-component's `Theme`,
//! which colours everything gpui-component draws (buttons, `Kbd` caps, icons,
//! scrollbars, the dock's own tab bars and panel headers), and
//! [`NudoxThemeExt`], which colours everything this crate draws. They were
//! layered the wrong way round — `NudoxThemeExt::init` *read*
//! `cx.theme().is_dark()` and picked one of two hard-coded themes from it — so
//! gpui-component was upstream and our palette was a reaction to it.
//!
//! The visible consequence is in `.shots/fixtures/04-search-hits.png`: the
//! `⏎`, `⌘⏎`, `Tab` caps along the overlay's footer are `Kbd` elements filled
//! with gpui-component's `muted` on gpui-component's `muted_foreground`, which
//! no nudox theme has ever had a say in. They are the right *kind* of grey by
//! luck. Any theme with a warm neutral would have had cold grey key caps in
//! the middle of a warm panel, and nothing in this crate could have stopped it.
//!
//! Now the palette is upstream of both. [`apply`] resolves a [`ThemeSpec`] once
//! and writes it into *both* globals, so gpui-component's chrome is downstream
//! of the same twenty-three numbers as everything else.
//!
//! # Why switching is complete
//!
//! Three things have to be true, and all three are now structural rather than
//! remembered:
//!
//! 1. **Nothing caches a resolved colour.** Every view reads `cx.theme_ext()`
//!    inside `render`. A sweep of `src/` for `Hsla`-typed fields found three,
//!    all on `RenderOnce` values rebuilt per frame. This is the same property
//!    zed relies on (`crates/theme_settings/src/theme_settings.rs::reload_theme`
//!    is just `update_theme` + `refresh_windows`), and for the same reason:
//!    GPUI rebuilds the element tree each frame, so there is nowhere for a
//!    colour to persist.
//! 2. **Both globals move together.** [`apply`] writes them in one call. A
//!    caller cannot update one and forget the other, because there is no
//!    public way to update one.
//! 3. **Every window redraws.** `cx.refresh_windows()` marks all windows dirty.
//!
//! `tests/theme_law.rs` pins (1) by photographing the window in one theme,
//! switching, photographing again, and asserting that no colour with a
//! meaningful share of the first frame survives into the second.

use gpui::{App, Global, SharedString};
use gpui_component::{Theme, ThemeMode};

use crate::theme::ext::NudoxThemeExt;
use crate::theme::palette::{Appearance, Palette, Step};
use crate::theme::resolve::resolve;
use crate::theme::spec::{ThemeLoadError, ThemeSpec, parse_bundled};

// ─────────────────────────────────────────────────────────────────────────────
// Registry
// ─────────────────────────────────────────────────────────────────────────────

/// The set of themes the application can be in, and which one it is in.
///
/// Held as a GPUI global rather than threaded through views because "which
/// theme is live" is a property of the process, not of any window — a second
/// window opened after a cycle must come up in the cycled-to theme, which is
/// exactly the bug a per-window copy would create.
#[derive(Debug)]
pub struct ThemeRegistry {
    specs: Vec<ThemeSpec>,
    active: usize,
}

impl Global for ThemeRegistry {}

impl ThemeRegistry {
    /// Parse the bundled themes, install the registry, and apply the first one.
    ///
    /// Returns the typed error rather than panicking so `main` can report which
    /// theme file is broken. A malformed bundled theme is a build-time mistake
    /// that reaches runtime, and the one thing worse than failing is starting
    /// with an arbitrary substitute and letting the reader wonder why their
    /// theme looks wrong.
    pub fn init(cx: &mut App) -> Result<(), ThemeLoadError> {
        let specs = parse_bundled()?;
        cx.set_global(ThemeRegistry { specs, active: 0 });
        apply(cx, 0);
        Ok(())
    }

    /// Install a registry over an explicit spec list. Tests use this to pin a
    /// theme without depending on the bundle's order.
    pub fn init_with(cx: &mut App, specs: Vec<ThemeSpec>) -> Result<(), ThemeLoadError> {
        if specs.is_empty() {
            return Err(ThemeLoadError::Empty);
        }
        cx.set_global(ThemeRegistry { specs, active: 0 });
        apply(cx, 0);
        Ok(())
    }

    /// The live theme's spec.
    pub fn active(&self) -> &ThemeSpec {
        // `active` is only ever written by `set_active`/`cycle`, both of which
        // take it modulo `specs.len()`, and `specs` is non-empty by
        // construction (both constructors reject an empty list). The index
        // therefore cannot be out of range.
        &self.specs[self.active]
    }

    /// Position of the live theme in cycle order — for "3 of 4" style chrome.
    pub fn active_index(&self) -> usize {
        self.active
    }

    /// How many themes are installed.
    pub fn len(&self) -> usize {
        self.specs.len()
    }

    /// Whether the registry holds no themes. Cannot happen through either
    /// constructor; present because clippy asks for it beside `len`.
    pub fn is_empty(&self) -> bool {
        self.specs.is_empty()
    }

    /// Every theme's key, in cycle order.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.specs.iter().map(|s| s.key.as_str())
    }

    /// Advance to the next theme and apply it. Wraps.
    ///
    /// Returns the name of the theme now live, so the caller can say what
    /// happened rather than leaving the reader to infer it from the colours.
    pub fn cycle(cx: &mut App) -> SharedString {
        let next = {
            let reg = cx.global::<ThemeRegistry>();
            (reg.active + 1) % reg.specs.len()
        };
        Self::set_active(cx, next)
    }

    /// Go back one theme. Wraps.
    pub fn cycle_back(cx: &mut App) -> SharedString {
        let prev = {
            let reg = cx.global::<ThemeRegistry>();
            (reg.active + reg.specs.len() - 1) % reg.specs.len()
        };
        Self::set_active(cx, prev)
    }

    /// Make the theme at `index` live. Out-of-range indices wrap rather than
    /// panic, because the only callers are cycle arithmetic and tests.
    pub fn set_active(cx: &mut App, index: usize) -> SharedString {
        let index = {
            let reg = cx.global_mut::<ThemeRegistry>();
            let i = index % reg.specs.len();
            reg.active = i;
            i
        };
        apply(cx, index);
        cx.global::<ThemeRegistry>().active().name.clone().into()
    }

    /// Make the theme with this key live. Returns `false` — and changes
    /// nothing — if no theme has that key.
    pub fn activate_key(cx: &mut App, key: &str) -> bool {
        let Some(index) = cx
            .global::<ThemeRegistry>()
            .specs
            .iter()
            .position(|s| s.key == key)
        else {
            return false;
        };
        Self::set_active(cx, index);
        true
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Apply
// ─────────────────────────────────────────────────────────────────────────────

/// Resolve the theme at `index` and write it into both globals.
///
/// Private on purpose: the only way to change the theme is through
/// [`ThemeRegistry`], which keeps `active` and the globals in step. An
/// `apply`-shaped public function is how the two-globals drift happened.
fn apply(cx: &mut App, index: usize) {
    let spec = cx.global::<ThemeRegistry>().specs[index].clone();
    let ext = resolve(&spec);
    let palette = ext.palette;

    // gpui-component first: its `Theme::change` rewrites the whole colour set
    // from its own config, so anything we project must be written after it.
    let mode = match spec.appearance {
        Appearance::Light => ThemeMode::Light,
        Appearance::Dark => ThemeMode::Dark,
    };
    Theme::change(mode, None, cx);
    project_onto_gpui_component(cx, &palette, &ext);

    cx.set_global(ext);
    cx.refresh_windows();
}

/// Write our palette over gpui-component's colour table.
///
/// # Why every field and not just the ones we noticed
///
/// Because "the ones we noticed" is a list that goes stale the moment anyone
/// uses a new gpui-component element. The failure is silent — a `Switch` or a
/// `Slider` added next year renders in whatever gpui-component's default
/// happened to be, which is a *plausible* colour, so nobody looks. Projecting
/// the whole table means a component we have never used still comes up in the
/// live theme.
///
/// The assignments mirror [`crate::theme::resolve`]'s: the same step means the
/// same thing on both sides of the seam.
fn project_onto_gpui_component(cx: &mut App, p: &Palette, ext: &NudoxThemeExt) {
    let c = ext.colours;
    let al = ext.alpha;
    let n = &p.neutral;
    let a = &p.accent;
    let t = Theme::global_mut(cx);

    // ── Base surfaces and text ───────────────────────────────────────────────
    t.background = c.bg_base;
    t.foreground = c.fg_default;
    t.border = c.border_default;
    t.muted = n.get(Step::ElementBg);
    t.muted_foreground = c.fg_muted;
    t.popover = c.bg_overlay;
    t.popover_foreground = c.fg_default;
    t.overlay = c.scrim;
    t.window_border = c.border_default;
    t.selection = c.accent_wash;
    t.caret = c.accent;
    t.ring = c.ring;
    t.skeleton = n.get(Step::ElementHover);
    t.drag_border = c.accent;
    t.drop_target = c.accent_wash;

    // ── Primary / accent / secondary ─────────────────────────────────────────
    t.primary = c.accent;
    t.primary_hover = c.accent_hover;
    t.primary_active = a.get(Step::SolidHover);
    t.primary_foreground = c.accent_fg_on;
    t.button_primary = c.accent;
    t.button_primary_hover = c.accent_hover;
    t.button_primary_active = a.get(Step::SolidHover);
    t.button_primary_foreground = c.accent_fg_on;
    t.accent = c.accent_wash;
    t.accent_foreground = c.fg_default;
    t.secondary = n.get(Step::ElementBg);
    t.secondary_hover = n.get(Step::ElementHover);
    t.secondary_active = n.get(Step::ElementActive);
    t.secondary_foreground = c.fg_default;
    t.link = c.accent_text;
    t.link_hover = c.accent;
    t.link_active = c.accent_hover;
    t.input = n.get(Step::ElementBg);

    // ── Status families ──────────────────────────────────────────────────────
    t.danger = c.danger;
    t.danger_hover = p.danger.get(Step::SolidHover);
    t.danger_active = p.danger.get(Step::SolidHover);
    t.danger_foreground = c.danger_fg;
    t.warning = c.warn;
    t.warning_hover = p.warn.get(Step::SolidHover);
    t.warning_active = p.warn.get(Step::SolidHover);
    t.warning_foreground = c.warn_fg;
    t.success = c.ok;
    t.success_hover = p.ok.get(Step::SolidHover);
    t.success_active = p.ok.get(Step::SolidHover);
    t.success_foreground = c.ok_fg;
    t.info = c.info;
    t.info_hover = p.info.get(Step::SolidHover);
    t.info_active = p.info.get(Step::SolidHover);
    t.info_foreground = c.info_fg;

    // ── Chrome: title bar, status bar, tabs, sidebar ─────────────────────────
    // All of these are `bg_raised`, deliberately. The frame audit found docks,
    // the tab bar, the title bar and the status bar each on a slightly
    // different plane, which is what made the window read as assembled rather
    // than designed. Chrome is one plane; content is another.
    t.title_bar = c.bg_raised;
    t.title_bar_border = c.border_subtle;
    t.status_bar = c.bg_raised;
    t.status_bar_border = c.border_subtle;
    t.tab_bar = c.bg_raised;
    t.tab_bar_segmented = n.get(Step::ElementBg);
    t.tab = c.bg_raised;
    t.tab_foreground = c.fg_muted;
    t.tab_active = c.bg_base;
    t.tab_active_foreground = c.fg_default;
    t.sidebar = c.bg_raised;
    t.sidebar_foreground = c.fg_default;
    t.sidebar_border = c.border_subtle;
    t.sidebar_accent = c.accent_wash;
    t.sidebar_accent_foreground = c.fg_default;
    t.sidebar_primary = c.accent;
    t.sidebar_primary_foreground = c.accent_fg_on;
    t.tiles = c.bg_sunken;
    t.group_box = c.bg_raised;
    t.group_box_foreground = c.fg_default;
    t.accordion = c.bg_raised;
    t.accordion_hover = c.bg_hover;
    t.description_list_label = n.get(Step::ElementBg);
    t.description_list_label_foreground = c.fg_muted;

    // ── Lists and tables ─────────────────────────────────────────────────────
    // `list_even`/`table_even` are the zebra stripe: `SubtleBg`, which is the
    // step whose entire job is "almost the page".
    // `t.list` is `Theme::list: ListSettings`, not the colour — `Theme` derefs
    // to `ThemeColor`, and the outer field wins. Reaching through `colors`
    // explicitly is the only way to name the colour, and the compiler catching
    // it here is the reason this projection is worth writing out in full.
    t.colors.list = c.bg_base;
    t.list_even = n.get(Step::SubtleBg);
    t.list_hover = c.bg_hover;
    t.list_active = c.accent_wash;
    t.list_active_border = c.accent;
    t.list_head = c.bg_raised;
    t.table = c.bg_base;
    t.table_even = n.get(Step::SubtleBg);
    t.table_hover = c.bg_hover;
    t.table_active = c.accent_wash;
    t.table_active_border = c.accent;
    t.table_head = c.bg_raised;
    t.table_head_foreground = c.fg_muted;
    t.table_foot = c.bg_raised;
    t.table_foot_foreground = c.fg_muted;
    t.table_row_border = c.border_subtle;

    // ── Controls ─────────────────────────────────────────────────────────────
    t.progress_bar = c.accent;
    t.slider_bar = c.accent;
    t.slider_thumb = c.bg_overlay;
    t.switch = n.get(Step::BorderStrong);
    t.switch_thumb = c.bg_overlay;
    t.scrollbar = gpui::transparent_black();
    t.scrollbar_thumb = n.at(Step::BorderStrong, al.veil);
    t.scrollbar_thumb_hover = n.at(Step::BorderStrong, al.dim);

    // ── Geometry that must match ours ────────────────────────────────────────
    // gpui-component rounds its own elements with these; leaving them at their
    // defaults is why a `Kbd` cap and a nudox chip beside it had different
    // radii in every screenshot.
    t.radius = ext.space.r_md;
    t.radius_lg = ext.space.r_lg;
    t.shadow = !p.appearance.is_dark();

    // ── Charts and the ANSI-ish palette ──────────────────────────────────────
    // Not used by this application today. Projected anyway so that the first
    // component that does use one comes up themed rather than in
    // gpui-component's defaults, which is the silent-staleness case above.
    t.chart_1 = p.kinds[3];
    t.chart_2 = p.kinds[1];
    t.chart_3 = p.kinds[5];
    t.chart_4 = p.kinds[4];
    t.chart_5 = p.kinds[9];
    t.chart_bullish = c.ok;
    t.chart_bearish = c.danger;
    t.red = c.danger;
    t.red_light = p.danger.get(Step::ElementActive);
    t.green = c.ok;
    t.green_light = p.ok.get(Step::ElementActive);
    t.blue = c.info;
    t.blue_light = p.info.get(Step::ElementActive);
    t.yellow = c.warn;
    t.yellow_light = p.warn.get(Step::ElementActive);
    t.magenta = p.kinds[9];
    t.magenta_light = p.kinds[10];
    t.cyan = p.kinds[0];
    t.cyan_light = p.kinds[11];
}
