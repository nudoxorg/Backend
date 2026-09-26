//! The semantic core the symbol page and the graph view share (gui-plan
//! §8.1, §8.3): what a symbol is made of and what it goes into, said once,
//! in plain words, the same way for every language.
//!
//! Pure logic; nothing here draws. The elements live in
//! [`anatomy`](crate::anatomy) (the page) and `graph` (the map).
//!
//! - [`types`]: a language-neutral type tree, the Rust parser, and the
//!   plain-word speller (`maybe X`, `list of X`, `X or fails with E`, `its
//!   Value`, every named type a link [`Target`]).
//! - [`bounds`]: generic parameters as sentences (`T is any Deserialize`).
//! - [`caps`]: capabilities in words, how each arrives, implied derives
//!   compressed.
//! - [`members`]: receivers in words (Copy-aware) and look-alike folding.
//! - [`usage`]: the statement a caller writes to use a symbol.
//! - [`relations`]: the one rule for relations (left: comes from, right:
//!   goes into, `is`: the capability line) and the prism's columns.
//! - [`names`]: written type names resolved to symbols in the world.
//! - [`page`]: the page model each anatomy element renders ([`model`] holds
//!   its data types, free of the world).
//!
//! The prototype is `Nudox-Design-System/v4/graph/{app,page}.js`; where
//! this module and the prototype differ, the tests say why.

pub mod bounds;
pub mod caps;
pub mod members;
pub mod types;
pub mod usage;

pub mod model;
pub mod names;
pub mod page;
pub mod relations;

pub use bounds::Generic;
pub use caps::{Arrives, Cap};
pub use members::{Look, Param, Receiver};
pub use types::{Piece, Resolve, Scope, Spelled, Target, TypeExpr};
pub use usage::{Excerpt, Needle};

pub use model::{Column, Entry, Group, Note, PrismRow, Side, Word};
pub use names::{InWorld, Names};
pub use page::{Page, page};
pub use relations::{PAGE_SHOWS, except, prism, relations_of};

#[cfg(test)]
mod tests;
