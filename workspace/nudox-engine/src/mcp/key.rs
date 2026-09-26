//! `SymbolKeyDto` and `PackageLineageDto` — the codecs between agent-supplied
//! string keys and the wire `SymbolKey` / `PackageLineageId` values they
//! decode to (LR-1).
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
//! `to_wire` constructs a `SymbolKey` from its parts.  `crate::wire`
//! re-exports `SymbolKey`, `IntroId`, and `PackageLineageId`, but not the two
//! newtypes — `EcosystemId` and `PackageName` — needed to *build* a
//! `PackageLineageId`.  Those are re-exported from `crate::wire` too,
//! so this module no longer needs a direct `nudox-ir` dependency.

use crate::wire::{EcosystemId, IntroId, PackageLineageId, PackageName, SymbolKey};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::mcp::error::McpError;

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
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
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
        let loosened = loosen_symbol_key(&self.0);
        let malformed = |reason: &'static str| McpError::MalformedKey {
            key: self.0.clone(),
            reason,
        };

        let (lineage, intro_hex) = loosened
            .split_once('#')
            .ok_or_else(|| malformed("expected 'ecosystem:name#introhex' — no '#' found"))?;
        let (ecosystem, name) = split_lineage(lineage)
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
// PackageLineageDto
// ---------------------------------------------------------------------------

/// The wire spelling of a [`PackageLineageId`], as `"ecosystem:name"`.
///
/// This is the prefix half of [`SymbolKeyDto`] — the part before the `#` — and
/// exists as its own type for tools that name a *package* rather than a
/// symbol: `search_symbols`'s `packages` filter, `list_versions`, and
/// `select_version`. Same codec shape as `SymbolKeyDto` (LR-1: one id, one
/// spelling), one field shorter because a lineage has no `IntroId` half.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct PackageLineageDto(pub String);

impl PackageLineageDto {
    /// Render a wire lineage into its canonical string form.
    pub fn from_wire(id: &PackageLineageId) -> Self {
        Self(format!("{}:{}", id.ecosystem.as_str(), id.name.as_str()))
    }

    /// Parse the string form back into a wire lineage.
    ///
    /// Every failure is an [`McpError::MalformedPackage`] carrying the input,
    /// so an agent that mangled a lineage sees what it actually sent.
    pub fn to_wire(&self) -> Result<PackageLineageId, McpError> {
        let malformed = |reason: &'static str| McpError::MalformedPackage {
            package: self.0.clone(),
            reason,
        };

        let loosened = loosen_lineage(&self.0);
        let (ecosystem, name) = split_lineage(&loosened)
            .ok_or_else(|| malformed("expected 'ecosystem:name' — no ':' found"))?;
        if ecosystem.is_empty() {
            return Err(malformed("ecosystem segment is empty"));
        }
        if name.is_empty() {
            return Err(malformed("package name segment is empty"));
        }

        Ok(PackageLineageId::new(
            EcosystemId::new(ecosystem),
            PackageName::new(name),
        ))
    }
}

// ---------------------------------------------------------------------------
// Hex parsing helpers
// ---------------------------------------------------------------------------

/// Drop the paste noise agents actually send: wrapping quotes or backticks,
/// whitespace around the separators, a Rust-style `::`, a `0x` on the intro,
/// and a colon used where `#` belongs when the last segment is 64 hex digits.
///
/// The canonical spelling (`from_wire`) does not change. A key that is
/// genuinely the wrong shape still fails, with the original input echoed.
fn loosen_symbol_key(raw: &str) -> String {
    let compact = strip_wrapping(raw);
    let mut out = String::with_capacity(compact.len());
    for ch in compact.chars() {
        if !ch.is_whitespace() {
            out.push(ch);
        }
    }
    if let Some(rest) = out.strip_prefix("0x") {
        if rest.len() == 64 && rest.chars().all(|c| c.is_ascii_hexdigit()) {
            return out;
        }
    }
    let hash_at = out.rfind('#').or_else(|| {
        let colon = out.rfind(':')?;
        let tail = &out[colon + 1..];
        let tail = tail.strip_prefix("0x").unwrap_or(tail);
        (tail.len() == 64 && tail.chars().all(|c| c.is_ascii_hexdigit())).then_some(colon)
    });
    if let Some(idx) = hash_at {
        let (lineage, intro) = out.split_at(idx);
        let intro = intro.trim_start_matches(['#', ':']);
        let intro = intro.strip_prefix("0x").unwrap_or(intro);
        let lineage = lineage.replacen("::", ":", 1);
        return format!("{lineage}#{intro}");
    }
    out.replacen("::", ":", 1)
}

fn loosen_lineage(raw: &str) -> String {
    let compact = strip_wrapping(raw);
    let mut out = String::with_capacity(compact.len());
    for ch in compact.chars() {
        if !ch.is_whitespace() {
            out.push(ch);
        }
    }
    out.replacen("::", ":", 1)
}

fn strip_wrapping(raw: &str) -> &str {
    raw.trim().trim_matches(|c| matches!(c, '`' | '"' | '\''))
}

/// Split `ecosystem:name` on the first colon. The name may itself contain
/// colons only after we have already peeled a mistaken intro separator.
fn split_lineage(lineage: &str) -> Option<(&str, &str)> {
    lineage.split_once(':')
}

/// Decode a 64-character hex string into an `IntroId`.
///
/// Accepts either case on input.  Rejects any length other than 64 and any
/// non-hex byte, returning `None` rather than a partially-decoded id — a
/// truncated key must never resolve to a *different* symbol.
///
/// `pub(crate)` rather than private: `mcp::address`'s parser needs the exact
/// same decode this module already has for the legacy `#introhex` key half —
/// duplicating it would risk the two silently drifting on what counts as a
/// valid key.
pub(crate) fn parse_intro_hex(s: &str) -> Option<IntroId> {
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
    fn slightly_malformed_keys_parse_as_the_canonical_key() {
        let canonical = sample_key();
        let wire = SymbolKeyDto(canonical.clone())
            .to_wire()
            .expect("canonical key");
        let hex = "ab".repeat(32);
        let inputs = [
            format!("  cargo:serde#{hex}  "),
            format!("`cargo:serde#{hex}`"),
            format!("\"cargo:serde#{hex}\""),
            format!("cargo::serde#{hex}"),
            format!("cargo:serde:{hex}"),
            format!("cargo:serde#0x{hex}"),
            format!("cargo : serde # {hex}"),
        ];
        for input in inputs {
            let parsed = SymbolKeyDto(input.clone())
                .to_wire()
                .unwrap_or_else(|err| panic!("{input:?} should parse, got {err}"));
            assert_eq!(parsed, wire, "{input:?}");
            assert_eq!(
                SymbolKeyDto::from_wire(&parsed).0,
                canonical,
                "output spelling stays canonical"
            );
        }
        let lineage = PackageLineageDto(" `cargo::serde` ".into())
            .to_wire()
            .expect("lineage paste");
        assert_eq!(lineage.ecosystem.as_str(), "cargo");
        assert_eq!(lineage.name.as_str(), "serde");
    }

    #[test]
    fn symbol_key_rejects_every_malformed_shape() {
        // Doctrine §4: an assertion that some error came back is a tautology
        // a stub would also pass. Each case pins the *content* of `reason` —
        // the exact text an agent would read to self-correct — not merely
        // that `to_wire` returned `Err`.
        let cases = [
            ("", "no '#' found"),
            ("cargo:serde", "no '#' found"),
            ("serde#abcd", "no ':' found"),
            (":serde#abcd", "ecosystem segment is empty"),
            ("cargo:#abcd", "package name segment is empty"),
            (
                "cargo:serde#",
                "intro segment must be exactly 64 hex characters",
            ),
            (
                "cargo:serde#zz",
                "intro segment must be exactly 64 hex characters",
            ),
        ];
        for (input, expected_reason_substring) in cases {
            let err = SymbolKeyDto(input.to_owned())
                .to_wire()
                .expect_err(&format!("should have rejected {input:?}"));
            match err {
                McpError::MalformedKey { key, reason } => {
                    assert_eq!(
                        key, input,
                        "the rejected input must be echoed back verbatim"
                    );
                    assert!(
                        reason.contains(expected_reason_substring),
                        "input {input:?}: expected reason to mention {expected_reason_substring:?}, \
                         got {reason:?}"
                    );
                }
                other => panic!("expected MalformedKey for {input:?}, got {other:?}"),
            }
        }
        let short = format!("cargo:serde#{}", "a".repeat(63));
        match SymbolKeyDto(short).to_wire() {
            Err(McpError::MalformedKey { reason, .. }) => {
                assert!(
                    reason.contains("64 hex characters"),
                    "a truncated intro must be rejected as the wrong length, not silently \
                     accepted: got {reason:?}"
                );
            }
            other => panic!("truncated intro must not resolve, got {other:?}"),
        }
    }

    #[test]
    fn lineage_is_the_prefix_before_the_hash() {
        let dto = SymbolKeyDto(sample_key());
        assert_eq!(dto.lineage(), Some("cargo:serde"));
    }

    // -----------------------------------------------------------------------
    // PackageLineageDto
    // -----------------------------------------------------------------------

    #[test]
    fn package_lineage_round_trips_through_the_wire_type() {
        let dto = PackageLineageDto("cargo:serde".to_owned());
        let wire = dto.to_wire().expect("sample lineage parses");
        let back = PackageLineageDto::from_wire(&wire);
        assert_eq!(dto, back, "wire -> string -> wire must be lossless");
    }

    #[test]
    fn package_lineage_rejects_every_malformed_shape() {
        let cases = [
            ("", "no ':' found"),
            ("cargo", "no ':' found"),
            (":serde", "ecosystem segment is empty"),
            ("cargo:", "package name segment is empty"),
        ];
        for (input, expected_reason_substring) in cases {
            let err = PackageLineageDto(input.to_owned())
                .to_wire()
                .expect_err(&format!("should have rejected {input:?}"));
            match err {
                McpError::MalformedPackage { package, reason } => {
                    assert_eq!(
                        package, input,
                        "the rejected input must be echoed back verbatim"
                    );
                    assert!(
                        reason.contains(expected_reason_substring),
                        "input {input:?}: expected reason to mention {expected_reason_substring:?}, \
                         got {reason:?}"
                    );
                }
                other => panic!("expected MalformedPackage for {input:?}, got {other:?}"),
            }
        }
    }
}
