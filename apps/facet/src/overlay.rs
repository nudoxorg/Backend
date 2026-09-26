//! Overlays: everything that floats. One float layer per window carries
//! tips, peeks, lenses and menus (hover intent, placement, chain, pins,
//! exits, focus — written once in [`float`]); rich text makes identifiers in
//! prose and signatures rest-able; toasts, the dialog and hint labels ride
//! the same layer.
//!
//! Read `docs/architecture/gui-plan.md` §6–§7 before changing the anatomy
//! here; the peek is implemented against the lead's rendered targets under
//! `Nudox-Design-System/v4/shots/`, not prose.

pub mod float;
pub mod lens;
pub mod text;
pub mod peek;
pub mod tooltip;
pub mod menu;
pub mod toast;
pub mod dialog;
pub mod hint;

#[cfg(feature = "gallery")]
pub(crate) mod gallery;
#[cfg(feature = "gallery")]
pub(crate) mod gallery_overlays;

pub use float::{FloatKind, FloatRequest, Side, Surface};
