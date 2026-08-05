//! Working-tree naming and link wire form for the file-per-`IntroId` layout.
//!
//! Ported from `workspace/ir/serialize.rs` for use within `ir-vcs`.

use ir::change::{IntroId, StableRef};
use ir::kind::KindDiscriminant;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// LinkWire
// ---------------------------------------------------------------------------

/// A link from this symbol's perspective (stored on the canonical-owner side).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkWire {
    /// The other endpoint of the link.
    pub other: StableRef,
    /// The kind of this (self) endpoint.
    pub kind_self: KindDiscriminant,
    /// The kind of the other endpoint.
    pub kind_other: KindDiscriminant,
}

// ---------------------------------------------------------------------------
// Path helpers
// ---------------------------------------------------------------------------

/// File extension for a symbol file in the working tree.
pub const SYMBOL_EXT: &str = ".nir";

/// Returns `"{intro_hex}.nir"` — the path of an intro's file at the root of
/// the working tree.
pub fn symbol_path(intro: IntroId) -> String {
    format!("{}{}", intro.to_hex(), SYMBOL_EXT)
}

/// True if `path` is a symbol file at the working-tree root.
pub fn is_symbol_path(path: &str) -> bool {
    !path.contains('/') && path.ends_with(SYMBOL_EXT) && path.len() == 64 + SYMBOL_EXT.len()
}

/// Extract the intro hex from a symbol file path (`"{hex}.nir"` → `"{hex}"`).
pub fn intro_hex_of(path: &str) -> Option<&str> {
    path.strip_suffix(SYMBOL_EXT)
}
