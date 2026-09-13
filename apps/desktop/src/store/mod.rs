//! GPUI entities holding render-ready state, updated at event time.
//! Nothing in a store is computed during render: a view reads fields and draws.
//! Stores form a directed graph — workspace is the only source, shell the sink.
//!
//! The graph is: `workspace` (the live feed and the shelf) and `jobs` (intents
//! in flight) feed `search` and `document`; all four feed `shell`, which owns
//! layout, focus, notices, and the persisted preferences. Nothing flows back
//! from a view into a store except as a method call the reader caused.

pub(crate) mod document;
pub(crate) mod events;
pub(crate) mod jobs;
pub(crate) mod prefs;
pub(crate) mod search;
pub(crate) mod service;
pub(crate) mod shell;
pub(crate) mod workspace;
