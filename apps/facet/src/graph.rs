//! The world graph: every symbol of a workspace and its dependencies as one
//! map you pan, zoom and fly through (gui-plan §8.2; the prototype is
//! `Nudox-Design-System/v4/graph/app.js`).
//!
//! - [`model`]: the world (symbols, typed relations, rollups, importance,
//!   your footprint). Pure data; the page and the graph share it.
//! - [`layout`]: nested, deterministic positions — world → packages →
//!   modules → items → member shells — computed off the UI thread and
//!   cached by the world's content hash.
//! - [`scene`]: what the renderer derives once per layout (picking grid,
//!   inner edges, framings).
//! - [`camera`]: `(x, y, w)`, flights, inertia, the eased wheel.
//! - [`draw`] and [`prism`]: painting one frame.
//! - [`view`]: the region the shell mounts.
//! - [`peek`]: a symbol as the float layer's peek card.

pub mod camera;
pub mod draw;
pub mod discovery;
pub(crate) mod highlight;
pub mod interaction;
pub mod layout;
pub mod model;
pub(crate) mod navigation;
pub mod peek;
pub mod prism;
pub mod scene;
pub mod view;

#[cfg(feature = "gallery")]
pub(crate) mod gallery;

pub use layout::Layout;
pub use model::{Edge, Kind, Module, Node, NodeId, Package, Rel, World};
pub use view::{GraphView, Start};
