pub mod types;
pub mod body;
pub mod walker;
pub mod error;

pub use types::{ResolvedReference, ReferenceKind};
pub use body::{FunctionBody, ParsedBody};
pub use walker::walk_references;
pub use error::ParseError;

#[cfg(test)]
pub mod tests;
