//! The GUI-only half of the presentation model.
//!
//! Everything that is *the same* on every surface — identity, pages,
//! signatures, prose, shelves, faults, coverage, outlines, the command
//! grammar — lives in [`backend_present`] and is used directly here. This
//! module holds only what a window needs and a terminal does not: a crumb
//! trail whose steps are separately clickable, chips sized for a status bar,
//! and the mapping from a shared [`backend_present::Fault`] to the weight a
//! window draws it at.
//!
//! The rule that decides where a type belongs: if the CLI would print it, it
//! belongs in `backend-present`; if only a pointer can reach it, it belongs
//! here. A local copy of a shared type would mean the desktop could disagree
//! with `backend search` about what a coordinate means, which is the one
//! failure this split exists to make impossible.

pub(crate) mod chips;
pub(crate) mod crumb;
pub(crate) mod fault;
pub(crate) mod project;
