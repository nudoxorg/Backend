//! The package folio's own components: what the package page is made of
//! beyond the marks in [`crate::marks`] and the maps in [`crate::data`].
//!
//! - [`cards`]: one symbol as a card whose badges (never code) say what it
//!   is, and the module view that lays them out.
//! - [`crest`]: the licence stamp and the cells of the hero's crest row.
//! - [`module`]: a module opened, inline or as a page of its own.
//! - [`rail`]: the chips that stand in for the map while a module is open.
//! - [`shingles`]: the territory, one shingle per public name.
//! - [`ticker`]: the release ticker, a fisheye along a time line.
//!
//! Plain data in, and nothing here knows the engine.

pub mod berg;
pub mod cards;
pub mod crest;
pub mod features;
#[cfg(any(test, feature = "gallery"))]
pub mod fixture;
pub mod heads;
#[cfg(feature = "gallery")]
pub(crate) mod gallery;
pub mod module;
pub mod rail;
pub mod shingles;
pub mod state;
pub mod text;
pub mod ticker;

#[cfg(test)]
mod tests;
