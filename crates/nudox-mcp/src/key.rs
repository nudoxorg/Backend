//! `SymbolKeyDto` — the codec between agent-supplied string keys and wire
//! `SymbolKey` values (LR-1).
//!
//! # Why this module exists
//!
//! The wire `SymbolKey` (= `nudox_ir::change::StableRef`) is a structured type
//! that the engine and GUI use natively.  An agent, however, copies keys between
//! tools as opaque strings in `"ecosystem:name#introhex"` form.  `SymbolKeyDto`
//! is the **codec** between those two representations:
//!
//! * **Input** — agent supplies a string; `to_wire` decodes it into a `SymbolKey`
//!   that the engine methods accept.
//! * **Output** — `from_wire` renders a `SymbolKey` into the same canonical
//!   string so search results and `get_symbol` responses share the identical key
//!   spelling.  This is LR-1's "one key, spelled one way".
//!
//! This is a *codec*, not a mirror.  It does not replicate any field of the wire
//! vocabulary; it only bridges the agent's text world and the engine's typed
//! world at the boundary.
//!
//! # `nudox-ir` dependency
//!
//! `to_wire` constructs a `SymbolKey` from its parts.  `nudox_engine::wire`
//! re-exports `SymbolKey`, `IntroId`, and `PackageLineageId`, but not the two
//! newtypes — `EcosystemId` and `PackageName` — needed to *build* a
//! `PackageLineageId`.  Those are re-exported from `nudox_engine::wire` too,
//! so this module no longer needs a direct `nudox-ir` dependency.

use nudox_engine::wire::{EcosystemId, IntroId, PackageLineageId, PackageName, SymbolKey};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::McpError;

// ---------------------------------------------------------------------------
// SymbolKeyDto
// ---------------------------------------------------------------------------

/// The wire spelling of [`SymbolKey`] (LR-1), as `"ecosystem:name#introhex"`.
///
/// Example: `cargo:serde#3f1a…` (the intro half is 64 lowercase hex chars).
/// This is byte-identical to the `key` property in `schema.graphql`, so keys
/// move between `graph_query`, `search_symbols` and `get_symbol` unchanged.
///
/// **Agent input:** supply this string wherever a tool asks for a `key`.
/// **Agent output:** every result that carries a symbol reference includes this
/// string in its `key` field so you can pass it directly to `get_symbol` or
/// `find_usages`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct SymbolKeyDto(pub String);

impl SymbolKeyDto {
    /// Render a wire key into its canonical string form.
    pub fn from_wire(key: &SymbolKey) -> Self {
        Self(format!(
            "{}:{}#{}",
            key.package.ecosystem.as_str(),
            key.package.name.as_str(),
            key.intro.to_hex()
        ))
    }

    /// Parse the string form back into a wire key.
    ///
    /// Every failure is an [`McpError::MalformedKey`] carrying the input, so an
    /// agent that mangled a key sees what it actually sent.
    pub fn to_wire(&self) -> Result<SymbolKey, McpError> {
        let malformed = |reason: &'static str| McpError::MalformedKey {
            key: self.0.clone(),
            reason,
        };

        let (lineage, intro_hex) = self
            .0
            .split_once('#')
            .ok_or_else(|| malformed("expected 'ecosystem:name#introhex' — no '#' found"))?;
        let (ecosystem, name) = lineage
            .split_once(':')
            .ok_or_else(|| malformed("expected 'ecosystem:name' before '#' — no ':' found"))?;
        if ecosystem.is_empty() {
            return Err(malformed("ecosystem segment is empty"));
        }
        if name.is_empty() {
            return Err(malformed("package name segment is empty"));
        }
        let intro = parse_intro_hex(intro_hex)
            .ok_or_else(|| malformed("intro segment must be exactly 64 hex characters"))?;

        Ok(SymbolKey::new(
            PackageLineageId::new(EcosystemId::new(ecosystem), PackageName::new(name)),
            intro,
        ))
    }

    /// The `ecosystem:name` lineage prefix of this key (the part before `#`).
    ///
    /// Used by `search_symbols`'s `packages` filter.
    pub fn lineage(&self) -> Option<&str> {
        self.0.split_once('#').map(|(lineage, _)| lineage)
    }
}

// ---------------------------------------------------------------------------
// Hex parsing helpers
// ---------------------------------------------------------------------------

/// Decode a 64-character hex string into an `IntroId`.
///
/// Accepts either case on input.  Rejects any length other than 64 and any
/// non-hex byte, returning `None` rather than a partially-decoded id — a
/// truncated key must never resolve to a *different* symbol.
fn parse_intro_hex(s: &str) -> Option<IntroId> {
    if s.len() != 64 {
        return None;
    }
    let mut bytes = [0u8; 32];
    for (i, slot) in bytes.iter_mut().enumerate() {
        let hi = hex_nibble(s.as_bytes()[i * 2])?;
        let lo = hex_nibble(s.as_bytes()[i * 2 + 1])?;
        *slot = (hi << 4) | lo;
    }
    Some(IntroId::from_raw(bytes))
}

/// One hex character to its nibble value.
fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_key() -> String {
        format!("cargo:serde#{}", "ab".repeat(32))
    }

    #[test]
    fn symbol_key_round_trips_through_the_wire_type() {
        let dto = SymbolKeyDto(sample_key());
        let wire = dto.to_wire().expect("sample key parses");
        let back = SymbolKeyDto::from_wire(&wire);
        assert_eq!(dto, back, "wire -> string -> wire must be lossless");
    }

    #[test]
    fn symbol_key_rejects_every_malformed_shape() {
        let cases = [
            ("", "empty"),
            ("cargo:serde", "no '#'"),
            ("serde#abcd", "no ':'"),
            (":serde#abcd", "empty ecosystem"),
            ("cargo:#abcd", "empty name"),
            ("cargo:serde#", "empty intro"),
            ("cargo:serde#zz", "non-hex intro"),
        ];
        for (input, why) in cases {
            assert!(
                SymbolKeyDto(input.to_owned()).to_wire().is_err(),
                "should have rejected {input:?} ({why})"
            );
        }
        let short = format!("cargo:serde#{}", "a".repeat(63));
        assert!(
            SymbolKeyDto(short).to_wire().is_err(),
            "truncated intro must not resolve"
        );
    }

    #[test]
    fn lineage_is_the_prefix_before_the_hash() {
        let dto = SymbolKeyDto(sample_key());
        assert_eq!(dto.lineage(), Some("cargo:serde"));
    }
}
