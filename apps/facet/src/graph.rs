//! The world graph: every symbol of a workspace and its dependencies as one
//! map you pan, zoom and fly through (gui-plan §8.2; the prototype is
//! `Nudox-Design-System/v4/graph/app.js`).
//!
//! - [`model`]: the world (symbols, typed relations, rollups, importance,
//!   your footprint). Pure data; the page and the graph share it.
//! - [`layout`]: nested, deterministic positions — world → packages →
//!   modules → items → member shells — computed off the UI thread and
//!   cached by the world's content hash.

pub mod layout;
pub mod model;

pub use layout::Layout;
pub use model::{Edge, Kind, Module, Node, NodeId, Package, Rel, World};
