//! The application theme: the exact Facet palette plus the geometry tokens.
//!
//! The theme is a GPUI global rather than a field threaded through views, so a
//! stateless element builder can ask for a colour without being handed one.
//! No view in this application may write a colour literal; every tone comes
//! from [`Theme::paint`] or from one of the five fixed family hues.

pub(crate) mod fonts;
pub(crate) mod kind;
pub(crate) mod language;
pub(crate) mod palette;
pub(crate) mod ramp;
pub(crate) mod tokens;

use crate::core::layout::{LayoutCache, LayoutInput, ResponsiveLayout, TextScale};
use crate::ui::components::{ActionFrameToken, ActionFrames, ActionMetadata, ActionTree};
use gpui::{App, Global, Hsla, Pixels, SharedString};
use palette::{Appearance, Contrast, Paint, Palette};
use ramp::Hue;
use std::cell::RefCell;
use std::rc::Rc;
use tokens::{
    ControlState, Density, Elevation, InterfaceSize, Motion, Space, StateFrame, space_at,
};

/// The lit palette and the reading preferences that scale it.
#[derive(Clone, Debug)]
pub(crate) struct Theme {
    palette: Palette,
    interface: InterfaceSize,
    density: Density,
    contrast: Contrast,
    reduced_motion: bool,
    ui_face: SharedString,
    display_face: SharedString,
    serif_face: SharedString,
    specimen: SharedString,
    action_frames: Rc<ActionFrames>,
    action_frame: Option<ActionFrameToken>,
    action_parent: Option<SharedString>,
    modal_active: bool,
    layout_cache: Rc<RefCell<LayoutCache>>,
}

impl Global for Theme {}

impl Theme {
    /// Lights one appearance at one interface size.
    pub(crate) fn new(
        appearance: Appearance,
        interface: InterfaceSize,
        reduced_motion: bool,
    ) -> Self {
        Self::new_with_options(
            appearance,
            interface,
            Density::Compact,
            Contrast::Normal,
            reduced_motion,
        )
    }

    /// Lights a theme with explicit accessibility and density options.
    pub(crate) fn new_with_options(
        appearance: Appearance,
        interface: InterfaceSize,
        density: Density,
        contrast: Contrast,
        reduced_motion: bool,
    ) -> Self {
        Self {
            palette: Palette::new_with_contrast(appearance, contrast),
            interface,
            density,
            contrast,
            reduced_motion,
            ui_face: SharedString::new_static(fonts::UI_FAMILY),
            display_face: SharedString::new_static(fonts::DISPLAY_FAMILY),
            serif_face: SharedString::new_static(fonts::SERIF_FAMILY),
            specimen: SharedString::new_static(fonts::SPECIMEN_FAMILY),
            action_frames: Rc::new(ActionFrames::default()),
            action_frame: None,
            action_parent: None,
            modal_active: false,
            layout_cache: Rc::new(RefCell::new(LayoutCache::default())),
        }
    }

    /// Reuses the owning workspace's explicit per-window action collector.
    pub(crate) fn with_action_frames(mut self, frames: Rc<ActionFrames>) -> Self {
        self.action_frames = frames;
        self
    }

    /// Derives a builder theme whose controls are children of one semantic
    /// composite, such as a dialog or listbox. The action frame itself stays
    /// shared with the owning window.
    pub(crate) fn with_action_parent(&self, parent: impl Into<SharedString>) -> Self {
        let mut derived = self.clone();
        derived.action_parent = Some(parent.into());
        derived
    }

    /// Derives a builder theme for one control subtree while preserving the
    /// window's modal state. Dialog descendants remain tab stops when the
    /// document shell is inert.
    pub(crate) fn with_modal_state(mut self, active: bool) -> Self {
        self.modal_active = active;
        self
    }

    pub(crate) const fn modal_active(&self) -> bool {
        self.modal_active
    }

    /// Returns whether a control built by this theme belongs in native Tab
    /// traversal. Underlying document controls are removed while a dialog is
    /// open; dialog descendants retain their stops through `with_action_parent`.
    pub(crate) const fn control_tab_stop(&self) -> bool {
        !self.modal_active || self.action_parent.is_some()
    }

    /// Starts the semantic frame that receives all controls built from this
    /// theme, including controls created later by CE popover content.
    pub(crate) fn begin_action_frame(
        &mut self,
        window: &gpui::Window,
        route: impl Into<SharedString>,
    ) {
        self.action_frame = Some(self.action_frames.begin(window, route));
    }

    pub(crate) fn begin_action_frame_with_modal(
        &mut self,
        window: &gpui::Window,
        route: impl Into<SharedString>,
        modal_active: bool,
    ) {
        if modal_active && !self.modal_active {
            self.action_frames.remember_modal_restore(window);
        } else if !modal_active && self.modal_active {
            self.action_frames.request_modal_restore(window);
        }
        self.modal_active = modal_active;
        self.begin_action_frame(window, route);
    }

    /// Leaves the published frame available to the harness. A later frame
    /// replaces it atomically for this window.
    pub(crate) fn publish_action_frame(&self, window: &gpui::Window) {
        self.action_frames.finalize(window);
    }

    /// Adds one action to the current explicit window frame.
    pub(crate) fn register_action(&self, action: ActionMetadata) {
        let action = if action.parent_value().is_none() {
            self.action_parent
                .as_ref()
                .map_or(action.clone(), |parent| action.parent(parent.clone()))
        } else {
            action
        };
        if let Some(token) = self.action_frame {
            self.action_frames.register(token, action);
        }
    }

    /// Returns the current per-window frame token for post-layout observers.
    pub(crate) const fn action_frame_token(&self) -> Option<ActionFrameToken> {
        self.action_frame
    }

    /// Shares the frame collector with measurement boundaries attached to
    /// concrete GPUI elements.
    pub(crate) fn action_frames_handle(&self) -> Rc<ActionFrames> {
        self.action_frames.clone()
    }

    /// Returns rectangles measured during the current prepaint pass.
    pub(crate) fn action_bounds(
        &self,
        window: &gpui::Window,
    ) -> std::collections::HashMap<String, crate::ui::components::SemanticBounds> {
        self.action_frames.snapshot_bounds(window)
    }

    /// Returns the focus order measured from concrete GPUI controls in this
    /// frame. Callers must use this alongside [`Self::action_bounds`] so a
    /// metadata-only node cannot enter keyboard assertions.
    pub(crate) fn action_focus_order(
        &self,
        window: &gpui::Window,
    ) -> std::collections::HashMap<String, u32> {
        self.action_frames.snapshot_focus_order(window)
    }

    /// Returns the stable action id whose rendered control owned focus when
    /// the post-layout registry observed the frame.
    pub(crate) fn action_focus_owner(&self, window: &gpui::Window) -> Option<String> {
        self.action_frames.snapshot_focus_owner(window)
    }

    /// Returns the action id proven by a concrete GPUI focus handle during the
    /// current render pass. This is separate from the app-declared owner.
    pub(crate) fn action_native_focus_owner(&self, window: &gpui::Window) -> Option<String> {
        self.action_frames.snapshot_native_focus_owner(window)
    }

    /// Returns the current frame for a screenshot or accessibility harness.
    pub(crate) fn action_tree(&self, window: &gpui::Window) -> ActionTree {
        self.action_frames.snapshot(window)
    }

    /// Drops one window's published frame after a scenario completes.
    pub(crate) fn reset_action_tree(&self, window: &gpui::Window) {
        self.action_frames.reset(window);
    }

    /// Returns the colour for one semantic role.
    pub(crate) fn paint(&self, role: Paint) -> Hsla {
        self.palette.paint(role)
    }

    /// Returns a hue rendered on the single chromatic plane.
    pub(crate) fn on_plane(&self, hue: Hue) -> Hsla {
        self.palette.on_plane(hue)
    }

    /// Returns a translucent wash of one hue on the chromatic plane.
    pub(crate) fn plane_wash(&self, hue: Hue, alpha: f32) -> Hsla {
        self.palette.plane_wash(hue, alpha)
    }

    /// Returns the root font size implied by the reading size.
    pub(crate) fn root_pixels(&self) -> Pixels {
        self.interface.root_pixels()
    }

    /// Returns the active text scale consumed by the responsive resolver.
    pub(crate) const fn interface_scale_percent(&self) -> u16 {
        self.interface.get()
    }

    /// Resolves one measured shell input through the shared cache.
    pub(crate) fn responsive_layout(&self, input: LayoutInput) -> ResponsiveLayout {
        self.layout_cache.borrow_mut().resolve(input)
    }

    /// Returns the typed resolver scale without exposing theme internals.
    pub(crate) fn layout_text_scale(&self) -> TextScale {
        TextScale::percent(self.interface_scale_percent())
    }

    /// Returns the spacing token at this theme's responsive density.
    pub(crate) fn space(&self, token: Space) -> gpui::Rems {
        space_at(token, self.density)
    }

    /// Returns the active density.
    pub(crate) const fn density(&self) -> Density {
        self.density
    }

    /// Returns the active contrast treatment.
    pub(crate) const fn contrast(&self) -> Contrast {
        self.contrast
    }

    /// Returns whether high contrast is enabled.
    pub(crate) const fn high_contrast(&self) -> bool {
        self.contrast.is_high()
    }

    /// Returns whether motion is suppressed.
    pub(crate) const fn reduced_motion(&self) -> bool {
        self.reduced_motion
    }

    /// Returns the complete visual contract for one control state.
    pub(crate) const fn state_frame(&self, state: ControlState) -> StateFrame {
        StateFrame::for_state(state)
    }

    /// Returns the canonical elevation level for a surface.
    pub(crate) const fn elevation(&self, level: Elevation) -> Elevation {
        level
    }

    /// Returns a motion token, collapsing every animated transition when the
    /// reader has opted into reduced motion.
    pub(crate) const fn motion(&self, motion: Motion) -> Motion {
        if self.reduced_motion {
            Motion::Instant
        } else {
            motion
        }
    }

    /// Returns the system UI face used by controls and navigation.
    pub(crate) fn ui_face(&self) -> SharedString {
        self.ui_face.clone()
    }

    /// Returns the display face used by page titles and headings.
    pub(crate) fn display_face(&self) -> SharedString {
        self.display_face.clone()
    }

    /// Returns the serif face reserved for reading prose.
    pub(crate) fn serif_face(&self) -> SharedString {
        self.serif_face.clone()
    }

    /// Returns the monospace family used for signatures and source.
    pub(crate) fn specimen(&self) -> SharedString {
        self.specimen.clone()
    }

    /// Returns the active appearance for semantic visual probes.
    pub(crate) const fn appearance(&self) -> Appearance {
        self.palette.appearance()
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::new(Appearance::Abyss, InterfaceSize::DEFAULT, false)
    }
}

/// Returns the lit theme, installing the default one if none is set.
pub(crate) fn theme(cx: &App) -> Theme {
    cx.try_global::<Theme>().cloned().unwrap_or_default()
}

/// Projects Nudox semantic roles onto GPUI CE's behavior/component theme.
///
/// GPUI CE owns focus rings, popovers, tooltips, inputs, scrollbars, and
/// virtual lists. It must see the same palette as Nudox-owned elements or a
/// sheet would change colour when it crosses a component boundary. The
/// projection intentionally sets only semantic roles; component geometry and
/// interaction behavior stay with GPUI CE.
pub(crate) fn sync_components(cx: &mut App, theme: &Theme) {
    {
        let component = gpui_component::Theme::global_mut(cx);
        component.background = theme.paint(Paint::Abyss0);
        component.foreground = theme.paint(Paint::Silver1);
        component.muted = theme.paint(Paint::Tint);
        component.muted_foreground = theme.paint(Paint::Silver2);
        component.border = theme.paint(Paint::Rule1);
        component.input = theme.paint(Paint::Rule3);
        component.ring = theme.paint(Paint::Focus);
        component.primary = theme.paint(Paint::Mint);
        component.primary_hover = theme.paint(Paint::Teal);
        component.primary_active = theme.paint(Paint::Leaf);
        component.primary_foreground = theme.paint(Paint::MintInk);
        component.secondary = theme.paint(Paint::Abyss1);
        component.secondary_hover = theme.paint(Paint::Tint);
        component.secondary_active = theme.paint(Paint::PeriwinkleSoft);
        component.secondary_foreground = theme.paint(Paint::Silver1);
        component.popover = theme.paint(Paint::Abyss2);
        component.popover_foreground = theme.paint(Paint::Silver1);
        component.accent = theme.paint(Paint::PeriwinkleSoft);
        component.accent_foreground = theme.paint(Paint::Periwinkle);
        component.danger = theme.paint(Paint::Stopped);
        component.danger_foreground = theme.paint(Paint::Silver0);
        component.warning = theme.paint(Paint::Waiting);
        component.warning_foreground = theme.paint(Paint::Silver0);
        component.info = theme.paint(Paint::Periwinkle);
        component.info_foreground = theme.paint(Paint::Silver0);
        component.success = theme.paint(Paint::Action);
        component.success_foreground = theme.paint(Paint::Silver0);
        component.list.active_highlight = true;
        component.list_hover = theme.paint(Paint::Tint);
        component.list_active = theme.paint(Paint::PeriwinkleSoft);
        component.list_active_border = theme.paint(Paint::Leaf);
        component.sidebar = theme.paint(Paint::Abyss1);
        component.sidebar_foreground = theme.paint(Paint::Silver1);
        component.sidebar_border = theme.paint(Paint::Rule1);
        component.title_bar = theme.paint(Paint::Abyss1);
        component.title_bar_border = theme.paint(Paint::Rule1);
        component.status_bar = theme.paint(Paint::Abyss0);
        component.status_bar_border = theme.paint(Paint::Rule1);
        component.overlay = theme.paint(Paint::Veil);
        component.scrollbar = theme.paint(Paint::Abyss0);
        component.scrollbar_thumb = theme.paint(Paint::Silver3);
        component.scrollbar_thumb_hover = theme.paint(Paint::Silver2);
        component.font_family = theme.ui_face();
        component.mono_font_family = theme.specimen();
        // Facet surfaces use hard cuts. Setting the CE theme radii to zero
        // keeps inputs, popovers and component-owned buttons in the same
        // square grammar as application-owned plates.
        component.radius = Pixels::ZERO;
        component.radius_lg = Pixels::ZERO;
        component.focus_ring = true;
    }
    gpui_component::Theme::sync_base(cx);
}
