//! lindsey — the nudox GPUI shell.
//!
//! Structure is GUI-PLAN §26 verbatim, with the Rev 2 substitution from
//! GUI-LOCAL-PLAN: there is no `client_stub/`, because the engine is real.
//!
//! The crate is a library plus a thin binary so that kernels (`motion`,
//! `bridge`) and store/view pairs are testable under `#[gpui::test]` without
//! opening a window.
//!
//! ## The one law a reviewer should hold in their head
//!
//! Data flows *up* — engine → store → view — and intent flows *down* —
//! view → store → engine. A view never reaches past its store, and a store
//! never reaches past the engine handle. Everything else in this crate is a
//! detail of how that is made fast and how it is made to move.

pub mod app;
pub mod bridge;
pub mod highlight;
pub mod motion;
pub mod platform;
pub mod theme;

pub mod perf;
pub mod stores;
pub mod ui;
pub mod views;
pub mod workspace;
