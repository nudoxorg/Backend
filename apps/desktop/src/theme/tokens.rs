//! Geometry and typography tokens.
//!
//! Every measurement in the application comes from this module, expressed in
//! `rem` so that one preference — the interface size — rescales the whole
//! window coherently instead of rescaling text against fixed-pixel chrome.
//! The spacing scale is a 4px grid at the default 16px root, the type scale is
//! a modular ladder, and both are closed enums so no view can invent a value.

use super::palette::Paint;
use gpui::{Pixels, Rems, px, rems};

/// The complete interaction-state vocabulary shared by controls and the
/// semantic capture tree. A view cannot invent a seventh visual state and a
/// disabled action cannot accidentally render as a normal one.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum ControlState {
    /// Resting, actionable control.
    #[default]
    Default,
    /// Pointer or keyboard hover target.
    Hover,
    /// Pointer press or keyboard activation.
    Pressed,
    /// Keyboard focus ring owner.
    Focus,
    /// Present but unavailable.
    Disabled,
    /// Current selection in a list, tree, or toggle group.
    Selected,
    /// Action is executing and retains its geometry.
    Loading,
    /// Action has a recoverable or terminal error.
    Error,
}

/// One fully specified visual state frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct StateFrame {
    /// The state this frame represents.
    pub(crate) state: ControlState,
    /// Surface role used by the component.
    pub(crate) surface: Paint,
    /// Text/icon role used by the component.
    pub(crate) foreground: Paint,
    /// Border/focus role used by the component.
    pub(crate) border: Paint,
    /// Opacity in hundredths, avoiding per-view floating-point literals.
    pub(crate) opacity_hundredths: u8,
    /// Motion regime used when entering this state.
    pub(crate) motion: Motion,
}

impl StateFrame {
    /// Returns the complete token frame for one interaction state.
    pub(crate) const fn for_state(state: ControlState) -> Self {
        match state {
            ControlState::Default => Self::new(
                state,
                Paint::Abyss1,
                Paint::Silver1,
                Paint::Rule1,
                100,
                Motion::Standard,
            ),
            ControlState::Hover => Self::new(
                state,
                Paint::Tint,
                Paint::Silver0,
                Paint::Focus,
                100,
                Motion::Standard,
            ),
            ControlState::Pressed => Self::new(
                state,
                Paint::PeriwinkleSoft,
                Paint::Silver0,
                Paint::Leaf,
                100,
                Motion::Instant,
            ),
            ControlState::Focus => Self::new(
                state,
                Paint::MintSoft,
                Paint::Silver0,
                Paint::Focus,
                100,
                Motion::Standard,
            ),
            ControlState::Disabled => Self::new(
                state,
                Paint::Abyss0,
                Paint::Silver3,
                Paint::Rule1,
                52,
                Motion::Instant,
            ),
            ControlState::Selected => Self::new(
                state,
                Paint::PeriwinkleSoft,
                Paint::Mint,
                Paint::Leaf,
                100,
                Motion::Standard,
            ),
            ControlState::Loading => Self::new(
                state,
                Paint::Abyss1,
                Paint::Silver2,
                Paint::Waiting,
                82,
                Motion::Emphasis,
            ),
            ControlState::Error => Self::new(
                state,
                Paint::Abyss1,
                Paint::Stopped,
                Paint::Stopped,
                100,
                Motion::Emphasis,
            ),
        }
    }

    const fn new(
        state: ControlState,
        surface: Paint,
        foreground: Paint,
        border: Paint,
        opacity_hundredths: u8,
        motion: Motion,
    ) -> Self {
        Self {
            state,
            surface,
            foreground,
            border,
            opacity_hundredths,
            motion,
        }
    }
}

/// A closed elevation vocabulary; shadows remain a theme decision rather than
/// ad hoc blur values in individual views.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum Elevation {
    /// Content resting on its parent surface.
    #[default]
    Flat,
    /// Cards and panels above the reading plane.
    Raised,
    /// Sheets, menus, and transient surfaces.
    Floating,
}

/// Motion tokens shared by normal, reduced-motion, and capture rendering.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum Motion {
    /// No interpolation; used by reduced-motion and pressed transitions.
    Instant,
    /// A 90ms glint or comb micro-transition.
    Micro,
    /// The ordinary 160ms control transition.
    Quick,
    #[default]
    /// The ordinary 240ms surface transition.
    Standard,
    /// Emphasis motion for state and wave feedback.
    Emphasis,
    /// The 620ms route descent transition.
    Scene,
}

impl Motion {
    /// Returns the canonical duration in milliseconds before reduced motion.
    pub(crate) const fn duration_ms(self) -> u16 {
        match self {
            Self::Instant => 0,
            Self::Micro => 90,
            Self::Quick => 160,
            Self::Standard => 240,
            Self::Emphasis => 380,
            Self::Scene => 620,
        }
    }
}

/// The amount of space a control gives its label and pointer target.
///
/// The reader is dense by default, but a touch sized surface is still a
/// supported rendering target. Keeping density in the token layer means a
/// component can change its hit target without inventing a second spacing
/// vocabulary.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum Density {
    /// The compact desktop layout used by the reference designs.
    #[default]
    Compact,
    /// A little more breathing room for low vision and narrow layouts.
    Comfortable,
    /// Pointer targets sized for touch and presentation capture.
    Spacious,
}

impl Density {
    /// Returns the multiplier applied to spacing tokens.
    pub(crate) const fn scale(self) -> f32 {
        match self {
            Self::Compact => 1.0,
            Self::Comfortable => 1.125,
            Self::Spacious => 1.25,
        }
    }

    /// Returns the next density, clamped at the spacious end.
    pub(crate) const fn stepped(self, up: bool) -> Self {
        match (self, up) {
            (Self::Compact, true) => Self::Comfortable,
            (Self::Comfortable, true) => Self::Spacious,
            (Self::Spacious, true) => Self::Spacious,
            (Self::Spacious, false) => Self::Comfortable,
            (Self::Comfortable, false) => Self::Compact,
            (Self::Compact, false) => Self::Compact,
        }
    }
}

/// Root font size, in pixels, at an interface size of 100%.
const ROOT_PIXELS: f32 = 16.0;

/// One step on the 4px spacing grid.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Space {
    /// 4px — the gap between a glyph and its label.
    Tight,
    /// 8px — the gap between fields in one row.
    Snug,
    /// 12px — the gap between rows in a list.
    Base,
    /// 16px — the padding inside a panel.
    Room,
    /// 20px — the gap between sections.
    Loose,
    /// 24px — the gutter between panels.
    Gutter,
    /// 32px — the margin around a page.
    Margin,
    /// 48px — the gap before a new page region.
    Bay,
}

/// Returns the `rem` length of one spacing step.
pub(crate) fn space(step: Space) -> Rems {
    space_at(step, Density::Compact)
}

/// Returns one spacing token at a chosen density.
pub(crate) fn space_at(step: Space, density: Density) -> Rems {
    rems(
        density.scale()
            * match step {
                Space::Tight => 0.25,
                Space::Snug => 0.5,
                Space::Base => 0.75,
                Space::Room => 1.0,
                Space::Loose => 1.25,
                Space::Gutter => 1.5,
                Space::Margin => 2.0,
                Space::Bay => 3.0,
            },
    )
}

/// One rung on the modular type ladder.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TypeScale {
    /// 10px — key tags and counts.
    Micro,
    /// 11px — status bar and chips.
    Tiny,
    /// 12px — secondary rows, signatures in lists.
    Small,
    /// 13px — the interface default.
    Interface,
    /// 15px — reading prose.
    Body,
    /// 18px — section headings.
    Section,
    /// 22px — page titles.
    Title,
    /// 46px — design-system facet and first-run hero titles.
    Hero,
}

/// Returns the `rem` size of one type rung.
pub(crate) fn type_size(scale: TypeScale) -> Rems {
    rems(match scale {
        TypeScale::Micro => 0.625,
        TypeScale::Tiny => 0.6875,
        TypeScale::Small => 0.75,
        TypeScale::Interface => 0.8125,
        TypeScale::Body => 0.875,
        TypeScale::Section => 1.25,
        TypeScale::Title => 1.875,
        TypeScale::Hero => 2.875,
    })
}

/// Returns the `rem` line height of one type rung.
///
/// Small rungs are set tight so dense rows stay scannable; reading rungs open
/// up to a comfortable measure.
pub(crate) fn line_height(scale: TypeScale) -> Rems {
    rems(match scale {
        TypeScale::Micro => 0.875,
        TypeScale::Tiny => 1.0,
        TypeScale::Small => 1.125,
        TypeScale::Interface => 1.25,
        TypeScale::Body | TypeScale::Section => 1.5,
        TypeScale::Title => 2.0,
        TypeScale::Hero => 3.125,
    })
}

/// A corner radius.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Radius {
    /// 3px — chips and key tags.
    Hair,
    /// 5px — rows and buttons.
    Small,
    /// 8px — panels and cards.
    Medium,
    /// 12px — floating sheets.
    Large,
    /// Fully rounded — the omnibar capsule.
    Capsule,
}

/// Returns the pixel radius of one corner token.
///
/// Radii stay in pixels: a corner is an optical constant, not a measure that
/// should grow with the reading size.
pub(crate) fn radius(corner: Radius) -> Pixels {
    px(match corner {
        Radius::Hair => 3.0,
        Radius::Small => 5.0,
        Radius::Medium => 8.0,
        Radius::Large => 12.0,
        Radius::Capsule => 999.0,
    })
}

/// Thickness of every border in the application.
pub(crate) fn hairline() -> Pixels {
    px(1.0)
}

/// The reading preference that rescales the whole window.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct InterfaceSize(u16);

impl InterfaceSize {
    /// The smallest supported interface size, in percent.
    pub(crate) const MIN: u16 = 80;
    /// The largest supported interface size, in percent.
    pub(crate) const MAX: u16 = 200;

    /// The unscaled interface size.
    pub(crate) const DEFAULT: Self = Self(100);

    /// Clamps a percentage into the supported range.
    pub(crate) fn percent(value: u16) -> Self {
        Self(value.clamp(Self::MIN, Self::MAX))
    }

    /// Returns the percentage.
    pub(crate) const fn get(self) -> u16 {
        self.0
    }

    /// Returns the root font size this preference implies.
    pub(crate) fn root_pixels(self) -> Pixels {
        px(ROOT_PIXELS * f32::from(self.0) / 100.0)
    }

    /// Returns this size stepped by one 10% notch, clamped to the range.
    pub(crate) fn stepped(self, up: bool) -> Self {
        let next = if up {
            self.0.saturating_add(10)
        } else {
            self.0.saturating_sub(10)
        };
        Self::percent(next)
    }
}

impl Default for InterfaceSize {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Panel width limits, in pixels.
///
/// Panels are sized in pixels rather than rems: a sidebar holds a fixed number
/// of characters of a path, and that budget should not change when prose is
/// resized for reading comfort.
pub(crate) struct PanelWidth;

impl PanelWidth {
    /// Narrowest usable library panel.
    pub(crate) const MIN_LIBRARY: f32 = 190.0;
    /// Widest useful library panel.
    pub(crate) const MAX_LIBRARY: f32 = 420.0;
    /// Default library panel width.
    pub(crate) const DEFAULT_LIBRARY: f32 = 268.0;
    /// Narrowest usable context panel.
    pub(crate) const MIN_CONTEXT: f32 = 190.0;
    /// Widest useful context panel.
    pub(crate) const MAX_CONTEXT: f32 = 420.0;
    /// Default context panel width.
    pub(crate) const DEFAULT_CONTEXT: f32 = 252.0;
    /// Width of a panel collapsed to its glyph rail.
    ///
    /// A collapsed panel is a rail, not an absence: a reader who folds the
    /// shelf away to read a wide signature still wants to see that a project
    /// is indexing, and twenty-six pixels of readiness marks says so.
    pub(crate) const RAIL: f32 = 26.0;
    /// Below this window width both panels auto-collapse.
    pub(crate) const AUTO_COLLAPSE_WINDOW: f32 = 1040.0;
    /// Below this window width the context panel auto-collapses.
    pub(crate) const AUTO_COLLAPSE_CONTEXT: f32 = 1240.0;
}

/// Fixed heights of the window's structural rows, in pixels.
pub(crate) struct Chrome;

impl Chrome {
    /// Titlebar height, tall enough to clear the macOS traffic lights.
    pub(crate) const TITLEBAR: f32 = 44.0;
    /// Status bar height.
    pub(crate) const STATUS: f32 = 24.0;
    /// Height of one outline row.
    pub(crate) const OUTLINE_ROW: f32 = 22.0;
    /// Maximum height of the omnibar sheet.
    pub(crate) const SHEET_MAX: f32 = 460.0;
    /// Width of the omnibar capsule.
    pub(crate) const OMNIBAR: f32 = 520.0;
    /// Width of the omnibar sheet.
    pub(crate) const SHEET: f32 = 720.0;
}

#[cfg(test)]
mod tests {
    use super::{ControlState, Density, InterfaceSize, Motion, Space, StateFrame, space, space_at};

    #[test]
    fn compact_density_preserves_the_reference_spacing_scale() {
        assert_eq!(space(Space::Base), space_at(Space::Base, Density::Compact));
    }

    #[test]
    fn density_only_grows_spacing_and_has_no_wraparound() {
        let compact = space_at(Space::Room, Density::Compact);
        let comfortable = space_at(Space::Room, Density::Comfortable);
        let spacious = space_at(Space::Room, Density::Spacious);
        assert!(comfortable.0 > compact.0);
        assert!(spacious.0 > comfortable.0);
        assert_eq!(Density::Compact.stepped(false), Density::Compact);
        assert_eq!(Density::Spacious.stepped(true), Density::Spacious);
    }

    #[test]
    fn interface_size_clamps_before_it_reaches_geometry() {
        assert_eq!(InterfaceSize::percent(0).get(), InterfaceSize::MIN);
        assert_eq!(InterfaceSize::percent(u16::MAX).get(), InterfaceSize::MAX);
    }

    #[test]
    fn every_control_state_has_an_explicit_complete_frame() {
        let states = [
            ControlState::Default,
            ControlState::Hover,
            ControlState::Pressed,
            ControlState::Focus,
            ControlState::Disabled,
            ControlState::Selected,
            ControlState::Loading,
            ControlState::Error,
        ];
        for state in states {
            let frame = StateFrame::for_state(state);
            assert_eq!(frame.state, state);
            assert!(frame.opacity_hundredths > 0);
            assert!(frame.motion.duration_ms() <= Motion::Emphasis.duration_ms());
        }
    }
}
