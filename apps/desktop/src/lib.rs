//! Nudox desktop: one local-first, versioned reader.
//!
//! A GPUI window owns one [`runtime::UiRootEntity`]. It admits an immutable
//! [`model::AppSnapshot`], reduces typed navigation intents, and delegates
//! producer work through the bounded engine actor. The desktop crate does not
//! contain a second store, reducer, socket transport, or widget-owned service
//! session; CLI and MCP use the shared client directly.

#![deny(unsafe_code)]

/// Product-neutral identities and read-model ports for the desktop shell.
pub mod core;
/// One immutable snapshot and its persistence schema.
pub mod model;
/// Typed routes, intents, effects, actions, focus, and pure reduction.
pub mod navigation;
/// Background actor, stale-result coordinator, motion clock, and GPUI graph.
pub mod runtime;

#[cfg(feature = "visual-harness")]
pub mod harness;

#[cfg(unix)]
pub(crate) mod host;
pub(crate) mod theme;
#[cfg(unix)]
mod ui;
#[cfg(unix)]
mod views;

#[cfg(unix)]
pub use host::lease::{DesktopHost, HostError, HostMode};
#[cfg(unix)]
pub use host::launch::main_entry;
