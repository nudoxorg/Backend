pub mod body;
pub mod error;
pub mod types;
pub mod walker;

pub use body::{FunctionBody, ParsedBody};
pub use error::ParseError;
pub use types::{ReferenceKind, ResolvedReference};
pub use walker::walk_references;

#[cfg(test)]
pub mod tests;
