//! Serialization layer: IR ⇄ file-per-IntroId bytes.
//!
//! Each live symbol maps to a `symbols/{intro_hex}` file in the pijul working
//! tree. The file content is a postcard-serialized [`SymbolFile`].
//!
//! Links are stored **exactly once** on the endpoint with the smaller
//! [`IntroId`] (canonical owner, determined by byte comparison). Cross-package
//! links: the local endpoint always owns the entry.

use nudox_change::{IntroId, StableRef};
use nudox_ir::kind::KindDiscriminant;
use nudox_ir::wire::OwnedEntryPayload;
use serde::{Deserialize, Serialize};

use crate::error::VcsError;

// ---------------------------------------------------------------------------
// SymbolFile
// ---------------------------------------------------------------------------

/// A single `symbols/{intro_hex}` file content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolFile {
    /// The serialized payload for this symbol.
    pub payload: OwnedEntryPayload,
    /// Optional parent intro (for nested symbols like methods inside a class).
    pub parent: Option<IntroId>,
    /// Links that have this intro's canonical owner as the smaller endpoint.
    pub links: Vec<LinkWire>,
}

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
// Path helper
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

// ---------------------------------------------------------------------------
// Serialization
// ---------------------------------------------------------------------------

/// Serialize a [`SymbolFile`] to deterministic postcard bytes.
pub fn serialize_symbol_file(sf: &SymbolFile) -> Vec<u8> {
    postcard::to_allocvec(sf).expect("SymbolFile serialization is infallible for valid inputs")
}

/// Deserialize a [`SymbolFile`] from postcard bytes.
pub fn deserialize_symbol_file(bytes: &[u8]) -> Result<SymbolFile, VcsError> {
    postcard::from_bytes(bytes).map_err(VcsError::Serialize)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nudox_ir::wire::{EntryPayloadFlags, FunctionWire, KindWire, ModuleWire, ParamWire, SymbolWire, TypeRefWire};
    use nudox_ir::kind::KindDiscriminant;
    use nudox_change::{EcosystemId, PackageLineageId, PackageName};

    fn intro(n: u8) -> IntroId {
        IntroId::from_raw([n; 32])
    }

    fn sref(n: u8) -> StableRef {
        StableRef::new(PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("lib")), intro(n))
    }

    fn function_payload(name: &str) -> OwnedEntryPayload {
        let sym = SymbolWire {
            name: name.to_owned(),
            visibility: 1,
            documentation: Some("doc".to_owned()),
            source_path: "src/lib.rs".to_owned(),
            span_start: 3,
            span_end: 9,
            aliases: vec!["alias".to_owned()],
            deprecation: None,
            doc_links: Vec::new(),
        };
        OwnedEntryPayload::sealed(
            sym,
            KindDiscriminant::Function,
            KindWire::Function(FunctionWire {
                input_params: Box::new([ParamWire { name: Some("x".into()), ty: TypeRefWire::Same(intro(9)) }]),
                output_params: Box::new([]),
            }),
            EntryPayloadFlags::default(),
        )
    }

    #[test]
    fn symbol_path_round_trips() {
        let i = intro(0xAB);
        let path = symbol_path(i);
        assert!(is_symbol_path(&path), "generated path must be recognized");
        let hex = intro_hex_of(&path).unwrap();
        assert_eq!(hex, i.to_hex());
        // A subdirectory path or wrong extension must NOT be recognized.
        assert!(!is_symbol_path("symbols/deadbeef"));
        assert!(!is_symbol_path("notahex.nir"));
        assert!(!is_symbol_path(&format!("dir/{}", path)));
    }

    #[test]
    fn symbol_file_postcard_round_trips() {
        let sf = SymbolFile {
            payload: function_payload("do_thing"),
            parent: Some(intro(1)),
            links: vec![
                LinkWire { other: sref(2), kind_self: KindDiscriminant::Function, kind_other: KindDiscriminant::Module },
                LinkWire { other: sref(3), kind_self: KindDiscriminant::Function, kind_other: KindDiscriminant::Field },
            ],
        };
        let bytes = serialize_symbol_file(&sf);
        let back = deserialize_symbol_file(&bytes).expect("deserialize");
        assert_eq!(sf, back, "SymbolFile must survive a postcard round-trip");
    }

    #[test]
    fn serialization_is_deterministic() {
        let sf = SymbolFile {
            payload: OwnedEntryPayload::sealed(
                SymbolWire {
                    name: "m".into(),
                    visibility: 0,
                    documentation: None,
                    source_path: "src/lib.rs".into(),
                    span_start: 0,
                    span_end: 1,
                    aliases: Vec::new(),
                    deprecation: None,
                    doc_links: Vec::new(),
                },
                KindDiscriminant::Module,
                KindWire::Module(ModuleWire {}),
                EntryPayloadFlags::default(),
            ),
            parent: None,
            links: Vec::new(),
        };
        assert_eq!(serialize_symbol_file(&sf), serialize_symbol_file(&sf));
    }

    #[test]
    fn corrupt_bytes_error_not_panic() {
        assert!(deserialize_symbol_file(&[0xff, 0x00, 0x13, 0x37]).is_err());
    }
}
