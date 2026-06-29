//! Heart — the methods and types that are most widely shared across modules,
//! usually common or shared generic types.
//!
//! Anything that is specific to a single subsystem (the registry, the runtime
//! stores, the compiler, the server) lives in that subsystem's crate. What
//! remains here is deliberately small and generic: the type-tagged identifier,
//! the scored-result wrapper, the language enum, the versioned wrapper, and the
//! cross-cutting `Progressive` / `Sink` behavioural traits.

pub mod hit;
pub mod identifier;
pub mod language;
pub mod progress;
pub mod sink;
pub mod version;

pub use hit::Hit;
pub use identifier::Id;
pub use language::Language;
pub use progress::Progressive;
pub use sink::{BatchSink, Sink};
pub use version::Versioned;
