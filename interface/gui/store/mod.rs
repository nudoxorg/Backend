//! Defines the state DAG for `interface-gui`.
//! This module owns the three stores every view reads and no view writes around.
//! Its narrow surface is the shell's whole behavioural memory, headless and testable.

pub mod document;
pub mod explore;
pub mod library;
pub mod search;

pub use document::DocumentStore;
pub use explore::ExploreStore;
pub use library::LibraryStore;
pub use search::SearchStore;
