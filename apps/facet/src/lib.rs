//! FACET v3, "cut, not painted": the Nudox design system in GPUI.
//!
//! This crate owns everything visual that is not a product route: tokens,
//! fonts, icons, the motion engine, the paint primitives (the cut plate, the
//! hatch, the gem, the faceted ground), controls, marks, data marks,
//! overlays and chrome pieces. It never depends on the engine, so it builds
//! and links in seconds and every component is exercised in the
//! `facet-gallery` binary before a product view uses it.
//!
//! The contract lives in `docs/architecture/gui-plan.md`.

pub mod tokens;
pub mod theme;
pub mod measure;

pub mod fonts;
pub mod motion;
pub mod paint;
pub mod icons;
pub mod controls;
pub mod chrome;
pub mod data;
pub mod overlay;
pub mod probe;
pub mod code;
pub mod graph;
pub mod semantics;
pub mod anatomy;

#[cfg(feature = "gallery")]
pub mod gallery;

pub use fonts::Typeset;
pub use motion::{Motion, Pose, Pulse, Spec};
pub use measure::{Control, Density, Measure, Needs, Reveal, Room, Rung, Set, Space};
pub use theme::{ActiveFacet, Contrast, Facet, set_facet};
pub use tokens::{Appearance, Face, Family, Palette, Tone, TypeRole, Voice};
