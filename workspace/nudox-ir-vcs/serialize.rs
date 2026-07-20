//! Working-tree naming and link wire form for the file-per-`IntroId` layout.
//!
//! Each live symbol maps to a `{intro_hex}.nir` file at the root of the pijul
//! working tree; the file content is the line-oriented text format defined in
//! [`crate::blob`].
//!
//! Links are stored **exactly once** on the endpoint with the smaller
//! [`IntroId`] (canonical owner, determined by byte comparison). Cross-package
//! links: the local endpoint always owns the entry.

use nudox_ir::change::{IntroId, StableRef};
use nudox_ir::kind::KindDiscriminant;
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

/// Returns `"{intro_hex}.nir"` — the path of an intro's file at the **root** of
/// the working tree.
///
/// Symbol files live at the root (not under a `symbols/` subdirectory) on
/// purpose: libpijul's in-memory working copy panics (`unreachable!()` in
/// `touch`) when `unrecord`'s re-output touches a directory inode. Keeping every
/// symbol at the root means the tree contains only files, so that path is never
/// exercised. The intro hex is fixed-length (64 chars) so the extension parse is
/// unambiguous.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbol_path_round_trips() {
        let i = IntroId::from_raw([0xAB; 32]);
        let path = symbol_path(i);
        assert!(is_symbol_path(&path), "generated path must be recognized");
        let hex = intro_hex_of(&path).unwrap();
        assert_eq!(hex, i.to_hex());
        // A subdirectory path or wrong extension must NOT be recognized.
        assert!(!is_symbol_path("symbols/deadbeef"));
        assert!(!is_symbol_path("notahex.nir"));
        assert!(!is_symbol_path(&format!("dir/{}", path)));
    }
}
