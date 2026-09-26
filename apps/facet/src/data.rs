//! Data marks: how Nudox shows relations, releases, modules and progress in
//! very little space, and how every part of every mark opens.
//!
//! Every mark here is one custom-painted element — never a div per tick,
//! stone or spoke — sized from the [`Measure`](crate::Measure) it is given
//! and drawn at the rung its room affords. Every sub-part is a door
//! ([`Door`]): resting on it reports its exact sub-rect to the float layer,
//! the keyboard walks the parts with the arrow keys (the walked part carries
//! the periwinkle light), Space opens it, Enter descends. Under ⌥ x-ray the
//! marks spell their meaning in place.
//!
//! Hover and walk state live in the mark (keyed by its element id), so a
//! hover never rebuilds the caller's data.

pub mod caps;
pub mod comb;
pub mod compass;
pub mod door;
pub mod facts;
#[cfg(feature = "gallery")]
pub(crate) mod gallery;
pub mod live;
pub mod mosaic;
pub mod progress;
pub mod rose;
pub mod spatial;
pub mod spell;
pub mod strands;
pub mod territory;
pub mod text;
#[cfg(test)]
mod tests;

pub use caps::{Caps, Has, caps};
pub use comb::{Comb, CombOrientation, FileComb, FileUses, Tick, TickInk, TickTone, comb, fcomb};
pub use compass::{
    ArmLight, Compass, CompassBar, CompassSize, Dir, Directions, arm_length, compass, compass_bar,
    compass_row, paint_arms,
};
pub use facts::{Facts, LensBar, Run, Tab, facts, lens_bar};
pub use door::{Door, Opens, Rested, Side, part_key};
pub use live::Live;
pub use mosaic::{Cell, Mosaic, Stone, StoneState, mosaic};
pub use progress::{GemProgress, Seam, Stage, StageState, gem_progress, seam};
pub use rose::{Member, Rose, rose};
pub use spatial::{Aabb, Grid, Painted, take_painted, visible};
pub use spell::{Seg, Spell, spell};
pub use territory::{Region, Spot, Territory, squarify, territory};
