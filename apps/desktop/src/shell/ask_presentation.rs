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
}

#[derive(Default)]
pub(super) struct AskPresentation {
    placement: AskPlacement,
    motion: Motion,
    last: Option<AskScene>,
}

impl AskPresentation {
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
        let phase = if active {
            Some(if veil < 0.999 {
                StackPhase::Entering
            } else {
                StackPhase::Open
            })
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
