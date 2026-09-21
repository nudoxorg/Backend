//! The application theme: one generated palette plus the geometry tokens.
//!
//! The theme is a GPUI global rather than a field threaded through views, so a
//! stateless element builder can ask for a colour without being handed one.
//! No view in this application may write a colour literal; every tone comes
//! from [`Theme::paint`] or from a hue placed on the chromatic plane.

pub(crate) mod fonts;
pub(crate) mod kind;
pub(crate) mod language;
pub(crate) mod palette;
pub(crate) mod ramp;
pub(crate) mod tokens;

use crate::ui::components::{ActionFrameToken, ActionFrames, ActionMetadata, ActionTree};
use gpui::{App, Global, Hsla, Pixels, SharedString};
use palette::{Appearance, Contrast, Paint, Palette};
use ramp::Hue;
use std::rc::Rc;
use tokens::{
    ControlState, Density, Elevation, InterfaceSize, Motion, Space, StateFrame, radius, space_at,
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
        }
    }

    /// Reuses the owning workspace's explicit per-window action collector.
    pub(crate) fn with_action_frames(mut self, frames: Rc<ActionFrames>) -> Self {
        self.action_frames = frames;
        self
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

    /// Leaves the published frame available to the harness. A later frame
    /// replaces it atomically for this window.
    pub(crate) fn publish_action_frame(&self, _window: &gpui::Window) {}

    /// Adds one action to the current explicit window frame.
    pub(crate) fn register_action(&self, action: ActionMetadata) {
        if let Some(token) = self.action_frame {
            self.action_frames.register(token, action);
        }
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
        Self::new(Appearance::Ink, InterfaceSize::DEFAULT, false)
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
        component.background = theme.paint(Paint::Ground);
        component.foreground = theme.paint(Paint::Text);
        component.muted = theme.paint(Paint::Hover);
        component.muted_foreground = theme.paint(Paint::TextDim);
        component.border = theme.paint(Paint::Hairline);
        component.input = theme.paint(Paint::HairlineStrong);
        component.ring = theme.paint(Paint::Focus);
        component.primary = theme.paint(Paint::GiltWash);
        component.primary_hover = theme.paint(Paint::Hover);
        component.primary_active = theme.paint(Paint::Selected);
        component.primary_foreground = theme.paint(Paint::Gilt);
        component.secondary = theme.paint(Paint::Panel);
        component.secondary_hover = theme.paint(Paint::Hover);
        component.secondary_active = theme.paint(Paint::Selected);
        component.secondary_foreground = theme.paint(Paint::Text);
        component.popover = theme.paint(Paint::Raised);
        component.popover_foreground = theme.paint(Paint::Text);
        component.accent = theme.paint(Paint::GiltWash);
        component.accent_foreground = theme.paint(Paint::Gilt);
        component.danger = theme.paint(Paint::Fault);
        component.danger_foreground = theme.paint(Paint::TextStrong);
        component.warning = theme.paint(Paint::Caution);
        component.warning_foreground = theme.paint(Paint::TextStrong);
        component.info = theme.paint(Paint::Info);
        component.info_foreground = theme.paint(Paint::TextStrong);
        component.success = theme.paint(Paint::Ok);
        component.success_foreground = theme.paint(Paint::TextStrong);
        component.list.active_highlight = true;
        component.list_hover = theme.paint(Paint::Hover);
        component.list_active = theme.paint(Paint::Selected);
        component.list_active_border = theme.paint(Paint::GiltDim);
        component.sidebar = theme.paint(Paint::Panel);
        component.sidebar_foreground = theme.paint(Paint::Text);
        component.sidebar_border = theme.paint(Paint::Hairline);
        component.title_bar = theme.paint(Paint::Panel);
        component.title_bar_border = theme.paint(Paint::Hairline);
        component.status_bar = theme.paint(Paint::Sunken);
        component.status_bar_border = theme.paint(Paint::Hairline);
        component.overlay = theme.paint(Paint::Scrim);
        component.scrollbar = theme.paint(Paint::Sunken);
        component.scrollbar_thumb = theme.paint(Paint::TextFaint);
        component.scrollbar_thumb_hover = theme.paint(Paint::TextDim);
        component.font_family = theme.ui_face();
        component.mono_font_family = theme.specimen();
        component.radius = radius(tokens::Radius::Small);
        component.radius_lg = radius(tokens::Radius::Large);
        component.focus_ring = true;
    }
    gpui_component::Theme::sync_base(cx);
}
