//! The `interface-gui` crate exists to read one shared local library through a desktop reader.
//! Its public types are the complete boundary; implementation details remain private.
//! Callers compose capabilities through explicit authority, ownership, and failure values.
//!
//! # Shape
//!
//! ```text
//! theme/    colour, type, space: one palette, one accent, one rule
//! motion/   springs and reveals; an idle window requests no frames
//! prefs     the typed file loaded before the first frame
//! store/    the state DAG: library → search/document → palette/shell
//! preview/  typed fixtures (feature `preview`)
//! ui/       the element library
//! views/    the shell
//! app/      window, actions, keymap, and the resident engine
//! ```
//!
//! The stores, the theme, the motion integrator, and the preference codec are headless and tested
//! without a window; the element library, views, and app draw them with real GPUI, unconditionally.
//! That is not a testing convenience — it is what makes the interface's behaviour a value rather
//! than a side effect of drawing. There is no `real-gpui` feature: the desktop surface is the
//! product, and a crate that could compile without its window could silently stop having one.
//!
//! # The one dispatch
//!
//! No view ever reaches past `interface_library::Library::execute`. A click, a palette row, and a
//! keystroke all produce the same closed [`interface_library::Command`], and every failure the
//! engine returns is drawn in place as a typed fault with its exact operand retained.

pub mod motion;
pub mod prefs;
pub mod store;
pub mod theme;

pub mod app;
pub mod ui;
pub mod views;

#[cfg(feature = "preview")]
pub mod preview;

pub use app::{Workspace, run};
