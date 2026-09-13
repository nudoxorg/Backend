//! Canonical product schemas and admission façade.
//!
//! Schema markers and complete logical preimages are kept separate from wire
//! claim admission. Callers can parse an untrusted fixed-width claim, but a
//! typed identity is returned only after the canonical value or an accepted
//! expected identity has been checked.

mod admission;
mod encoding;
mod frontier;
mod schema;
mod values;

pub use admission::*;
pub use encoding::*;
pub use frontier::*;
pub use schema::*;
pub use values::*;

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests;
