//! One presentation state for Ask's veil, occupied plate, and Reader clearance.
//! The live Ask entity belongs only to the current command-palette overlay;
//! an exiting plate is paint with no results, focus handles, or callbacks.

use super::frame::{AskGeometry, AskPlacement, Frame};
use facet::fluid::Modes;
use facet::motion::{Motion, spec};
use facet::probe::StackPhase;
use gpui::{App, Pixels, Size, Window, px};

/// Motion values and interaction admission sampled for one painted frame.
#[derive(Clone, Copy, Debug)]
pub(super) struct AskScene {
    pub phase: Option<StackPhase>,
    pub veil: f32,
    pub geometry: Option<AskGeometry>,
    /// Active results paint behind the horizontal reveal. They stay inert
    /// until the plate exposes their full content width.
    pub paint_results: bool,
    /// Only fully exposed results may register focus or native actions.
    pub live_results: bool,
    /// The full layout width of the results inside the clipped reveal.
    pub content_width: Pixels,
}

impl AskScene {
    pub fn visible(self) -> bool {
        self.phase.is_some()
    }

    pub fn background(self, columns_moving: bool, input_allowed: bool) -> BackgroundPresentation {
        if columns_moving || matches!(self.phase, Some(StackPhase::Entering | StackPhase::Leaving)) {
            BackgroundPresentation::Moving
        } else if input_allowed {
            BackgroundPresentation::Interactive
        } else {
            BackgroundPresentation::Covered
        }
    }
}

/// The sampled shell frame owns both the Reader's settlement and input gate.
/// An open, settled Ask may cover a settled page; an entering or leaving plate
/// cannot advertise that page as settled while its clearance still moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BackgroundPresentation {
    Moving,
    Covered,
    Interactive,
}

#[derive(Default)]
pub(super) struct AskPresentation {
    placement: AskPlacement,
    motion: Motion,
    last: Option<AskScene>,
}

impl AskPresentation {
    #[cfg(test)]
    pub(super) fn diagnostic_scene(&self, cx: &App) -> (Option<AskScene>, bool) {
        (self.last, self.motion.is_live(cx))
    }

    /// The previous painted scene owns keyboard admission between an overlay
    /// close event and the next frame. It releases background keys only after
    /// a sampled frame has actually dropped the last exit pixel.
    pub fn blocks_background_input(&self, active: bool) -> bool {
        !active && self.last.is_some_and(AskScene::visible)
    }

    pub fn sample(
        &mut self,
        active: bool,
        has_query: bool,
        frame: &Frame,
        viewport: Size<Pixels>,
        status: Pixels,
        reader_left: Pixels,
        modes: &Modes,
        window: &mut Window,
        cx: &mut App,
    ) -> AskScene {
        let target = if active && has_query {
            self.placement.target_width(frame, viewport, reader_left, modes)
        } else {
            px(0.0)
        };
        // Both tracks use FACET's executor clock and bounded frame gate.
        // Retargeting a spring preserves its sampled velocity, including an
        // interrupted close followed by another opening or a resize.
        let width = self.motion.animate_from(
            "ask-plate-w", 0.0, f32::from(target), spec::SETTLE, window, cx,
        ).clamp(0.0, f32::from(viewport.width));
        let veil = self.motion.animate_from(
            "ask-veil", 0.0, if active { 1.0 } else { 0.0 }, spec::REVEAL, window, cx,
        ).clamp(0.0, 1.0);
        let geometry = (width > 0.5).then(|| AskGeometry::resolve(
            frame, viewport, status, reader_left, px(width),
        ));
        // Both values were sampled from this presentation's one motion store.
        // A settled veil cannot call the scene Open while the occupied plate
        // is still moving beside the Reader.
        let tracks_settled = !self.motion.is_live(cx);
        let phase = if active {
            Some(if tracks_settled { StackPhase::Open } else { StackPhase::Entering })
        } else if veil > 0.001 || geometry.is_some() {
            Some(StackPhase::Leaving)
        } else {
            None
        };
        let paint_results = active && has_query && geometry.is_some();
        let live_results = paint_results && width + 0.5 >= f32::from(target);
        let scene = AskScene { phase, veil, geometry, paint_results, live_results, content_width: target };
        self.last = Some(scene);
        scene
    }
}
