//! This handles the creation and emission of the blobs themselves
//!
//! Any record that a dependency or crate exists on an external registry don't
//! have to be typed/explicitly recorded in any real way. I think at best we
//! just want to link out to it, but to any processing code, the location is
//! weird overhead, they just need the text.
//!
//!
//!
//!
//! When it comes to storing the information as a blob, treesitter isn't a
//! lossless parse, and we should maintain the text alongside it, in the same
//! blob. For this we need to have a (sort of) custom binary serialization
//! process (probably just bincode)

use std::{io::Cursor, marker::PhantomData};

use arborium_tree_sitter::Tree;
use ir::entry::Index;

pub mod creation;
pub mod emit;

/// An anonymous blob. Stores the representation of a particular package in the
/// three resolutions we care the most about, post-processing.
#[must_use = "a built blob must be emitted (or explicitly discarded)"]
pub struct Blob {
	// TODO: Some way of idenitfying/claering out old blobs
	/// The entire syntax tree (re: treesitter)
	concrete_syntax_tree: Tree,

	/// The full API surface (re: nudox IR gen)
	api_surface: Index,

	/// The condensed source representation (re: taring): an in-memory `tar`
	/// archive of the package's source text, kept alongside the lossy CST.
	source_text: tar::Archive<Cursor<Vec<u8>>>,
}
