//! Defines address behavior for `interface-identity`, whose purpose is to spell, parse, and abbreviate every identity a person or agent can name.
//! This module owns the address invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! The address: coordinate, path, and optional key in one line a human can read and a tool can resolve.

use core::fmt;

use crate::{
    ContentKey, CoordinateParseError, KeyParseError, PackageCoordinate, PathParseError,
    SymbolPath, key::KeyExactness,
};

/// Longest address spelling accepted on any input surface.
pub const MAX_ADDRESS_BYTES: usize = 4096;

/// A key attached to an address, together with how exactly it was spelled.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AddressKey {
    /// Parsed or rendered key.
    pub key: ContentKey,
    /// Whether the spelling pinned one exact instance.
    pub exactness: KeyExactness,
}

/// One resolvable, readable identity: `cargo:serde@1.0.196::de::Deserializer[trait]#<key>`.
///
/// The key is optional on input and always emitted on output when the producer proved one, so the
/// worst case of the readable scheme is exactly a bare key lookup.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Address {
    /// Package that owns the declaration.
    pub package: PackageCoordinate,
    /// Root-to-leaf declaration path; empty for the package page itself.
    pub path: SymbolPath,
    /// Exact key when known.
    pub key: Option<AddressKey>,
}

impl Address {
    /// Names the package page.
    #[must_use]
    pub fn package(package: PackageCoordinate) -> Self {
        Self {
            package,
            path: SymbolPath::default(),
            key: None,
        }
    }

    /// Names one declaration with a proven exact key.
    #[must_use]
    pub const fn exact(package: PackageCoordinate, path: SymbolPath, key: ContentKey) -> Self {
        Self {
            package,
            path,
            key: Some(AddressKey {
                key,
                exactness: KeyExactness::Exact,
            }),
        }
    }

    /// Parses one address. The coordinate ends at the first top-level `::`, the key begins at the
    /// last `#`, and everything between is the path.
    ///
    /// # Errors
    ///
    /// Returns the exact part that failed with its typed cause.
    pub fn parse(text: &str) -> Result<Self, AddressParseError> {
        if text.is_empty() {
            return Err(AddressParseError::Empty);
        }
        if text.len() > MAX_ADDRESS_BYTES {
            return Err(AddressParseError::TooLong {
                observed: text.len(),
                maximum: MAX_ADDRESS_BYTES,
            });
        }
        let (body, key) = match text.rsplit_once('#') {
            Some((body, key)) => {
                let (key, exactness) =
                    ContentKey::parse(key).map_err(|cause| AddressParseError::Key { cause })?;
                (body, Some(AddressKey { key, exactness }))
            }
            None => (text, None),
        };
        let (coordinate, path) = match body.find("::") {
            Some(cut) => (&body[..cut], &body[cut + 2..]),
            None => (body, ""),
        };
        let package = PackageCoordinate::parse(coordinate)
            .map_err(|cause| AddressParseError::Coordinate { cause })?;
        let path = SymbolPath::parse(path).map_err(|cause| AddressParseError::Path { cause })?;
        Ok(Self { package, path, key })
    }

    /// Returns the same address without its key, for comparisons that ignore instance identity.
    #[must_use]
    pub fn without_key(&self) -> Self {
        Self {
            package: self.package.clone(),
            path: self.path.clone(),
            key: None,
        }
    }
}

impl fmt::Display for Address {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.package)?;
        if !self.path.is_root() {
            write!(formatter, "::{}", self.path)?;
        }
        if let Some(AddressKey { key, exactness }) = self.key {
            match exactness {
                KeyExactness::Exact => write!(formatter, "#{key}")?,
                KeyExactness::FamilyOnly => write!(formatter, "#{}", key.family())?,
            }
        }
        Ok(())
    }
}

/// Exact address spelling rejection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AddressParseError {
    /// The input was empty.
    Empty,
    /// The spelling exceeds the fixed address budget.
    TooLong {
        /// Observed UTF-8 bytes.
        observed: usize,
        /// Accepted UTF-8 bytes.
        maximum: usize,
    },
    /// The coordinate prefix was rejected.
    Coordinate {
        /// Exact coordinate cause.
        cause: CoordinateParseError,
    },
    /// The path was rejected.
    Path {
        /// Exact path cause.
        cause: PathParseError,
    },
    /// The key suffix was rejected.
    Key {
        /// Exact key cause.
        cause: KeyParseError,
    },
}

#[cfg(test)]
mod tests {
    use compiler_ir_vocabulary::{DeclarationFamilyId, DeclarationIdentity, VariantFingerprint};

    use super::*;

    #[test]
    fn addresses_round_trip_with_and_without_keys() -> Result<(), AddressParseError> {
        let key = ContentKey::new(DeclarationIdentity {
            family: DeclarationFamilyId::from_raw([0x1a; 16]),
            variant: VariantFingerprint::from_raw([0x2b; 16]),
        });
        let text =
            format!("cargo:serde@1.0.196::de::Deserializer[trait]::deserialize_map[fn]#{key}");
        let address = Address::parse(&text)?;
        assert_eq!(address.to_string(), text);
        assert_eq!(address.path.segments().len(), 3);
        let bare = Address::parse("cargo:serde@1.0.196")?;
        assert!(bare.path.is_root());
        assert_eq!(bare.to_string(), "cargo:serde@1.0.196");
        assert!(matches!(
            Address::parse("serde::de"),
            Err(AddressParseError::Coordinate { .. })
        ));
        Ok(())
    }
}
