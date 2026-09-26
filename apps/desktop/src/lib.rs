//! Nudox desktop: one local-first, versioned reader.
//!
//! A GPUI window mounts one [`shell::Shell`]: the faceted ground, region
//! entities (titlebar, shelf, reader, pins, status) and the transient
//! layers. State lives in one [`runtime::UiRootEntity`], which admits an
//! immutable [`model::AppSnapshot`], reduces typed navigation intents, and
//! delegates producer work through the bounded engine actor; page data lives
//! in the [`runtime::store::DataStore`], filled by the read pool. The desktop
//! crate does not contain a second store, reducer, socket transport, or
//! widget-owned service session; CLI and MCP use the shared client directly.

#![deny(unsafe_code)]

/// Product-neutral identities and read-model ports for the desktop shell.
pub mod core;
/// One immutable snapshot and its persistence schema.
pub mod model;
/// Typed routes, intents, effects, actions, and pure reduction.
pub mod navigation;
/// Background actor, stale-result coordinator, the data plane, wake signals.
pub mod runtime;
/// The window root and its region views.
pub mod shell;

#[cfg(feature = "visual-harness")]
pub mod harness;

#[cfg(any(unix, windows))]
pub(crate) mod host;

#[cfg(any(unix, windows))]
pub use host::lease::{DesktopHost, HostError, HostMode};
#[cfg(any(unix, windows))]
pub use host::launch::main_entry;
