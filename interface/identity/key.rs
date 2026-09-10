//! Defines key behavior for `interface-identity`, whose purpose is to spell, parse, and abbreviate every identity a person or agent can name.
//! This module owns the key invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! The content key: the compiler's declaration identity rendered as a stable, copyable spelling.

use core::fmt;

use compiler_ir_vocabulary::{DeclarationFamilyId, DeclarationIdentity, VariantFingerprint};

use crate::hex::{HexParseError, KEY_HEX_BYTES, parse_lower_hex, write_lower_hex};

/// The one durable per-declaration identity, spelled `family[.variant]` in lower hex.
///
/// The family half survives overload instances and source moves; the variant half pins one exact
/// structural instance. Rendering both is exact; rendering only the family is the readable default.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ContentKey(DeclarationIdentity);

impl ContentKey {
    /// Wraps the compiler's exact declaration identity.
    #[must_use]
    pub const fn new(identity: DeclarationIdentity) -> Self {
        Self(identity)
    }

    /// Returns the exact compiler identity.
    #[must_use]
    pub const fn identity(self) -> DeclarationIdentity {
        self.0
    }

    /// Returns the eight-character abbreviation people glance at and tools never resolve by.
    #[must_use]
    pub const fn abbreviation(self) -> KeyAbbreviation {
        let bytes = self.0.family.as_bytes();
        KeyAbbreviation([bytes[0], bytes[1], bytes[2], bytes[3]])
    }

    /// Renders the family half only.
    #[must_use]
    pub const fn family(self) -> FamilyDisplay {
        FamilyDisplay(self.0.family)
    }

    /// Parses `family` or `family.variant`; a family-only key carries a zero variant fingerprint
    /// and is reported through [`KeyParseError`] when the caller requires an exact instance.
    ///
    /// # Errors
    ///
    /// Returns the exact half and hex cause that rejected the spelling.
    pub fn parse(text: &str) -> Result<(Self, KeyExactness), KeyParseError> {
        let (family_text, variant_text) = match text.split_once('.') {
            Some((family, variant)) => (family, Some(variant)),
            None => (text, None),
        };
        let family = parse_lower_hex::<KEY_HEX_BYTES>(family_text)
            .map_err(|cause| KeyParseError::Family { cause })?;
        let (variant, exactness) = match variant_text {
            Some(variant) => (
                parse_lower_hex::<KEY_HEX_BYTES>(variant)
                    .map_err(|cause| KeyParseError::Variant { cause })?,
                KeyExactness::Exact,
            ),
            None => ([0_u8; KEY_HEX_BYTES], KeyExactness::FamilyOnly),
        };
        Ok((
            Self(DeclarationIdentity {
                family: DeclarationFamilyId::from_raw(family),
                variant: VariantFingerprint::from_raw(variant),
            }),
            exactness,
        ))
    }
}

impl fmt::Display for ContentKey {
    /// Writes `family.variant` in lower hex without a prefix; the address grammar adds `#`.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_lower_hex(formatter, self.0.family.as_bytes())?;
        formatter.write_str(".")?;
        write_lower_hex(formatter, self.0.variant.as_bytes())
    }
}

/// Whether a parsed key named one exact instance or only its family.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeyExactness {
    /// Both halves were spelled.
    Exact,
    /// Only the family was spelled; resolution must disambiguate overload instances.
    FamilyOnly,
}

/// Family-only rendering of a key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FamilyDisplay(DeclarationFamilyId);

impl fmt::Display for FamilyDisplay {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_lower_hex(formatter, self.0.as_bytes())
    }
}

/// Eight lower-hex characters shown beside a symbol as a visual fingerprint.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct KeyAbbreviation([u8; 4]);

impl fmt::Display for KeyAbbreviation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_lower_hex(formatter, &self.0)
    }
}

/// Exact key spelling rejection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeyParseError {
    /// The family half was not thirty-two lower-hex characters.
    Family {
        /// Exact hex cause.
        cause: HexParseError,
    },
    /// The variant half was not thirty-two lower-hex characters.
    Variant {
        /// Exact hex cause.
        cause: HexParseError,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(seed: u8) -> DeclarationIdentity {
        DeclarationIdentity {
            family: DeclarationFamilyId::from_raw([seed; 16]),
            variant: VariantFingerprint::from_raw([seed.wrapping_add(1); 16]),
        }
    }

    #[test]
    fn key_round_trips_and_family_only_is_reported() -> Result<(), KeyParseError> {
        let key = ContentKey::new(identity(0xab));
        let text = key.to_string();
        assert_eq!(text.len(), 65);
        assert_eq!(ContentKey::parse(&text), Ok((key, KeyExactness::Exact)));
        let (family_only, exactness) = ContentKey::parse(&key.family().to_string())?;
        assert_eq!(exactness, KeyExactness::FamilyOnly);
        assert_eq!(family_only.identity().family, key.identity().family);
        assert_eq!(key.abbreviation().to_string(), "abababab");
        Ok(())
    }
}
