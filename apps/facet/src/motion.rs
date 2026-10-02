//! The motion engine: clock, curves, springs, keyframes, the per-view motion
//! store, the leased ambient pulse, reduced motion, and layout-neutral
//! elements.
//!
//! # Clock
//!
//! Every time source is the executor clock, [`now`] (the vendored gpui reads
//! the same clock in `with_animation`). In the app that is real time; in the
//! headless harness it is the `TestClock`, which only moves with
//! `advance_clock`, so any frame at any virtual time is reproducible.
//!
//! # Using it in a view
//!
//! ```ignore
//! struct Row { motion: Motion, open: bool }
//!
//! impl Render for Row {
//!     fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
//!         // A value: retargets smoothly whenever `open` flips.
//!         let lift = self.motion.animate("lift", if self.open { -3.0 } else { 0.0 }, spec::LIFT, window, cx);
//!         // A one-shot keyframe pose, started the first time the key is seen.
//!         let pose = self.motion.play(("drop", self.id), &keys::DROP_IN, window, cx);
//!         offset(div().child("…")).y(px(lift)).pose(pose)
//!     }
//! }
//! ```
//!
//! While a track is live the store asks the window's gate for one more frame
//! for this view; once everything settles, nothing is scheduled.

pub mod carry;
pub mod compositing;
mod curve;
mod element;
pub mod flight;
pub mod flow;
pub mod keys;
pub mod presence;
pub mod print;
pub mod shared;
pub mod pulse;
mod spring;
mod store;
#[cfg(test)]
mod tests;

#[cfg(feature = "gallery")]
pub(crate) mod gallery;
#[cfg(feature = "gallery")]
pub(crate) mod lab;

pub use curve::{EASE, LINEAR};
pub use element::{Offset, Reveal, offset, posed, reveal};
pub use keys::{Keys, Mix, Pose};
pub use flight::{Camera, Flights, Shot};
pub use flow::{Flow, Resize};
pub use carry::{CARRY, Carry, band};
pub use presence::{Presence, act};
pub use print::{Edge, masked, print};
pub use shared::{Fit, shared};
pub use pulse::Pulse;
pub use spring::{BOUNCY, GENTLE, Phase, SNAPPY, Spring};
pub use store::{Motion, Spec, frames_requested, request_frame};

use crate::theme::ActiveFacet;
use gpui::{App, Global};
use std::time::Instant;
use std::{cell::Cell, rc::Rc};

/// Named motion specs: views pick one of these, never a raw duration.
pub mod spec {
    use super::Spec;
    use super::spring::{GENTLE, SNAPPY};
    use crate::tokens::motion::{BOUNCE, DROP, EMPH, GLIDE, MICRO, QUICK, SCENE, SNAP, STD};

    /// Hover colour and small state changes.
    pub const HOVER: Spec = Spec::tween(MICRO, GLIDE);
    /// Press feedback.
    pub const PRESS: Spec = Spec::tween(MICRO, SNAP);
    /// Tooltips and small reveals.
    pub const REVEAL: Spec = Spec::tween(QUICK, GLIDE);
    /// Plate lifts and most transitions (the v3 default overshoot).
    pub const LIFT: Spec = Spec::tween(STD, BOUNCE);
    /// Emphasis: the facet sweep, dialogs.
    pub const EMPHASIS: Spec = Spec::tween(EMPH, BOUNCE);
    /// Scene changes: descent between depths.
    pub const DESCENT: Spec = Spec::tween(SCENE, GLIDE);
    /// Things leaving: accelerate away.
    pub const LEAVE: Spec = Spec::tween(QUICK, DROP);
    /// Values that follow a pointer or a measured size.
    pub const FOLLOW: Spec = Spec::Spring(SNAPPY);
    /// Panels and splitters settling without overshoot.
    pub const SETTLE: Spec = Spec::Spring(GENTLE);
}

/// The executor clock: real time in the app, the `TestClock` headless.
#[must_use]
pub fn now(cx: &App) -> Instant {
    cx.background_executor().now()
}

/// Whether motion is reduced (the facet's setting or the platform's).
#[must_use]
pub fn reduced(cx: &App) -> bool {
    is_still(cx) || cx.facet().reduced_motion || cx.reduce_motion()
}

/// Retained native subtrees settle their internal motion to exact layout,
/// independently of the current user/system motion preference.
#[must_use]
pub fn reduced_in(window: &gpui::Window, cx: &App) -> bool {
    reduced(cx) || window.is_inert_subtree()
}

/// A retained body must settle immediately, even where ordinary reduced
/// motion intentionally uses a short opacity transition.
pub(crate) fn is_still_in(window: &gpui::Window, cx: &App) -> bool {
    is_still(cx) || window.is_inert_subtree()
}

/// Locally sample a retained body at rest while it is assembled. This does
/// not change the user's preference; dropping the guard restores the prior
/// scope even during unwinding. Hold it only around synchronous body work.
#[must_use]
pub fn still(cx: &mut App) -> StillGuard {
    let depth = Rc::clone(&cx.default_global::<StillDepth>().0);
    depth.set(depth.get() + 1);
    StillGuard(depth)
}

/// A bounded synchronous still-motion scope, independent of preferences.
pub struct StillGuard(Rc<Cell<usize>>);

impl Drop for StillGuard {
    fn drop(&mut self) {
        self.0.set(self.0.get() - 1);
    }
}

#[derive(Default)]
struct StillDepth(Rc<Cell<usize>>);
impl Global for StillDepth {}

fn is_still(cx: &App) -> bool {
    cx.try_global::<StillDepth>().is_some_and(|depth| depth.0.get() > 0)
}

#[derive(Default)]
struct Epoch(Option<Instant>);

impl Global for Epoch {}

/// The motion epoch: the first time motion was used in this app. Probe times
/// and the pulse are measured from it.
pub fn epoch(cx: &mut App) -> Instant {
    let now = now(cx);
    *cx.default_global::<Epoch>().0.get_or_insert(now)
}

/// Pins the motion epoch to the current time. The gallery calls this right
/// before building a scene so probe times start at 0 with the scene.
pub fn reset_epoch(cx: &mut App) {
    let now = now(cx);
    cx.default_global::<Epoch>().0 = Some(now);
}
