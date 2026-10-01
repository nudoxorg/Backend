//! Fluid: the one authority for width.
//!
//! The window is dragged from 2560 px down to a 320 px phone and back. Nothing
//! may jump, flicker, clip or overflow on the way, so no part of the app turns
//! a width into a layout decision on its own. Width enters here and leaves as
//! one of four typed things:
//!
//! 1. **[`Room`]**: the width a region actually has (its own, not the
//!    window's) and the text scale it is set in. Decisions are made on design
//!    px, width ÷ text scale, so 200 % text on a 1440 px window lays out as a
//!    720 px window would.
//! 2. **[`Fluid`] tokens**: a token is a ramp between end points, not a
//!    number. `GUTTER = 16 at 320 → 40 at 1440` glides with the room, is
//!    clamped beyond its ends, and is read as `GUTTER.at(room)`. Gutters, gaps,
//!    type steps, rails, gems and card widths are all tokens, defined once in
//!    [`crate::tokens::fluid`], the only place a [`Fluid`] can be built.
//! 3. **[`Ladder`] modes**: the few genuine changes of arrangement (a shelf
//!    beside the page, a spine, a drawer over it) are an ordered set of modes,
//!    each holding from a room up. A region reads it through its [`Modes`], which
//!    remembers the mode it is in: the mode changes only when the room is half
//!    a [`HYSTERESIS`] band past the edge, so a window resting on a threshold
//!    cannot flip it every frame. A change reports how far it has come
//!    ([`Settled::progress`], through `facet::motion`, instant under reduced
//!    motion) and counts an [`Epoch`] that a [`crate::motion::Flow`] turns into FLIP, so
//!    items move to their new places instead of jumping.
//! 4. **[`Grid`] columns**: a column count is a mode too. It changes half a
//!    band past the edge, and the columns' own width is continuous between
//!    changes.
//!
//! ```ignore
//! use facet::tokens::fluid;
//! // A region, every render:
//! let room = measure.fluid_room();
//! let pad = fluid::READER_PAD.at(room);                     // glides with the width
//! let dock = self.modes.settle(&fluid::DOCK, window_room);  // holds still at an edge
//! let cols = self.modes.columns(&fluid::ROLES, room, gap);  // 1 or 2, with FLIP
//! self.flow.epoch((cols.epoch, facet.density));
//! ```
//!
//! The rule the types enforce: there is no `if width < 900.0` outside this
//! module. Room has no comparison, a token cannot be built outside the
//! design tokens, and a mode cannot be read without its hysteresis. The rule
//! is also checked by a test that scans the source (`tests::no_hand_rolled_breakpoints`).

mod ladder;
mod modes;
mod ramp;
mod room;

#[cfg(test)]
mod tests;

pub use ladder::{HYSTERESIS, Ladder, ModeId, Rung, rung};
pub use modes::{Columns, Epoch, Grid, Modes, Settled};
pub use ramp::{Blend, Curve, Fluid, Length, Px, Ratio, Stop, Unit, stop};
pub use room::{Design, Room};
