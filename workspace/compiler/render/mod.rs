//! Item-level IR → surface-syntax rendering.
//!
//! The reverse of the language producers: where producers lower a package's
//! items *into* the shared surface IR, this module renders single IR entries
//! *back* into faithful, human-readable source syntax — one struct, one enum,
//! one function signature, one trait at a time. Whole-module and whole-package
//! emission is deliberately out of scope.
//!
//! One submodule per target language, starting with Rust. Every renderer is a
//! plain, infallible function returning `String`; there is intentionally no
//! trait-abstraction layer here — the shared vocabulary stays limited to
//! [`RenderOptions`] until a second target language justifies more.
//!
//! Item-level documentation and item names live on `ir::kind::Symbol`, not on
//! the payload structs (`Record`, `Function`, `TraitDef`, …), so renderers
//! take the name explicitly where the payload does not carry one and leave
//! item-level doc comments to the caller. Documentation carried *inside* the
//! payloads (fields, variants, trait methods) is rendered when
//! [`RenderOptions::show_docs`] is set.

pub mod rust;

/// Shared knobs for all target languages.
///
/// Deliberately concrete and minimal — plain data, no trait layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderOptions {
	/// Identifiers in the IR are fully-qualified paths
	/// (e.g. `std::vec::Vec`). By default renderers print only the last
	/// path segment (`Vec`) for readability; set this to render the full
	/// path instead.
	pub qualified_paths: bool,

	/// Emit documentation carried inside IR payloads (fields, variants,
	/// trait methods) as doc comments above the corresponding lines.
	pub show_docs: bool,
}

impl Default for RenderOptions {
	fn default() -> Self {
		Self { qualified_paths: false, show_docs: false }
	}
}
