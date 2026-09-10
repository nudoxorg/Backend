//! Defines the motion system for `interface-gui`.
//! This module owns every value that changes over time and the frame budget they imply.
//! Its narrow surface guarantees an idle window asks the compositor for nothing.
//!
//! # The idle invariant
//!
//! Nothing in this crate calls `request_animation_frame` on a timer. A frame is requested only
//! when [`Motion::advance`] reports that at least one spring or reveal is still moving. When the
//! reader stops interacting, every spring reaches rest, `advance` returns [`Frames::Idle`], and
//! the window goes completely quiet until the next event.

mod reveal;
mod spring;
mod tokens;

pub use reveal::{Phase, Reveal};
pub use spring::{Spring, SpringParams};
pub use tokens::{MotionPreference, Preset, Timing, WATCH_INTERVAL};

use core::time::Duration;

/// Whether the window needs another frame after an advance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Frames {
    /// Something is still moving; ask for another frame.
    Wanted,
    /// Everything has settled; ask for nothing.
    Idle,
}

impl Frames {
    /// Whether another frame is wanted.
    #[must_use]
    pub const fn wanted(self) -> bool {
        matches!(self, Self::Wanted)
    }

    /// Combines two advances, wanting a frame if either does.
    #[must_use]
    pub const fn or(self, other: Self) -> Self {
        match (self, other) {
            (Self::Idle, Self::Idle) => Self::Idle,
            _ => Self::Wanted,
        }
    }

    /// Lifts an advance's boolean answer.
    #[must_use]
    pub const fn from_moving(moving: bool) -> Self {
        if moving { Self::Wanted } else { Self::Idle }
    }
}

/// Every animated value the shell owns, advanced as one unit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Motion {
    preference: MotionPreference,
    library_width: Spring,
    outline_width: Spring,
    selection_nib: Spring,
    palette: Reveal,
    add_flow: Reveal,
}

/// How wide the library panel is when open.
pub const LIBRARY_PANEL_WIDTH: f32 = 292.0;

/// How wide the context panel is when open.
pub const OUTLINE_PANEL_WIDTH: f32 = 236.0;

impl Motion {
    /// Builds the motion set from restored preferences.
    #[must_use]
    pub fn restored(preference: MotionPreference, library_open: bool, outline_open: bool) -> Self {
        let library = if library_open { LIBRARY_PANEL_WIDTH } else { 0.0 };
        let outline = if outline_open { OUTLINE_PANEL_WIDTH } else { 0.0 };
        Self {
            preference,
            library_width: Spring::resting(library, Preset::Default.params()),
            outline_width: Spring::resting(outline, Preset::Default.params()),
            selection_nib: Spring::resting(0.0, Preset::Snappy.params()),
            palette: Reveal::closed(Timing::Reveal),
            add_flow: Reveal::closed(Timing::Disclosure),
        }
    }

    /// The reader's motion preference.
    #[must_use]
    pub const fn preference(&self) -> MotionPreference {
        self.preference
    }

    /// Replaces the motion preference, snapping everything in flight when motion is removed.
    pub fn set_preference(&mut self, preference: MotionPreference) {
        self.preference = preference;
        if !preference.animates() {
            self.library_width.snap_to(self.library_width.target());
            self.outline_width.snap_to(self.outline_width.target());
            self.selection_nib.snap_to(self.selection_nib.target());
            self.palette.set(self.palette.is_opening(), preference);
            self.add_flow.set(self.add_flow.is_opening(), preference);
        }
    }

    /// The library panel's present width in pixels.
    #[must_use]
    pub const fn library_width(&self) -> f32 {
        self.library_width.value()
    }

    /// The context panel's present width in pixels.
    #[must_use]
    pub const fn outline_width(&self) -> f32 {
        self.outline_width.value()
    }

    /// Where the selection nib currently sits, in pixels from the top of its list.
    #[must_use]
    pub const fn selection_nib(&self) -> f32 {
        self.selection_nib.value()
    }

    /// The palette's reveal.
    #[must_use]
    pub const fn palette(&self) -> Reveal {
        self.palette
    }

    /// The inline add flow's reveal.
    #[must_use]
    pub const fn add_flow(&self) -> Reveal {
        self.add_flow
    }

    /// Opens or closes the library panel.
    pub fn set_library_open(&mut self, open: bool) {
        self.retarget_panel(open, LIBRARY_PANEL_WIDTH, PanelSide::Library);
    }

    /// Opens or closes the context panel.
    pub fn set_outline_open(&mut self, open: bool) {
        self.retarget_panel(open, OUTLINE_PANEL_WIDTH, PanelSide::Outline);
    }

    /// Moves the selection nib to a new offset.
    pub fn move_nib(&mut self, offset: f32) {
        if self.preference.animates() {
            self.selection_nib.animate_to(offset);
        } else {
            self.selection_nib.snap_to(offset);
        }
    }

    /// Shows or hides the palette.
    pub fn set_palette_open(&mut self, open: bool) {
        self.palette.set(open, self.preference);
    }

    /// Shows or hides the inline add flow.
    pub fn set_add_flow_open(&mut self, open: bool) {
        self.add_flow.set(open, self.preference);
    }

    /// Advances every animated value and reports whether the window needs another frame.
    pub fn advance(&mut self, elapsed: Duration) -> Frames {
        Frames::from_moving(self.library_width.advance(elapsed))
            .or(Frames::from_moving(self.outline_width.advance(elapsed)))
            .or(Frames::from_moving(self.selection_nib.advance(elapsed)))
            .or(Frames::from_moving(self.palette.advance(elapsed)))
            .or(Frames::from_moving(self.add_flow.advance(elapsed)))
    }
}

#[derive(Clone, Copy)]
enum PanelSide {
    Library,
    Outline,
}

impl Motion {
    fn retarget_panel(&mut self, open: bool, width: f32, side: PanelSide) {
        let target = if open { width } else { 0.0 };
        let animates = self.preference.animates();
        let spring = match side {
            PanelSide::Library => &mut self.library_width,
            PanelSide::Outline => &mut self.outline_width,
        };
        if animates {
            spring.animate_to(target);
        } else {
            spring.snap_to(target);
        }
    }
}

impl Default for Motion {
    fn default() -> Self {
        Self::restored(MotionPreference::default(), true, true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shell_that_nothing_touched_requests_no_frame() {
        let mut motion = Motion::default();
        assert_eq!(motion.advance(Duration::from_millis(16)), Frames::Idle);
        assert_eq!(motion.advance(Duration::from_millis(16)), Frames::Idle);
    }

    #[test]
    fn closing_a_panel_wants_frames_until_it_lands() {
        let mut motion = Motion::default();
        motion.set_library_open(false);
        let mut frames = 0_u32;
        while motion.advance(Duration::from_millis(16)).wanted() {
            frames += 1;
            assert!(frames < 400, "the library panel never settled");
        }
        assert!(frames > 4, "the panel arrived without any motion at all");
        assert!(motion.library_width().abs() < 0.01);
    }

    #[test]
    fn reduced_motion_lands_a_panel_with_no_frames_at_all() {
        let mut motion = Motion::default();
        motion.set_preference(MotionPreference::Reduced);
        motion.set_library_open(false);
        assert_eq!(motion.advance(Duration::from_millis(16)), Frames::Idle);
        assert!(motion.library_width().abs() < f32::EPSILON);
    }

    #[test]
    fn switching_to_reduced_motion_lands_everything_in_flight() {
        let mut motion = Motion::default();
        motion.set_outline_open(false);
        motion.set_palette_open(true);
        assert!(motion.advance(Duration::from_millis(16)).wanted());
        motion.set_preference(MotionPreference::Reduced);
        assert_eq!(motion.advance(Duration::from_millis(16)), Frames::Idle);
        assert!(motion.outline_width().abs() < f32::EPSILON);
        assert_eq!(motion.palette().phase(), Phase::Open);
    }
}
