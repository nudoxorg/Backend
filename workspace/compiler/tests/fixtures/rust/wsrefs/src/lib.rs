//! Workspace/multi-module reference fixture: `app` and `util` both call
//! `math::add` across module boundaries (one fully-qualified, one via `use`),
//! and `app` also calls into the `helper` path-dependency.

pub mod app;
pub mod math;
pub mod util;
